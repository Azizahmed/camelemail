//! The sync path (design §5.3): `GET/PUT /v1/sections[/{id}]`.
//!
//! This is the only module that can decrypt a user's sections: the user data
//! key is handed out against a [`SyncPath`], which can't be built anywhere
//! else, and the decrypt function lives in a private submodule. Every decrypt
//! is audited (`decrypt.sync`, aggregated per device per 10 minutes).
//!
//! The server never merges. A `PUT` carries `If-Match: "v<n>"`; a stale
//! version gets `412` with the current document, and the client merges and
//! retries (the CRDT in `remail_proto::sync` makes that converge).

mod crypt;

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use chrono::{DateTime, Utc};
use remail_proto::api::{PutSection, PutSectionResponse, Section, SectionIndex, SectionInfo, VersionMismatch};
use remail_proto::sections::{self, Violation};
use remail_proto::sync::SectionDoc;
use remail_proto::{ApiError, ErrorCode};
use sqlx::FromRow;

use crate::app::AppState;
use crate::auth::AuthCtx;
use crate::error::{ApiJson, ApiPath, AppError, AppResult};
use crate::notices::HubEvent;

/// Proof that a caller is the sync path. The private field means only this
/// module can make one, and only it can therefore ask for a user data key.
pub struct SyncPath(());

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/sections", get(index))
        .route("/sections/{id}", get(get_section).put(put_section))
}

fn etag(version: i64) -> HeaderValue {
    HeaderValue::from_str(&format!("\"v{version}\"")).unwrap_or_else(|_| HeaderValue::from_static("\"v0\""))
}

/// `"v7"`, `W/"v7"`, `v7` or `7`.
fn parse_if_match(headers: &HeaderMap) -> AppResult<i64> {
    let raw = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::bad_request("If-Match is required (\"v<version>\", \"v0\" for a new section)."))?;
    let v = raw.trim().trim_start_matches("W/").trim_matches('"');
    let v = v.strip_prefix('v').unwrap_or(v);
    v.parse::<i64>()
        .ok()
        .filter(|v| *v >= 0)
        .ok_or_else(|| AppError::bad_request("If-Match must look like \"v7\"."))
}

fn check_name(id: &str) -> AppResult<()> {
    if sections::is_valid(id) {
        Ok(())
    } else {
        Err(AppError::new(ErrorCode::NotFound, "No such section."))
    }
}

#[derive(FromRow)]
struct InfoRow {
    section: String,
    version: i64,
    updated_at: DateTime<Utc>,
}

async fn index(State(s): State<AppState>, ctx: AuthCtx) -> AppResult<ApiJson<SectionIndex>> {
    let rows: Vec<InfoRow> =
        sqlx::query_as("select section, version, updated_at from user_sections where user_id = $1 order by section")
            .bind(&ctx.user_id)
            .fetch_all(&s.db)
            .await?;
    Ok(ApiJson(SectionIndex {
        sections: rows
            .into_iter()
            .map(|r| SectionInfo { id: r.section, version: r.version, updated_at: r.updated_at.timestamp_millis() })
            .collect(),
        server_time: remail_proto::now_ms(),
    }))
}

async fn get_section(State(s): State<AppState>, ctx: AuthCtx, ApiPath(id): ApiPath<String>) -> AppResult<Response> {
    check_name(&id)?;
    let row: Option<(i64, Vec<u8>)> =
        sqlx::query_as("select version, ciphertext from user_sections where user_id = $1 and section = $2")
            .bind(&ctx.user_id)
            .bind(&id)
            .fetch_optional(&s.db)
            .await?;
    let section = match row {
        None => Section { id, version: 0, doc: SectionDoc::default() },
        Some((version, ct)) => {
            let dk = crypt::user_key(&s, &ctx).await?;
            let doc = crypt::decrypt_user_section(&dk, &ctx, &id, version, &ct)?;
            audit_decrypt(&s, &ctx).await;
            Section { id, version, doc }
        }
    };
    let tag = etag(section.version);
    let mut res = ApiJson(section).into_response();
    res.headers_mut().insert(header::ETAG, tag);
    Ok(res)
}

/// Section reads and writes count towards the device's `decrypt.sync` row.
async fn audit_decrypt(s: &AppState, ctx: &AuthCtx) {
    crate::audit::decrypt(&s.db, "sync", &ctx.org_id, &ctx.user_id, &ctx.device_id).await;
}

fn violation(v: Violation) -> AppError {
    match v {
        Violation::ProviderNotAllowed { provider, .. } => {
            AppError::new(ErrorCode::ProviderNotAllowed, format!("Your organisation doesn't allow {provider} mailboxes."))
        }
        Violation::ImapHostNotAllowed { host, .. } => {
            AppError::new(ErrorCode::ProviderNotAllowed, format!("Your organisation doesn't allow the mail server {host}."))
        }
        Violation::Malformed(m) => AppError::bad_request(m),
    }
}

