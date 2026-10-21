//! Discovery and org-level reads: `/v1/meta`, `/v1/orgcode/{code}`,
//! `/v1/org/config`, `/v1/providers` and the server-side Google code exchange.

use axum::extract::State;
use axum::routing::{get, post};
use axum::Router;
use remail_proto::api::{
    providers_for, GoogleExchange, GoogleRefresh, GoogleTokens, Meta, OrgCodeResolved, OrgConfigResponse, ProviderOption,
};
use remail_proto::ErrorCode;
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::audit::{self, Actor, Event};
use crate::auth::AuthCtx;
use crate::config::Mode;
use crate::error::{ApiJson, ApiPath, AppError, AppResult};
use crate::orgs;
use crate::redact::Redacted;

/// Oldest desktop/mobile client this server still talks to.
pub const MIN_CLIENT: &str = "0.2.0";

pub fn routes() -> Router<AppState> {
    let r = Router::new()
        .route("/meta", get(meta))
        .route("/orgcode/{code}", get(orgcode))
        .route("/org/config", get(org_config))
        .route("/providers", get(providers))
        .route("/providers/google/token", post(google_token))
        .route("/providers/google/refresh", post(google_refresh));
    #[cfg(debug_assertions)]
    let r = r.route("/dev/mail", get(dev_mail));
    r
}

async fn meta(State(s): State<AppState>) -> AppResult<ApiJson<Meta>> {
    let lic = crate::licence::status(&s.cfg, &s.db, &s.env).await?;
    let (org_name, signup) = match s.cfg.mode {
        Mode::Hosted => {
            let open = orgs::by_slug(&s.db, orgs::PUBLIC_SLUG).await?.is_some_and(|o| o.signup_mode == "open");
            (None, if open { "open" } else { "invite" })
        }
        Mode::Onprem => (orgs::first(&s.db).await?.map(|o| o.name), "invite"),
    };
    Ok(ApiJson(Meta {
        mode: s.cfg.mode.as_str().into(),
        version: env!("CARGO_PKG_VERSION").into(),
        min_client: MIN_CLIENT.into(),
        org_name,
        signup: signup.into(),
        verify_email: s.cfg.mode == Mode::Hosted,
        licence: lic.state,
    }))
}

/// Hosted directory: which server an org code belongs to.
async fn orgcode(State(s): State<AppState>, ApiPath(code): ApiPath<String>) -> AppResult<ApiJson<OrgCodeResolved>> {
    let code = code.trim().to_lowercase();
    if !orgs::valid_slug(&code) {
        return Err(AppError::not_found());
    }
    let org = orgs::by_slug(&s.db, &code).await?.filter(|o| o.disabled_at.is_none()).ok_or_else(AppError::not_found)?;
    Ok(ApiJson(OrgCodeResolved { server: org.server_url.unwrap_or_else(|| s.cfg.public_url.clone()) }))
}

async fn caller_org(s: &AppState, ctx: &AuthCtx) -> AppResult<orgs::Org> {
    orgs::by_id(&s.db, &ctx.org_id).await?.ok_or_else(AppError::unauthorized)
}

async fn org_config(State(s): State<AppState>, ctx: AuthCtx) -> AppResult<ApiJson<OrgConfigResponse>> {
    let org = caller_org(&s, &ctx).await?;
    let (version, config) = orgs::config(&s.db, &s.env, &org).await?;
    Ok(ApiJson(OrgConfigResponse { version, config }))
}

async fn providers(State(s): State<AppState>, ctx: AuthCtx) -> AppResult<ApiJson<Vec<ProviderOption>>> {
    let org = caller_org(&s, &ctx).await?;
    let (_, config) = orgs::config(&s.db, &s.env, &org).await?;
    let secrets = orgs::secrets(&s.db, &s.env, &org.id).await?;
    Ok(ApiJson(providers_for(&config, !secrets.google_client_secret.is_empty())))
}

