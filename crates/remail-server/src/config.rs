//! Server configuration, read from the environment.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ed25519_dalek::VerifyingKey;

/// Multi-tenant service, or one customer's own server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Hosted,
    Onprem,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Hosted => "hosted",
            Mode::Onprem => "onprem",
        }
    }
}

/// argon2id cost. Production uses the design's m=64 MiB, t=3, p=1; tests use
/// something cheap so a sign-up doesn't take a second.
#[derive(Debug, Clone, Copy)]
pub struct Argon2Cost {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl Default for Argon2Cost {
    fn default() -> Self {
        Argon2Cost { m_kib: 64 * 1024, t: 3, p: 1 }
    }
}

/// Token-bucket sizes for the unauthenticated auth routes.
#[derive(Debug, Clone, Copy)]
pub struct RateConfig {
    /// Requests an IP may burst.
    pub ip_burst: u32,
    /// Refill per second, per IP.
    pub ip_per_sec: f64,
    /// Login attempts one e-mail may burst (independent of the lockout).
    pub account_burst: u32,
    pub account_per_sec: f64,
}

impl Default for RateConfig {
    fn default() -> Self {
        RateConfig { ip_burst: 30, ip_per_sec: 0.5, account_burst: 10, account_per_sec: 0.1 }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub mode: Mode,
    pub bind: SocketAddr,
    /// Base for links in e-mails (`https://mail.acme.com`).
    pub public_url: String,
    pub master_key_file: PathBuf,
    /// lettre URL, e.g. `smtps://user:pass@smtp.example.com`.
    pub smtp_url: Option<String>,
    /// SNDR (CranL's e-mail service) API key, used when there's no SMTP URL.
    pub sndr_api_key: Option<String>,
    pub sndr_url: String,
    pub mail_from: String,
    /// Keep sent mail in memory and expose it at `GET /v1/dev/mail`. Debug
    /// builds only; ignored in release.
    pub dev_mail: bool,
    pub argon2: Argon2Cost,
    /// First lockout after five failed sign-ins; it doubles from there.
    pub lockout_base: Duration,
    pub lockout_max: Duration,
    pub rate: RateConfig,
    /// Honour `X-Forwarded-For` (behind Caddy or another proxy).
    pub trust_proxy: bool,
    pub access_ttl: Duration,
    pub google_token_url: String,
    /// Licence issuer public keys. Empty = an unlicensed build: no licence
    /// enforcement at all.
    pub licence_keys: Vec<VerifyingKey>,
    /// On-prem: name of the organisation created on first start.
    pub first_org_name: String,
    /// Hosted: the Google OAuth app ("Desktop app" client) the public org
    /// signs Gmail and Google Calendar in with. Devices get both and talk to
    /// Google directly (an installed-app secret isn't confidential).
    pub google_client_id: Option<String>,
    pub google_client_secret: Option<String>,
}

/// Issuer keys compiled in with `REMAIL_LICENCE_PUBKEYS=<b64url>,<b64url>`.
pub fn builtin_licence_keys() -> Vec<VerifyingKey> {
    option_env!("REMAIL_LICENCE_PUBKEYS")
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .filter_map(remail_proto::licence::parse_public_key)
        .collect()
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

impl Config {
    /// Defaults for everything but the database (tests fill in the rest).
    pub fn new(database_url: impl Into<String>, mode: Mode) -> Self {
        Config {
            database_url: database_url.into(),
            mode,
            bind: SocketAddr::from(([127, 0, 0, 1], 8080)),
            public_url: "http://localhost:8080".into(),
            master_key_file: PathBuf::from("/etc/remail/master.key"),
            smtp_url: None,
            sndr_api_key: None,
            sndr_url: "https://api.sndr.sh/v1/send".into(),
            mail_from: "Remail <no-reply@localhost>".into(),
            dev_mail: false,
            argon2: Argon2Cost::default(),
            lockout_base: Duration::from_secs(60),
            lockout_max: Duration::from_secs(3600),
            rate: RateConfig::default(),
            trust_proxy: false,
            access_ttl: Duration::from_secs(15 * 60),
            google_token_url: "https://oauth2.googleapis.com/token".into(),
            licence_keys: builtin_licence_keys(),
            first_org_name: "My organisation".into(),
            google_client_id: None,
            google_client_secret: None,
        }
    }

    pub fn from_env() -> Result<Self> {
        let database_url = env("DATABASE_URL").context("DATABASE_URL is not set")?;
        let mode = match env("REMAIL_MODE").as_deref() {
            None | Some("onprem") => Mode::Onprem,
            Some("hosted") => Mode::Hosted,
            Some(other) => bail!("REMAIL_MODE must be hosted or onprem, not {other:?}"),
        };
        let mut c = Config::new(database_url, mode);
        if let Some(b) = env("REMAIL_BIND") {
            c.bind = b.parse().with_context(|| format!("REMAIL_BIND {b:?} is not host:port"))?;
        } else if let Some(port) = env("PORT") {
            // Hosting platforms hand the port over in PORT and route to all interfaces.
            let port: u16 = port.parse().with_context(|| format!("PORT {port:?} is not a port number"))?;
            c.bind = std::net::SocketAddr::from(([0, 0, 0, 0], port));
        }
        c.public_url = env("REMAIL_PUBLIC_URL").unwrap_or_else(|| format!("http://{}", c.bind));
        c.public_url = c.public_url.trim_end_matches('/').to_string();
        if let Some(p) = env("REMAIL_MASTER_KEY_FILE") {
            c.master_key_file = PathBuf::from(p);
        }
        c.smtp_url = env("REMAIL_SMTP_URL");
        c.sndr_api_key = env("REMAIL_SNDR_API_KEY");
        if let Some(u) = env("REMAIL_SNDR_URL") {
            c.sndr_url = u;
        }
        if let Some(f) = env("REMAIL_MAIL_FROM") {
            c.mail_from = f;
        }
        c.dev_mail = cfg!(debug_assertions) && env("REMAIL_DEV_MAIL").as_deref() == Some("1");
        c.trust_proxy = env("REMAIL_TRUST_PROXY").as_deref() == Some("1");
        if let Some(u) = env("REMAIL_GOOGLE_TOKEN_URL") {
            c.google_token_url = u;
        }
        if let Some(n) = env("REMAIL_ORG_NAME") {
            c.first_org_name = n;
        }
        c.google_client_id = env("REMAIL_GOOGLE_CLIENT_ID");
        c.google_client_secret = env("REMAIL_GOOGLE_CLIENT_SECRET");
        Ok(c)
    }
}
