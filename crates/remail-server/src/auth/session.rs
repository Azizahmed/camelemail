//! Devices and sessions: access tokens (15 min) and rotating refresh tokens.
//!
//! Each refresh writes a new session row in the same family and marks the old
//! one rotated. Presenting a rotated refresh token again means it was copied,
//! and the whole family is revoked; unless the session that refresh made was
//! never used, which means its reply never reached the device (see `refresh`).

use axum::extract::{FromRequestParts, OptionalFromRequestParts};
use axum::http::request::Parts;
use chrono::{DateTime, Duration, Utc};
use remail_proto::api::DeviceInfo;
use sqlx::{FromRow, PgConnection};

use super::tokens::{hash, new_token};
use crate::app::AppState;
use crate::error::{AppError, AppResult};
use crate::notices::HubEvent;
use crate::redact::Redacted;

/// The signed-in caller of a request.
#[derive(Debug, Clone, FromRow)]
pub struct AuthCtx {
    pub session_id: String,
    pub device_id: String,
    pub user_id: String,
    pub org_id: String,
    pub role: String,
    pub email: String,
}

impl AuthCtx {
    pub async fn from_access(s: &AppState, token: &str) -> AppResult<Self> {
        // The first use is recorded: it shows the device got the session (see `refresh`).
        let ctx: Option<AuthCtx> = sqlx::query_as(
            "with used as (update sessions set access_used_at = now() where access_hash = $1 and access_used_at is null)
             select s.id as session_id, s.device_id, u.id as user_id, u.org_id, u.role, u.email
             from sessions s
             join devices d on d.id = s.device_id
             join users u on u.id = s.user_id
             join orgs o on o.id = u.org_id
             where s.access_hash = $1 and s.access_expires_at > now() and s.revoked_at is null
               and d.revoked_at is null and u.disabled_at is null and o.disabled_at is null",
        )
        .bind(hash(token))
        .fetch_optional(&s.db)
        .await?;
        ctx.ok_or_else(AppError::unauthorized)
    }

    pub fn is_admin(&self) -> bool {
        matches!(self.role.as_str(), "admin" | "owner")
    }
}

fn bearer(parts: &Parts) -> Option<String> {
    let h = parts.headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    let t = h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer "))?;
    Some(t.trim().to_string())
}

impl FromRequestParts<AppState> for AuthCtx {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> Result<Self, Self::Rejection> {
        let token = Redacted::new(bearer(parts).ok_or_else(AppError::unauthorized)?);
        AuthCtx::from_access(s, token.expose()).await
    }
}

impl OptionalFromRequestParts<AppState> for AuthCtx {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> Result<Option<Self>, Self::Rejection> {
        match bearer(parts) {
            None => Ok(None),
            Some(t) => {
                let token = Redacted::new(t);
                AuthCtx::from_access(s, token.expose()).await.map(Some)
            }
        }
    }
}

pub const PLATFORMS: [&str; 5] = ["linux", "windows", "macos", "ios", "android"];

pub fn check_device(info: &DeviceInfo) -> AppResult<()> {
    if !PLATFORMS.contains(&info.platform.as_str()) {
        return Err(AppError::bad_request("Unknown device platform."));
    }
    if info.name.trim().is_empty() || info.name.chars().count() > 100 || info.app_version.len() > 40 {
        return Err(AppError::bad_request("Device name is missing or too long."));
    }
    Ok(())
}

pub async fn create_device(db: &mut PgConnection, user_id: &str, info: &DeviceInfo) -> sqlx::Result<String> {
    let id = remail_proto::new_id("dev");
    sqlx::query(
        "insert into devices (id, user_id, name, platform, app_version, last_seen_at) values ($1, $2, $3, $4, $5, now())",
    )
    .bind(&id)
    .bind(user_id)
    .bind(info.name.trim())
    .bind(&info.platform)
    .bind(&info.app_version)
    .execute(db)
    .await?;
    Ok(id)
}

pub struct Issued {
    pub access: Redacted<String>,
    pub refresh: Redacted<String>,
    pub access_expires_at: DateTime<Utc>,
    /// The session row's id.
    pub id: String,
}

/// A new session row (a new family unless `family` is given).
pub async fn issue(
    s: &AppState,
    db: &mut PgConnection,
    user_id: &str,
    device_id: &str,
    family: Option<&str>,
    session_days: u32,
) -> sqlx::Result<Issued> {
    let (access, access_hash) = new_token("rma");
    let (refresh, refresh_hash) = new_token("rmr");
    let now = Utc::now();
    let access_expires_at = now + Duration::from_std(s.cfg.access_ttl).unwrap_or(Duration::minutes(15));
    let days = i64::from(session_days.clamp(1, 365));
    let family = family.map(String::from).unwrap_or_else(|| remail_proto::new_id("fam"));
    let id = remail_proto::new_id("ses");
    sqlx::query(
        "insert into sessions (id, device_id, user_id, family, refresh_hash, access_hash, access_expires_at, expires_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(&id)
    .bind(device_id)
    .bind(user_id)
    .bind(family)
    .bind(refresh_hash)
    .bind(access_hash)
    .bind(access_expires_at)
    .bind(now + Duration::days(days))
    .execute(db)
    .await?;
    Ok(Issued { access, refresh, access_expires_at, id })
}

/// Signs a device out: its sessions and push token go, and its open notice
/// socket is told.
pub async fn revoke_device(s: &AppState, device_id: &str) -> sqlx::Result<()> {
    let mut tx = s.db.begin().await?;
    sqlx::query("update sessions set revoked_at = now() where device_id = $1 and revoked_at is null")
        .bind(device_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "update devices set revoked_at = coalesce(revoked_at, now()), push_kind = null, push_token_enc = null where id = $1",
    )
    .bind(device_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    s.hub.publish(&s.db, HubEvent::Device { device: device_id.into() }).await;
    Ok(())
}

/// Signs out every device of a user, except `keep` (the caller's own).
pub async fn revoke_user_devices(s: &AppState, user_id: &str, keep: Option<&str>) -> sqlx::Result<()> {
    let devices: Vec<String> =
        sqlx::query_scalar("select id from devices where user_id = $1 and revoked_at is null and id is distinct from $2")
            .bind(user_id)
            .bind(keep)
            .fetch_all(&s.db)
            .await?;
    for d in devices {
        revoke_device(s, &d).await?;
    }
    Ok(())
}

/// Revokes every session in a refresh-token family.
pub async fn revoke_family(s: &AppState, family: &str) -> sqlx::Result<()> {
    sqlx::query("update sessions set revoked_at = now() where family = $1 and revoked_at is null")
        .bind(family)
        .execute(&s.db)
        .await?;
    Ok(())
}
