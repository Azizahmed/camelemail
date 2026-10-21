//! `remail-server`: the API server and its host-side maintenance commands.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use remail_proto::org::{OrgConfig, OrgSecrets};
use remail_server::crypto::backup;
use remail_server::crypto::keys::{write_private, KeyRing};
use remail_server::crypto::{Envelope, FileKeyProvider, KeyFile, KeyProvider, MemoryKeyProvider};
use remail_server::{app, auth, keyops, licence, mailer, orgs, AppState, Config, Mode};
use sqlx::PgPool;
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(name = "remail-server", version, about = "Remail server")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the API (migrates first).
    Serve,
    /// Apply database migrations (hosted: also creates the public org).
    Migrate,
    /// Master key file maintenance.
    #[command(subcommand)]
    Key(KeyCmd),
    /// Organisations.
    #[command(subcommand)]
    Org(OrgCmd),
    /// Users.
    #[command(subcommand)]
    User(UserCmd),
    /// On-prem licence.
    #[command(subcommand)]
    Licence(LicenceCmd),
}

#[derive(Subcommand)]
enum KeyCmd {
    /// Create the key file (REMAIL_MASTER_KEY_FILE) with a new deployment id.
    Init,
    /// Add a new master key, make it active and re-wrap every data key.
    Rotate,
    /// Write the key file sealed with a passphrase (REMAIL_BACKUP_PASSPHRASE or prompt).
    Backup {
        #[arg(long)]
        out: PathBuf,
    },
    /// Recreate the key file from a backup.
    Restore {
        #[arg(long)]
        from: PathBuf,
        /// Where to write the key file (default REMAIL_MASTER_KEY_FILE).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Check that every data key unwraps with the key file.
    Verify,
    /// Remove an old master key once nothing references it.
    Prune {
        #[arg(long)]
        version: i32,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum OrgCmd {
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        slug: String,
        /// open | invite
        #[arg(long, default_value = "invite")]
        signup: String,
    },
    /// Org config (providers, policies, branding) from a JSON file.
    #[command(subcommand)]
    Config(SetFile),
    /// Org secrets (AI key, Google client secret) from a JSON file.
    #[command(subcommand)]
    Secrets(SetFile),
}

#[derive(Subcommand)]
enum SetFile {
    Set {
        #[arg(long)]
        org: String,
        #[arg(long)]
        file: PathBuf,
    },
}

#[derive(Subcommand)]
enum UserCmd {
    /// Print (and e-mail, with SMTP) an invite link.
    Invite {
        #[arg(long)]
        org: String,
        #[arg(long)]
        email: String,
        /// admin | user
        #[arg(long, default_value = "user")]
        role: String,
    },
}

#[derive(Subcommand)]
enum LicenceCmd {
    Install {
        #[arg(long)]
        file: PathBuf,
    },
}

fn key_path() -> PathBuf {
    std::env::var("REMAIL_MASTER_KEY_FILE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/remail/master.key"))
}

/// The master key: from the `REMAIL_MASTER_KEY` secret when it's set (hosting
/// platforms without a persistent disk), else from the key file.
fn open_keys(cfg: &Config) -> Result<Arc<dyn KeyProvider>> {
    if let Some(secret) = std::env::var("REMAIL_MASTER_KEY").ok().filter(|s| !s.trim().is_empty()) {
        let secret = Zeroizing::new(secret);
        let ring = KeyFile::parse_secret(&secret)?;
        return Ok(Arc::new(MemoryKeyProvider::from_ring(ring)));
    }
    let keys = FileKeyProvider::open(&cfg.master_key_file)
        .with_context(|| "can't load the master key file; run `remail-server key init` first, or set REMAIL_MASTER_KEY")?;
    Ok(Arc::new(keys))
}

struct Ctx {
    cfg: Config,
    db: PgPool,
    env: Envelope,
}

async fn ctx() -> Result<Ctx> {
    let cfg = Config::from_env()?;
    let keys = open_keys(&cfg)?;
    let db = remail_server::db::connect(&cfg.database_url, 5).await?;
    remail_server::db::migrate(&db).await?;
    keyops::check_keys(&db, keys.as_ref()).await?;
    let env = Envelope::new(keys);
    env.register_master_keys(&db).await?;
    Ok(Ctx { cfg, db, env })
}

async fn org_by_slug(c: &Ctx, slug: &str) -> Result<orgs::Org> {
    orgs::by_slug(&c.db, slug).await?.with_context(|| format!("no org with code {slug:?}"))
}

fn passphrase(confirm: bool) -> Result<Zeroizing<String>> {
    if let Ok(p) = std::env::var("REMAIL_BACKUP_PASSPHRASE") {
        return Ok(Zeroizing::new(p));
    }
    let p = Zeroizing::new(rpassword::prompt_password("Backup passphrase: ")?);
    if confirm {
        let again = Zeroizing::new(rpassword::prompt_password("Again: ")?);
        if *p != *again {
            bail!("the passphrases don't match");
        }
    }
    Ok(p)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let raw = Zeroizing::new(std::fs::read(path).with_context(|| format!("can't read {}", path.display()))?);
    serde_json::from_slice(&raw).with_context(|| format!("{} is not valid JSON for this setting", path.display()))
}

async fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Serve => {
            let cfg = Config::from_env()?;
            let keys = open_keys(&cfg)?;
            let db = remail_server::db::connect(&cfg.database_url, 20).await?;
            remail_server::db::migrate(&db).await?;
            keyops::check_keys(&db, keys.as_ref()).await?;
            let bind = cfg.bind;
            let state = AppState::new(cfg, db.clone(), keys).await?;
            if let Some(url) = orgs::startup(&state.cfg, &db, &state.env, true).await? {
                // Printed, not logged: shown once on the host console.
                let who = if state.cfg.mode == Mode::Hosted { "owner" } else { "administrator" };
                println!("\nNo {who} yet: create the {who} account with this one-time invite (7 days):\n\n  {url}\n");
            }
            let lic = licence::status(&state.cfg, &db, &state.env).await?;
            if let Some(p) = lic.problem.filter(|_| state.cfg.mode == Mode::Onprem) {
                tracing::warn!(problem = %p, "licence");
            }
            if state.cfg.smtp_url.is_none() {
                tracing::warn!("REMAIL_SMTP_URL is not set: verification, reset and invite e-mails are not sent");
            }
            app::spawn_housekeeping(db.clone());
            let listener = tokio::net::TcpListener::bind(bind).await.with_context(|| format!("can't listen on {bind}"))?;
            tracing::info!(%bind, mode = state.cfg.mode.as_str(), "remail-server listening");
            app::serve(state, listener, shutdown_signal()).await
        }
        Cmd::Migrate => {
            let c = ctx().await?;
            orgs::startup(&c.cfg, &c.db, &c.env, false).await?;
            println!("Database is up to date.");
            Ok(())
        }
        Cmd::Key(k) => key_cmd(k).await,
        Cmd::Org(OrgCmd::Create { name, slug, signup }) => {
            let c = ctx().await?;
            let org = orgs::create(&c.db, &c.env, &name, &slug, &signup).await?;
            licence::ensure_deployment(&c.cfg, &c.db, &c.env).await?;
            println!("Created org {} ({}), code {}", org.name, org.id, org.slug);
            Ok(())
        }
        Cmd::Org(OrgCmd::Config(SetFile::Set { org, file })) => {
            let c = ctx().await?;
            let org = org_by_slug(&c, &org).await?;
            let cfg: OrgConfig = read_json(&file)?;
            let v = orgs::set_config(&c.db, &c.env, &org.id, &cfg, "cli").await?;
            println!("Org config saved (version {v}).");
            Ok(())
        }
        Cmd::Org(OrgCmd::Secrets(SetFile::Set { org, file })) => {
            let c = ctx().await?;
            let org = org_by_slug(&c, &org).await?;
            let secrets: OrgSecrets = read_json(&file)?;
            let v = orgs::set_secrets(&c.db, &c.env, &org.id, &secrets).await?;
            println!("Org secrets saved (version {v}).");
            Ok(())
        }
        Cmd::User(UserCmd::Invite { org, email, role }) => {
            let c = ctx().await?;
            if !matches!(role.as_str(), "admin" | "user") {
                bail!("role must be admin or user");
            }
            let org = org_by_slug(&c, &org).await?;
            if let Err(e) = licence::check_seat(&c.cfg, &c.db, &c.env).await {
                bail!("{}", match e {
                    remail_server::AppError::Api(a) => a.message,
                    remail_server::AppError::Internal(i) => i.to_string(),
                });
            }
            let token = auth::tokens::create_invite(&c.db, &org.id, Some(&email), &role).await?;
            let url = orgs::invite_url(&c.cfg, token.expose());
            let m = mailer::Mailer::new(&c.cfg)?;
            if c.cfg.smtp_url.is_some() {
                m.send_and_wait(mailer::invite_mail(&c.cfg.public_url, &org.name, &auth::normalize_email(&email), token.expose())).await;
            }
            println!("Invite for {email} ({role}), valid 7 days:\n\n  {url}\n");
            Ok(())
        }
        Cmd::Licence(LicenceCmd::Install { file }) => {
            let c = ctx().await?;
            let raw = std::fs::read_to_string(&file).with_context(|| format!("can't read {}", file.display()))?;
            let (lic, state) = licence::install(&c.cfg, &c.db, &c.env, &raw).await?;
            println!(
                "Licence {} for {} installed: {} seats, until {} ({state:?}).",
                lic.lid, lic.customer, lic.seats, lic.expires
            );
            Ok(())
        }
    }
}

