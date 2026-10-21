//! The server in-process on `127.0.0.1:0` with a fresh database, for other
//! crates' end-to-end tests (feature `testing`; never in a release build).
//!
//! `start` creates `test_<uuid>` next to the database `admin_url` names,
//! migrates it, keeps the master key in memory, and serves until `finish`,
//! which drops the database again. Mail goes to the dev outbox
//! (`GET /v1/dev/mail`); argon2 and rate limits are cheap.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::PgPool;

use crate::config::{Argon2Cost, RateConfig};
use crate::crypto::MemoryKeyProvider;
use crate::{AppState, Config, Mode};

pub struct TestServer {
    /// `http://127.0.0.1:<port>`
    pub url: String,
    pub db: PgPool,
    pub state: AppState,
    /// On-prem: the one-time admin invite printed on first start.
    pub first_invite: Option<String>,
    admin: PgPool,
    db_name: String,
    task: tokio::task::JoinHandle<()>,
}

/// `REMAIL_TEST_DATABASE_URL`, if set (see `scripts/test-postgres.sh`).
pub fn database_url() -> Option<String> {
    std::env::var("REMAIL_TEST_DATABASE_URL").ok().filter(|u| !u.trim().is_empty())
}

fn with_db(url: &str, db: &str) -> String {
    let base = url.split('?').next().unwrap_or(url);
    let cut = base.rfind('/').unwrap_or(base.len());
    format!("{}/{db}", &base[..cut])
}

/// Start a server on a database of its own. `tweak` adjusts the config
/// (dev mail is on, the public URL is the listening address).
pub async fn start(admin_url: &str, mode: Mode, tweak: impl FnOnce(&mut Config)) -> Result<TestServer> {
    let admin = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(admin_url).await.context("test postgres")?;
    let db_name = format!("test_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("create database {db_name}"))).execute(&admin).await?;
    let url = with_db(admin_url, &db_name);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let mut cfg = Config::new(url.clone(), mode);
    cfg.bind = addr;
    cfg.public_url = format!("http://{addr}");
    cfg.dev_mail = true;
    cfg.argon2 = Argon2Cost { m_kib: 1024, t: 1, p: 1 };
    cfg.rate = RateConfig { ip_burst: 100_000, ip_per_sec: 10_000.0, account_burst: 100_000, account_per_sec: 10_000.0 };
    cfg.lockout_base = Duration::from_secs(1);
    cfg.licence_keys = Vec::new();
    cfg.first_org_name = "Acme".into();
    tweak(&mut cfg);

    let db = crate::db::connect(&url, 20).await?;
    crate::db::migrate(&db).await?;
    let keys = Arc::new(MemoryKeyProvider::new(remail_proto::new_id("dep")));
    let state = AppState::new(cfg, db.clone(), keys).await?;
    let first_invite = crate::orgs::startup(&state.cfg, &db, &state.env, true).await?;
    let serve_state = state.clone();
    let task = tokio::spawn(async move {
        let _ = crate::app::serve(serve_state, listener, std::future::pending()).await;
    });
    Ok(TestServer { url: format!("http://{addr}"), db, state, first_invite, admin, db_name, task })
}

impl TestServer {
    /// Stop serving and drop the database.
    pub async fn finish(self) {
        self.task.abort();
        self.db.close().await;
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists {} with (force)", self.db_name))).execute(&self.admin).await;
        self.admin.close().await;
    }

    /// The org a user belongs to.
    pub async fn org_of(&self, email: &str) -> Result<crate::orgs::Org> {
        let id: String = sqlx::query_scalar("select org_id from users where email = $1").bind(email).fetch_one(&self.db).await?;
        crate::orgs::by_id(&self.db, &id).await?.context("no such org")
    }

    /// The org's config as an admin (or `org config set`) would edit it.
    pub async fn org_config(&self, org_id: &str) -> Result<remail_proto::org::OrgConfig> {
        let org = crate::orgs::by_id(&self.db, org_id).await?.context("no such org")?;
        Ok(crate::orgs::config(&self.db, &self.state.env, &org).await?.1)
    }

    /// Save the org config the way the CLI does: through Postgres `NOTIFY`,
    /// so devices hear about it over the notices socket.
    pub async fn set_org_config(&self, org_id: &str, cfg: &remail_proto::org::OrgConfig) -> Result<i64> {
        crate::orgs::set_config(&self.db, &self.state.env, org_id, cfg, "testing-cli").await
    }

    pub async fn set_org_secrets(&self, org_id: &str, secrets: &remail_proto::org::OrgSecrets) -> Result<i64> {
        crate::orgs::set_secrets(&self.db, &self.state.env, org_id, secrets).await
    }

    /// `(version, device that wrote it last)` of a user's section.
    pub async fn section_meta(&self, email: &str, section: &str) -> Result<Option<(i64, Option<String>)>> {
        Ok(crate::sections::testing::section_meta(&self.db, email, section).await?)
    }

    /// Ciphertext of every section of a user, concatenated (to show what
    /// the database holds isn't readable).
    pub async fn raw_sections(&self, email: &str) -> Result<Vec<u8>> {
        Ok(crate::sections::testing::ciphertexts(&self.db, email).await?.concat())
    }

    /// Live (not revoked) devices of a user.
    pub async fn live_devices(&self, email: &str) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "select d.id from devices d join users u on u.id = d.user_id where u.email = $1 and d.revoked_at is null order by d.created_at",
        )
        .bind(email)
        .fetch_all(&self.db)
        .await?)
    }

    /// The TOTP code for a base32 secret at `unix_secs`.
    pub fn totp_code(secret_base32: &str, unix_secs: u64) -> Result<String> {
        let raw = totp_rs::Secret::Encoded(secret_base32.to_string()).to_bytes().map_err(|e| anyhow::anyhow!("bad TOTP secret: {e:?}"))?;
        Ok(crate::auth::totp::code_at(&raw, unix_secs))
    }
}
