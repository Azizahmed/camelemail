//! Admin API (`/v1/admin/*`): the organisation's email providers, its AI
//! assistant and who administers it. Only owners and admins of the org get
//! in (403 for everyone else), and every call is audited.
//!
//! Isolation rule: admin code works with account metadata only. It must never
//! read `user_sections` or ask for a user data key. That is enforced by
//! construction: user data keys are only handed out against
//! [`crate::sections::SyncPath`], which only the `sections` module can build,
//! and `tests/admin_isolation.rs` scans this directory for any reference to
//! sections, user keys or the sync decrypt path.
//!
//! Secrets are write-only here: answers say whether the Google client secret
//! and the AI key are set, never what they are.

pub mod validate;

use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::routing::{get, put};
use axum::Router;
use chrono::{DateTime, Utc};
use remail_proto::api::{AdminAiUpdate, AdminConfig, AdminProvidersUpdate, AdminUser, RoleUpdate};
use remail_proto::ErrorCode;
use serde::Deserialize;
use serde_json::json;
use sqlx::FromRow;

use crate::app::{AppState, ClientIp};
use crate::audit::{self, Actor, Event};
use crate::auth::AuthCtx;
use crate::error::{ApiJson, ApiPath, AppError, AppResult};
use crate::orgs;
use crate::redact::Redacted;

/// Shown in the change notice's origin (not this process's hub, so every
/// process, this one included, relays it to its devices).
const NOTICE_ORIGIN: &str = "admin";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/config", get(get_config))
        .route("/admin/providers", put(put_providers))
        .route("/admin/ai", put(put_ai))
        .route("/admin/users", get(list_users))
        .route("/admin/users/{id}/role", put(put_role))
}

/// A signed-in owner or admin, acting on their own organisation.
pub struct AdminCtx(pub AuthCtx);

impl FromRequestParts<AppState> for AdminCtx {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> Result<Self, Self::Rejection> {
        let ctx = AuthCtx::from_request_parts(parts, s).await?;
        if !ctx.is_admin() {
            return Err(AppError::forbidden("Only your organisation's administrators can do this."));
        }
        s.limit_user(&ctx.user_id)?;
        Ok(AdminCtx(ctx))
    }
}

fn admin_event<'a>(action: &'a str, ctx: &'a AuthCtx, ip: Option<std::net::IpAddr>) -> Event<'a> {
    Event::new(action, Actor::Admin(&ctx.user_id)).org(&ctx.org_id).ip(ip)
}

/// What a secret field asked for: keep, clear or replace.
fn change(v: &Option<Redacted<String>>) -> &'static str {
    match v {
        None => "kept",
        Some(k) if k.expose().trim().is_empty() => "cleared",
        Some(_) => "replaced",
    }
}

async fn caller_org(s: &AppState, ctx: &AuthCtx) -> AppResult<orgs::Org> {
    orgs::by_id(&s.db, &ctx.org_id).await?.ok_or_else(AppError::unauthorized)
}

/// The org's providers and AI setup, secrets reduced to "set or not".
async fn admin_config(s: &AppState, ctx: &AuthCtx) -> AppResult<AdminConfig> {
    let org = caller_org(s, ctx).await?;
    let (version, mut config) = orgs::config(&s.db, &s.env, &org).await?;
    let ai_key_set = !orgs::secrets(&s.db, &s.env, &org.id).await?.ai.api_key.is_empty();
    let google_secret_set = !config.providers.google_app.client_secret.is_empty();
    config.providers.google_app.client_secret.clear();
    Ok(AdminConfig { version, providers: config.providers, ai: config.ai, google_secret_set, ai_key_set })
}

async fn get_config(State(s): State<AppState>, AdminCtx(ctx): AdminCtx, ClientIp(ip): ClientIp) -> AppResult<ApiJson<AdminConfig>> {
    let out = admin_config(&s, &ctx).await?;
    audit::log(&s.db, admin_event("admin.config.read", &ctx, ip)).await;
    Ok(ApiJson(out))
}