async fn key_cmd(k: KeyCmd) -> Result<()> {
    let path = key_path();
    match k {
        KeyCmd::Init => {
            if path.exists() {
                bail!("{} already exists; refusing to overwrite a master key", path.display());
            }
            let ring = KeyRing::generate(remail_proto::new_id("dep"));
            KeyFile::save(&path, &ring)?;
            println!("Key file written to {} (mode 0600).", path.display());
            println!("Deployment id: {}", ring.deployment);
            println!("Master key mk1 fingerprint: {}", ring.fingerprint(1).unwrap_or_default());
            println!("\nBack it up now: remail-server key backup --out <file>");
            println!("Losing both the key file and its backup loses every user's settings and credentials.");
            Ok(())
        }
        KeyCmd::Rotate => {
            let mut ring = KeyFile::load(&path)?;
            let from = ring.active;
            let v = ring.rotate();
            KeyFile::save(&path, &ring)?;
            println!("Added mk{v} (fingerprint {}); it is now active.", ring.fingerprint(v).unwrap_or_default());
            let c = ctx().await?;
            let n = keyops::rewrap_all(&c.db, c.env.provider().as_ref(), |done| println!("  re-wrapped {done} data keys…")).await?;
            println!("Done: {n} data keys moved from mk{from} and older to mk{v}. Data was not re-encrypted.");
            println!("\nBack up the key file now (key backup), then remove the old key with: key prune --version {from}");
            Ok(())
        }
        KeyCmd::Backup { out } => {
            let ring = KeyFile::load(&path)?;
            let pass = passphrase(true)?;
            let sealed = backup::seal_backup(&ring, &pass)?;
            write_private(&out, &sealed).with_context(|| format!("can't write {}", out.display()))?;
            println!("Sealed backup of {} written to {}.", path.display(), out.display());
            println!("Keep it apart from database backups, together with its passphrase.");
            Ok(())
        }
        KeyCmd::Restore { from, out } => {
            let out = out.unwrap_or(path);
            if out.exists() {
                bail!("{} already exists; refusing to overwrite", out.display());
            }
            let data = std::fs::read(&from).with_context(|| format!("can't read {}", from.display()))?;
            let pass = passphrase(false)?;
            let ring = backup::open_backup(&data, &pass).context("wrong passphrase or not a Remail key backup")?;
            KeyFile::save(&out, &ring)?;
            println!("Key file restored to {} (deployment {}, active mk{}).", out.display(), ring.deployment, ring.active);
            Ok(())
        }
        KeyCmd::Verify => {
            let c = ctx().await?;
            let keys = c.env.provider();
            let r = keyops::verify(&c.db, keys.as_ref()).await?;
            println!("Deployment {}, active mk{}.", keys.deployment(), keys.active_version());
            for (v, n) in &r.by_version {
                println!("  mk{v}: {n} data keys");
            }
            for v in &r.fingerprint_mismatch {
                println!("  mk{v}: fingerprint differs from the database record");
            }
            for f in &r.failed {
                println!("  can't unwrap: {f}");
            }
            if !r.ok() {
                bail!("{} of {} data keys failed", r.failed.len(), r.total);
            }
            println!("All {} data keys unwrap.", r.total);
            Ok(())
        }
        KeyCmd::Prune { version, yes } => {
            let c = ctx().await?;
            keyops::check_prunable(&c.db, c.env.provider().as_ref(), version).await?;
            if !yes {
                println!("Make sure the current key file is backed up (key backup) before removing mk{version}.");
                let typed = rpassword::prompt_password(format!("Type {version} to remove mk{version}: "))?;
                if typed.trim() != version.to_string() {
                    bail!("not confirmed");
                }
            }
            let mut ring = KeyFile::load(&path)?;
            ring.remove(version)?;
            KeyFile::save(&path, &ring)?;
            keyops::forget_version(&c.db, version).await?;
            println!("Removed mk{version}.");
            Ok(())
        }
    }
}

/// Ctrl-C, or SIGTERM from `docker stop` / systemd.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,sqlx=warn,tower_http=info")),
        )
        // Plain text when logs go to a platform's log viewer, colours in a terminal.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();
    if let Err(e) = run(Cli::parse()).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