async fn put_section(
    State(s): State<AppState>,
    ctx: AuthCtx,
    ApiPath(id): ApiPath<String>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<PutSection>,
) -> AppResult<Response> {
    check_name(&id)?;
    let expected = parse_if_match(&headers)?;
    let mut doc = body.doc;
    // Tombstones past their 30 days go now, so they don't grow forever.
    doc.gc(remail_proto::now_ms());
    let plain = serde_json::to_vec(&doc).map_err(anyhow::Error::from)?;
    if plain.len() > sections::MAX_SECTION_BYTES {
        return Err(AppError::new(ErrorCode::TooLarge, "This section is over 256 KiB."));
    }

    // Everything that needs another pool connection happens before the
    // transaction: concurrent writers wait on the row lock holding theirs.
    let dk = crypt::user_key(&s, &ctx).await?;
    let providers = if id == sections::ACCOUNTS {
        let org = crate::orgs::by_id(&s.db, &ctx.org_id).await?.ok_or_else(AppError::unauthorized)?;
        Some(crate::orgs::config(&s.db, &s.env, &org).await?.1.providers)
    } else {
        None
    };

    let mut tx = s.db.begin().await?;
    // One writer per user at a time: versions and the per-user size total
    // stay consistent under concurrent PUTs.
    sqlx::query("select 1 from users where id = $1 for update").bind(&ctx.user_id).execute(&mut *tx).await?;
    let current: Option<(i64, Vec<u8>)> =
        sqlx::query_as("select version, ciphertext from user_sections where user_id = $1 and section = $2")
            .bind(&ctx.user_id)
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await?;
    let current_version = current.as_ref().map(|c| c.0).unwrap_or(0);
    if current_version != expected {
        drop(tx);
        let doc = match &current {
            Some((v, ct)) => crypt::decrypt_user_section(&dk, &ctx, &id, *v, ct)?,
            None => SectionDoc::default(),
        };
        audit_decrypt(&s, &ctx).await;
        let body = VersionMismatch {
            error: ApiError::new(ErrorCode::VersionMismatch, "The section changed on another device."),
            current: Section { id, version: current_version, doc },
        };
        let mut res = (axum::http::StatusCode::PRECONDITION_FAILED, ApiJson(body)).into_response();
        res.headers_mut().insert(header::ETAG, etag(current_version));
        return Ok(res);
    }

    // The allow-list holds for every client, UI or not.
    if let Some(providers) = &providers {
        let previous = match &current {
            Some((v, ct)) => Some(crypt::decrypt_user_section(&dk, &ctx, &id, *v, ct)?),
            None => None,
        };
        sections::check_accounts(&doc, previous.as_ref(), providers).map_err(violation)?;
    }

    let others: i64 = sqlx::query_scalar(
        "select coalesce(sum(length(ciphertext)), 0)::bigint from user_sections where user_id = $1 and section <> $2",
    )
    .bind(&ctx.user_id)
    .bind(&id)
    .fetch_one(&mut *tx)
    .await?;
    if others as usize + plain.len() > sections::MAX_USER_BYTES {
        return Err(AppError::new(ErrorCode::TooLarge, "Your settings are over the 2 MiB limit."));
    }

    let version = current_version + 1;
    let ct = crypt::encrypt_user_section(&dk, &ctx, &id, version, &plain);
    sqlx::query(
        "insert into user_sections (user_id, section, version, ciphertext, updated_at, updated_by)
         values ($1, $2, $3, $4, now(), $5)
         on conflict (user_id, section) do update
           set version = excluded.version, ciphertext = excluded.ciphertext,
               updated_at = now(), updated_by = excluded.updated_by",
    )
    .bind(&ctx.user_id)
    .bind(&id)
    .bind(version)
    .bind(ct)
    .bind(&ctx.device_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    audit_decrypt(&s, &ctx).await;

    s.hub
        .publish(&s.db, HubEvent::Section { user: ctx.user_id.clone(), section: id, version })
        .await;
    let mut res = ApiJson(PutSectionResponse { version }).into_response();
    res.headers_mut().insert(header::ETAG, etag(version));
    Ok(res)
}

/// Section metadata and raw ciphertext for `crate::testing` (the table
/// stays private to this module; nothing here decrypts).
#[cfg(feature = "testing")]
pub mod testing {
    use sqlx::PgPool;

    /// `(version, device that wrote it last)`.
    pub async fn section_meta(db: &PgPool, email: &str, section: &str) -> sqlx::Result<Option<(i64, Option<String>)>> {
        sqlx::query_as(
            "select s.version, s.updated_by from user_sections s join users u on u.id = s.user_id where u.email = $1 and s.section = $2",
        )
        .bind(email)
        .bind(section)
        .fetch_optional(db)
        .await
    }

    pub async fn ciphertexts(db: &PgPool, email: &str) -> sqlx::Result<Vec<Vec<u8>>> {
        sqlx::query_scalar("select s.ciphertext from user_sections s join users u on u.id = s.user_id where u.email = $1")
            .bind(email)
            .fetch_all(db)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn if_match_forms() {
        let h = |v: &str| {
            let mut m = HeaderMap::new();
            m.insert(header::IF_MATCH, HeaderValue::from_str(v).unwrap());
            parse_if_match(&m).ok()
        };
        assert_eq!(h("\"v7\""), Some(7));
        assert_eq!(h("W/\"v7\""), Some(7));
        assert_eq!(h("v0"), Some(0));
        assert_eq!(h("12"), Some(12));
        assert_eq!(h("\"x\""), None);
        assert_eq!(h("-1"), None);
        assert!(parse_if_match(&HeaderMap::new()).is_err());
    }
}