/// Email providers: toggles, the Google and Microsoft apps, the preset
/// servers. Saving bumps the org config version, and devices are told.
async fn put_providers(
    State(s): State<AppState>,
    AdminCtx(ctx): AdminCtx,
    ClientIp(ip): ClientIp,
    ApiJson(req): ApiJson<AdminProvidersUpdate>,
) -> AppResult<ApiJson<AdminConfig>> {
    let new_secret = req.google_client_secret.map(Redacted::new);
    let mut providers = validate::providers(req.providers)?;
    let org = caller_org(&s, &ctx).await?;
    let (_, mut config) = orgs::config(&s.db, &s.env, &org).await?;
    // The Google app is a "Desktop app" client: its secret goes to devices in
    // the config. A server-held secret (OrgSecrets) is never set from here.
    providers.google_app.client_secret = match &new_secret {
        None => config.providers.google_app.client_secret.clone(),
        Some(k) => validate::secret(k.expose(), "Google client secret")?,
    };
    let changed = config.providers != providers;
    let meta = json!({
        "google": providers.google,
        "microsoft": providers.microsoft,
        "imap": providers.imap,
        "manualServers": providers.manual_servers,
        "presets": providers.presets.len(),
        "googleSecret": change(&new_secret),
        "changed": changed,
    });
    if changed {
        config.providers = providers;
        orgs::set_config(&s.db, &s.env, &org.id, &config, NOTICE_ORIGIN).await?;
    }
    audit::log(&s.db, admin_event("admin.providers.update", &ctx, ip).meta(meta)).await;
    Ok(ApiJson(admin_config(&s, &ctx).await?))
}

/// The AI assistant: provider, URL, model, answer length, on/off and the key.
/// Always saves the config, so devices fetch the key again.
async fn put_ai(
    State(s): State<AppState>,
    AdminCtx(ctx): AdminCtx,
    ClientIp(ip): ClientIp,
    ApiJson(req): ApiJson<AdminAiUpdate>,
) -> AppResult<ApiJson<AdminConfig>> {
    let new_key = req.api_key.map(Redacted::new);
    let ai = validate::ai(req.ai)?;
    let key = match &new_key {
        None => None,
        Some(k) => Some(Redacted::new(validate::secret(k.expose(), "API key")?)),
    };
    let org = caller_org(&s, &ctx).await?;
    if let Some(k) = &key {
        let mut stored = orgs::secrets(&s.db, &s.env, &org.id).await?;
        if stored.ai.api_key != *k.expose() {
            stored.ai.api_key = k.expose().clone();
            orgs::set_secrets(&s.db, &s.env, &org.id, &stored).await?;
        }
    }
    let (_, mut config) = orgs::config(&s.db, &s.env, &org).await?;
    let meta = json!({"enabled": ai.enabled, "provider": ai.provider, "key": change(&new_key)});
    config.ai = ai;
    orgs::set_config(&s.db, &s.env, &org.id, &config, NOTICE_ORIGIN).await?;
    audit::log(&s.db, admin_event("admin.ai.update", &ctx, ip).meta(meta)).await;
    Ok(ApiJson(admin_config(&s, &ctx).await?))
}

#[derive(FromRow)]
struct UserMeta {
    id: String,
    name: String,
    email: String,
    role: String,
    created_at: DateTime<Utc>,
    last_active_at: Option<DateTime<Utc>>,
    email_verified_at: Option<DateTime<Utc>>,
    disabled_at: Option<DateTime<Utc>>,
    totp_enabled_at: Option<DateTime<Utc>>,
}

macro_rules! user_meta_cols {
    () => {
        "id, name, email, role, created_at, last_active_at, email_verified_at, disabled_at, totp_enabled_at"
    };
}

