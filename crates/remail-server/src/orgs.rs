//! Organisations: rows, their encrypted config and secrets, and the startup
//! steps that make sure one exists (hosted: the public org; on-prem: the
//! customer's org plus a one-time admin invite).

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use remail_proto::org::{OrgConfig, OrgSecrets};
use sqlx::{FromRow, PgPool};
use zeroize::Zeroizing;

use crate::config::{Config, Mode};
use crate::crypto::{open, seal, section_aad, Envelope, OwnerKind};
use crate::notices::{self, HubEvent};

/// Hosted: the org anyone can sign up to.
pub const PUBLIC_SLUG: &str = "public";

const CONFIG_SECTION: &str = "org_config";
const SECRETS_SECTION: &str = "org_secrets";

#[derive(Debug, Clone, FromRow)]
pub struct Org {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub signup_mode: String,
    pub allowed_domains: Vec<String>,
    pub server_url: Option<String>,
    pub disabled_at: Option<DateTime<Utc>>,
}

macro_rules! org_cols {
    () => {
        "id, name, slug, signup_mode, allowed_domains, server_url, disabled_at"
    };
}

impl Org {
    /// May an address on this domain join?
    pub fn allows_email(&self, email: &str) -> bool {
        if self.allowed_domains.is_empty() {
            return true;
        }
        let domain = email.rsplit('@').next().unwrap_or("");
        self.allowed_domains.iter().any(|d| d.trim_start_matches('@').eq_ignore_ascii_case(domain))
    }
}

pub fn valid_slug(slug: &str) -> bool {
    (2..=40).contains(&slug.len())
        && slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !slug.starts_with('-')
        && !slug.ends_with('-')
}

pub async fn by_id(db: &PgPool, id: &str) -> sqlx::Result<Option<Org>> {
    sqlx::query_as(concat!("select ", org_cols!(), " from orgs where id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn by_slug(db: &PgPool, slug: &str) -> sqlx::Result<Option<Org>> {
    sqlx::query_as(concat!("select ", org_cols!(), " from orgs where slug = $1"))
        .bind(slug)
        .fetch_optional(db)
        .await
}

/// The oldest org: on-prem's one organisation.
pub async fn first(db: &PgPool) -> sqlx::Result<Option<Org>> {
    sqlx::query_as(concat!("select ", org_cols!(), " from orgs order by created_at, id limit 1"))
        .fetch_optional(db)
        .await
}

/// Creates an org and its data key.
pub async fn create(db: &PgPool, env: &Envelope, name: &str, slug: &str, signup_mode: &str) -> Result<Org> {
    if !valid_slug(slug) {
        bail!("the org code must be 2–40 lower-case letters, digits or dashes");
    }
    if !matches!(signup_mode, "open" | "invite") {
        bail!("sign-up mode must be open or invite");
    }
    let name = name.trim();
    if name.is_empty() {
        bail!("the org needs a name");
    }
    let id = remail_proto::new_id("org");
    let mut tx = db.begin().await?;
    sqlx::query("insert into orgs (id, name, slug, signup_mode) values ($1, $2, $3, $4)")
        .bind(&id)
        .bind(name)
        .bind(slug)
        .bind(signup_mode)
        .execute(&mut *tx)
        .await
        .with_context(|| format!("can't create org {slug:?} (is the code taken?)"))?;
    env.create_key(&mut tx, OwnerKind::Org, &id, &id).await?;
    tx.commit().await?;
    by_id(db, &id).await?.context("org vanished after insert")
}

/// The two encrypted org blobs. Table names are fixed SQL, never input.
#[derive(Clone, Copy)]
enum Blob {
    Config,
    Secrets,
}

impl Blob {
    /// Name used in the AAD.
    fn section(self) -> &'static str {
        match self {
            Blob::Config => CONFIG_SECTION,
            Blob::Secrets => SECRETS_SECTION,
        }
    }
    fn select(self) -> &'static str {
        match self {
            Blob::Config => "select version, ciphertext from org_config where org_id = $1",
            Blob::Secrets => "select version, ciphertext from org_secrets where org_id = $1",
        }
    }
    fn select_version(self) -> &'static str {
        match self {
            Blob::Config => "select version from org_config where org_id = $1",
            Blob::Secrets => "select version from org_secrets where org_id = $1",
        }
    }
    fn upsert(self) -> &'static str {
        match self {
            Blob::Config => {
                "insert into org_config (org_id, version, ciphertext, updated_at) values ($1, $2, $3, now())
                 on conflict (org_id) do update set version = excluded.version, ciphertext = excluded.ciphertext, updated_at = now()"
            }
            Blob::Secrets => {
                "insert into org_secrets (org_id, version, ciphertext, updated_at) values ($1, $2, $3, now())
                 on conflict (org_id) do update set version = excluded.version, ciphertext = excluded.ciphertext, updated_at = now()"
            }
        }
    }
}

async fn load_blob(db: &PgPool, env: &Envelope, blob: Blob, org_id: &str) -> Result<Option<(i64, Zeroizing<Vec<u8>>)>> {
    let row: Option<(i64, Vec<u8>)> = sqlx::query_as(blob.select()).bind(org_id).fetch_optional(db).await?;
    let Some((v, ct)) = row else { return Ok(None) };
    let dk = env.org_key(db, org_id).await?;
    Ok(Some((v, open(dk.bytes(), &section_aad(org_id, blob.section(), v), &ct)?)))
}

