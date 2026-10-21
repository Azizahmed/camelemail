//! Opaque tokens: access/refresh tokens and one-time links (verify, reset,
//! invite, 2FA challenges). Only their SHA-256 is stored.

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgExecutor, PgPool};

use crate::crypto::{random_bytes, sha256};
use crate::redact::Redacted;

pub const VERIFY: &str = "verify";
pub const RESET: &str = "reset";
pub const INVITE: &str = "invite";
pub const TWO_FACTOR: &str = "2fa";
pub const TWO_FACTOR_SETUP: &str = "2fa_setup";

/// A challenge accepts this many wrong codes before it's burned.
pub const MAX_ATTEMPTS: i32 = 5;

/// `<prefix>_<43 chars of base64url>` (256 random bits) and its hash.
pub fn new_token(prefix: &str) -> (Redacted<String>, Vec<u8>) {
    let t = format!("{prefix}_{}", B64.encode(random_bytes::<32>()));
    let h = hash(&t);
    (Redacted::new(t), h)
}

pub fn hash(token: &str) -> Vec<u8> {
    sha256(token.trim().as_bytes())
}

pub fn ttl(kind: &str) -> Duration {
    match kind {
        VERIFY => Duration::hours(24),
        RESET => Duration::hours(1),
        INVITE => Duration::days(7),
        TWO_FACTOR => Duration::minutes(5),
        TWO_FACTOR_SETUP => Duration::minutes(15),
        _ => Duration::minutes(5),
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct OneTime {
    pub hash: Vec<u8>,
    pub kind: String,
    pub user_id: Option<String>,
    pub org_id: String,
    pub email: Option<String>,
    pub role: Option<String>,
    pub meta: Value,
    pub attempts: i32,
    pub expires_at: DateTime<Utc>,
}

pub struct NewOneTime<'a> {
    pub kind: &'a str,
    pub user_id: Option<&'a str>,
    pub org_id: &'a str,
    pub email: Option<&'a str>,
    pub role: Option<&'a str>,
    pub meta: Value,
}

/// Stores a one-time token and returns it.
pub async fn issue(db: impl PgExecutor<'_>, t: NewOneTime<'_>) -> sqlx::Result<Redacted<String>> {
    let prefix = match t.kind {
        INVITE => "inv",
        VERIFY => "ver",
        RESET => "rst",
        _ => "chl",
    };
    let (token, h) = new_token(prefix);
    sqlx::query(
        "insert into auth_tokens (hash, kind, user_id, org_id, email, role, meta, expires_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(h)
    .bind(t.kind)
    .bind(t.user_id)
    .bind(t.org_id)
    .bind(t.email)
    .bind(t.role)
    .bind(&t.meta)
    .bind(Utc::now() + ttl(t.kind))
    .execute(db)
    .await?;
    Ok(token)
}

/// A live (unused, unexpired) token of `kind`.
pub async fn find(db: impl PgExecutor<'_>, kind: &str, token: &str) -> sqlx::Result<Option<OneTime>> {
    sqlx::query_as(
        "select hash, kind, user_id, org_id, email, role, meta, attempts, expires_at from auth_tokens
         where hash = $1 and kind = $2 and used_at is null and expires_at > now()",
    )
    .bind(hash(token))
    .bind(kind)
    .fetch_optional(db)
    .await
}

/// Marks a token used; false if somebody else used it first.
pub async fn consume(db: impl PgExecutor<'_>, h: &[u8]) -> sqlx::Result<bool> {
    let r = sqlx::query("update auth_tokens set used_at = now() where hash = $1 and used_at is null")
        .bind(h)
        .execute(db)
        .await?;
    Ok(r.rows_affected() == 1)
}

/// Counts a wrong code; burns the challenge at [`MAX_ATTEMPTS`].
pub async fn wrong_attempt(db: impl PgExecutor<'_>, h: &[u8]) -> sqlx::Result<()> {
    sqlx::query(
        "update auth_tokens set attempts = attempts + 1,
           used_at = case when attempts + 1 >= $2 then now() else used_at end
         where hash = $1",
    )
    .bind(h)
    .bind(MAX_ATTEMPTS)
    .execute(db)
    .await?;
    Ok(())
}

/// An invite to `org_id`. `email` pins who may use it (`None`: anyone with
/// the link, used for the first on-prem admin).
pub async fn create_invite(db: &PgPool, org_id: &str, email: Option<&str>, role: &str) -> sqlx::Result<Redacted<String>> {
    let email = email.map(crate::auth::normalize_email);
    issue(
        db,
        NewOneTime {
            kind: INVITE,
            user_id: None,
            org_id,
            email: email.as_deref(),
            role: Some(role),
            meta: Value::Object(Default::default()),
        },
    )
    .await
}