impl From<UserMeta> for AdminUser {
    fn from(u: UserMeta) -> Self {
        AdminUser {
            id: u.id,
            name: u.name,
            email: u.email,
            role: u.role,
            created_at: u.created_at.timestamp_millis(),
            last_active_at: u.last_active_at.map(|t| t.timestamp_millis()),
            verified: u.email_verified_at.is_some(),
            disabled: u.disabled_at.is_some(),
            totp: u.totp_enabled_at.is_some(),
        }
    }
}

#[derive(Deserialize)]
struct UsersQuery {
    #[serde(default)]
    query: Option<String>,
}

const USERS_LIMIT: i64 = 500;

/// The org's people, owners and admins first. Account metadata only.
async fn list_users(
    State(s): State<AppState>,
    AdminCtx(ctx): AdminCtx,
    ClientIp(ip): ClientIp,
    Query(q): Query<UsersQuery>,
) -> AppResult<ApiJson<Vec<AdminUser>>> {
    let needle = q.query.map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty());
    if needle.as_ref().is_some_and(|n| n.chars().count() > 200) {
        return Err(AppError::bad_request("The search is too long."));
    }
    // `%` and `_` are taken literally.
    let pattern = needle.as_ref().map(|n| format!("%{}%", n.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
    let rows: Vec<UserMeta> = sqlx::query_as(concat!(
        "select ",
        user_meta_cols!(),
        " from users where org_id = $1 and ($2::text is null or lower(email) like $2 or lower(name) like $2)
         order by case role when 'owner' then 0 when 'admin' then 1 else 2 end, lower(email) limit $3"
    ))
    .bind(&ctx.org_id)
    .bind(&pattern)
    .bind(USERS_LIMIT)
    .fetch_all(&s.db)
    .await?;
    audit::log(&s.db, admin_event("admin.users.list", &ctx, ip).meta(json!({"search": needle.is_some(), "count": rows.len()}))).await;
    Ok(ApiJson(rows.into_iter().map(AdminUser::from).collect()))
}

/// Grant or remove the admin role. Owners can't be changed here, and the
/// org always keeps at least one owner or admin.
async fn put_role(
    State(s): State<AppState>,
    AdminCtx(ctx): AdminCtx,
    ClientIp(ip): ClientIp,
    ApiPath(id): ApiPath<String>,
    ApiJson(req): ApiJson<RoleUpdate>,
) -> AppResult<ApiJson<AdminUser>> {
    let role = req.role.trim();
    if !matches!(role, "admin" | "user") {
        return Err(AppError::bad_request("The role must be admin or user."));
    }
    let mut tx = s.db.begin().await?;
    // One role change per org at a time, so two admins can't demote each other at once.
    sqlx::query("select 1 from orgs where id = $1 for update").bind(&ctx.org_id).execute(&mut *tx).await?;
    let target: UserMeta = sqlx::query_as(concat!("select ", user_meta_cols!(), " from users where id = $1 and org_id = $2"))
        .bind(&id)
        .bind(&ctx.org_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(AppError::not_found)?;
    if target.role == "owner" {
        return Err(AppError::forbidden("Owners are set on the server and can't be changed here."));
    }
    let from = target.role.clone();
    if from == role {
        return Ok(ApiJson(target.into()));
    }
    if from == "admin" {
        let others: i64 = sqlx::query_scalar(
            "select count(*) from users where org_id = $1 and id <> $2 and role in ('owner', 'admin') and disabled_at is null",
        )
        .bind(&ctx.org_id)
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
        if others == 0 {
            return Err(AppError::new(ErrorCode::Conflict, "The organisation needs at least one administrator."));
        }
    }
    let updated: UserMeta = sqlx::query_as(concat!("update users set role = $2 where id = $1 returning ", user_meta_cols!()))
        .bind(&id)
        .bind(role)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    audit::log(&s.db, admin_event("admin.user.role", &ctx, ip).target(&id).meta(json!({"from": from, "to": role}))).await;
    Ok(ApiJson(updated.into()))
}