/// The org's public config and its version (0 + defaults if never set).
pub async fn config(db: &PgPool, env: &Envelope, org: &Org) -> Result<(i64, OrgConfig)> {
    let mut cfg = match load_blob(db, env, Blob::Config, &org.id).await? {
        None => (0, OrgConfig::default()),
        Some((v, plain)) => (v, serde_json::from_slice(&plain).context("org config is not valid JSON")?),
    };
    // The org row is the source of truth for the name.
    cfg.1.name = org.name.clone();
    Ok(cfg)
}

/// Server-only secrets (AI key, OAuth client secrets).
pub async fn secrets(db: &PgPool, env: &Envelope, org_id: &str) -> Result<OrgSecrets> {
    match load_blob(db, env, Blob::Secrets, org_id).await? {
        None => Ok(OrgSecrets::default()),
        Some((_, plain)) => serde_json::from_slice(&plain).context("org secrets are not valid JSON"),
    }
}

async fn store_blob(db: &PgPool, env: &Envelope, blob: Blob, org_id: &str, plain: &[u8]) -> Result<i64> {
    let dk = env.org_key(db, org_id).await?;
    let mut tx = db.begin().await?;
    // Serialise writers on the org row so versions never collide.
    sqlx::query("select 1 from orgs where id = $1 for update").bind(org_id).execute(&mut *tx).await?;
    let current: Option<i64> = sqlx::query_scalar(blob.select_version())
        .bind(org_id)
        .fetch_optional(&mut *tx)
        .await?;
    let version = current.unwrap_or(0) + 1;
    let ct = seal(dk.bytes(), &section_aad(org_id, blob.section(), version), plain);
    sqlx::query(blob.upsert())
    .bind(org_id)
    .bind(version)
    .bind(ct)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(version)
}

/// Saves the org config, bumps its version and tells the org's devices.
pub async fn set_config(db: &PgPool, env: &Envelope, org_id: &str, cfg: &OrgConfig, instance: &str) -> Result<i64> {
    let json = serde_json::to_vec(cfg)?;
    let version = store_blob(db, env, Blob::Config, org_id, &json).await?;
    notices::notify(db, instance, HubEvent::Org { org: org_id.into(), version }).await;
    Ok(version)
}

pub async fn set_secrets(db: &PgPool, env: &Envelope, org_id: &str, secrets: &OrgSecrets) -> Result<i64> {
    let json = Zeroizing::new(serde_json::to_vec(secrets)?);
    store_blob(db, env, Blob::Secrets, org_id, &json).await
}

/// Startup steps shared by `migrate` and `serve`: schema, master-key rows,
/// and the org every mode needs. Returns a one-time admin invite URL when an
/// on-prem server starts for the first time.
pub async fn startup(cfg: &Config, db: &PgPool, env: &Envelope, create_first_org: bool) -> Result<Option<String>> {
    crate::db::migrate(db).await?;
    env.register_master_keys(db).await?;
    let mut invite = None;
    match cfg.mode {
        Mode::Hosted => {
            let public = match by_slug(db, PUBLIC_SLUG).await? {
                Some(org) => org,
                None => {
                    let org = create(db, env, "Remail", PUBLIC_SLUG, "open").await?;
                    tracing::info!("created the public org");
                    org
                }
            };
            apply_google_app(cfg, db, env, &public).await?;
            // Until the service has an owner, every start offers a fresh
            // one-time owner invite (an invite needs no e-mail delivery).
            if create_first_org {
                let owners: i64 = sqlx::query_scalar("select count(*) from users where role = 'owner'").fetch_one(db).await?;
                if owners == 0 {
                    let token = crate::auth::tokens::create_invite(db, &public.id, None, "owner").await?;
                    invite = Some(invite_url(cfg, token.expose()));
                }
            }
        }
        Mode::Onprem if create_first_org => {
            if first(db).await?.is_none() {
                let org = create(db, env, &cfg.first_org_name, "org", "invite").await?;
                let token = crate::auth::tokens::create_invite(db, &org.id, None, "admin").await?;
                invite = Some(invite_url(cfg, token.expose()));
            }
        }
        Mode::Onprem => {}
    }
    crate::licence::ensure_deployment(cfg, db, env).await?;
    Ok(invite)
}

/// Hosted: the Google app from `REMAIL_GOOGLE_CLIENT_ID` and
/// `REMAIL_GOOGLE_CLIENT_SECRET` goes into the public org (saved only when
/// it changed). Both are needed: Google's token endpoint wants the secret.
async fn apply_google_app(cfg: &Config, db: &PgPool, env: &Envelope, org: &Org) -> Result<()> {
    let (Some(id), Some(secret)) = (&cfg.google_client_id, &cfg.google_client_secret) else {
        if cfg.google_client_id.is_some() != cfg.google_client_secret.is_some() {
            tracing::warn!("Google sign-in needs both REMAIL_GOOGLE_CLIENT_ID and REMAIL_GOOGLE_CLIENT_SECRET; ignoring the one set");
        }
        return Ok(());
    };
    let (_, mut config) = config(db, env, org).await?;
    if config.providers.google_app.client_id != *id || !config.providers.google {
        config.providers.google_app.client_id = id.clone();
        config.providers.google = true;
        set_config(db, env, &org.id, &config, "startup").await?;
        tracing::info!("Google sign-in set up for the public org");
    }
    let mut secrets = secrets(db, env, &org.id).await?;
    if secrets.google_client_secret != *secret {
        secrets.google_client_secret = secret.clone();
        set_secrets(db, env, &org.id, &secrets).await?;
    }
    Ok(())
}

pub fn invite_url(cfg: &Config, token: &str) -> String {
    format!("{}/invite?t={token}", cfg.public_url)
}