#[derive(Deserialize)]
struct GoogleTokenBody {
    access_token: String,
    expires_in: i64,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

/// The org's Google app and client secret, when the server talks to
/// Google's token endpoint for it (only with a secret set; otherwise clients
/// use PKCE with an installed-app client directly).
async fn google_app(s: &AppState, ctx: &AuthCtx) -> AppResult<(String, Redacted<String>)> {
    let org = caller_org(s, ctx).await?;
    let (_, config) = orgs::config(&s.db, &s.env, &org).await?;
    let secrets = orgs::secrets(&s.db, &s.env, &org.id).await?;
    let secret = Redacted::new(secrets.google_client_secret.clone());
    let google = &config.providers;
    if !google.google || google.google_app.client_id.is_empty() {
        return Err(AppError::new(ErrorCode::ProviderNotAllowed, "Your organisation doesn't allow Google mailboxes."));
    }
    if secret.expose().is_empty() {
        return Err(AppError::new(ErrorCode::NotFound, "This organisation doesn't exchange Google codes on the server."));
    }
    Ok((google.google_app.client_id.clone(), secret))
}

/// POST the form to Google's token endpoint; the status and the body.
/// Neither the form nor the answer is ever logged.
async fn google_post(s: &AppState, form: &[(&str, &str)]) -> AppResult<(reqwest::StatusCode, Option<GoogleTokenBody>, String)> {
    let res = reqwest::Client::new()
        .post(&s.cfg.google_token_url)
        .form(form)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|_| AppError::new(ErrorCode::Unavailable, "Google didn't answer."))?;
    let status = res.status();
    let text = Redacted::new(res.text().await.unwrap_or_default());
    if !status.is_success() {
        // Only Google's error code (`invalid_grant`, …) is kept.
        let code = serde_json::from_str::<serde_json::Value>(text.expose())
            .ok()
            .and_then(|v| v["error"].as_str().map(String::from))
            .filter(|c| c.len() <= 40 && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
            .unwrap_or_default();
        return Ok((status, None, code));
    }
    let body = serde_json::from_str::<GoogleTokenBody>(text.expose())
        .map_err(|_| AppError::new(ErrorCode::Unavailable, "Google sent an unexpected answer."))?;
    Ok((status, Some(body), String::new()))
}

fn google_tokens(body: GoogleTokenBody) -> GoogleTokens {
    GoogleTokens { access_token: body.access_token, expires_in: body.expires_in, refresh_token: body.refresh_token, id_token: body.id_token }
}

/// Code exchange with the org's Google client secret ("Web" client type).
/// Only offered when the org configured a secret; otherwise clients use PKCE
/// with an installed-app client directly.
async fn google_token(State(s): State<AppState>, ctx: AuthCtx, ApiJson(req): ApiJson<GoogleExchange>) -> AppResult<ApiJson<GoogleTokens>> {
    let code = Redacted::new(req.code);
    let verifier = Redacted::new(req.code_verifier);
    let (client_id, secret) = google_app(&s, &ctx).await?;
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code.expose().as_str()),
        ("code_verifier", verifier.expose().as_str()),
        ("redirect_uri", req.redirect_uri.as_str()),
        ("client_id", client_id.as_str()),
        ("client_secret", secret.expose().as_str()),
    ];
    let (status, body, _) = google_post(&s, &params).await?;
    audit::log(
        &s.db,
        Event::new("oauth.google.exchange", Actor::User(&ctx.user_id))
            .org(&ctx.org_id)
            .target(&ctx.device_id)
            .meta(json!({"status": status.as_u16()})),
    )
    .await;
    let body = body.ok_or_else(|| AppError::new(ErrorCode::InvalidCode, "Google refused the sign-in code."))?;
    Ok(ApiJson(google_tokens(body)))
}

/// A new Google access token from a refresh token, with the org's client
/// secret: a "Web" client (the one the server exchanges codes for) can't
/// refresh without it. Rate-limited per user; tokens are never logged.
async fn google_refresh(State(s): State<AppState>, ctx: AuthCtx, ApiJson(req): ApiJson<GoogleRefresh>) -> AppResult<ApiJson<GoogleTokens>> {
    let refresh = Redacted::new(req.refresh_token);
    if refresh.expose().is_empty() || refresh.expose().len() > 2048 {
        return Err(AppError::bad_request("Send the account's refresh token."));
    }
    s.limit_google_refresh(&ctx.user_id)?;
    let (client_id, secret) = google_app(&s, &ctx).await?;
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh.expose().as_str()),
        ("client_id", client_id.as_str()),
        ("client_secret", secret.expose().as_str()),
    ];
    let (status, body, google_error) = google_post(&s, &params).await?;
    audit::log(
        &s.db,
        Event::new("oauth.google.refresh", Actor::User(&ctx.user_id))
            .org(&ctx.org_id)
            .target(&ctx.device_id)
            .meta(json!({"status": status.as_u16()})),
    )
    .await;
    match body {
        Some(b) => Ok(ApiJson(google_tokens(b))),
        // Revoked or expired: the user signs in to Google again.
        None if status.is_client_error() => {
            let why = if google_error.is_empty() { "invalid_grant".to_string() } else { google_error };
            Err(AppError::new(ErrorCode::InvalidToken, format!("Google refused the sign-in ({why}).")))
        }
        None => Err(AppError::new(ErrorCode::Unavailable, "Google couldn't refresh the sign-in right now.")),
    }
}

/// Debug builds with `REMAIL_DEV_MAIL=1`: mail the server "sent".
#[cfg(debug_assertions)]
async fn dev_mail(State(s): State<AppState>) -> AppResult<ApiJson<Vec<crate::mailer::Mail>>> {
    if !s.cfg.dev_mail {
        return Err(AppError::not_found());
    }
    Ok(ApiJson(s.mailer.dev_outbox()))
}
