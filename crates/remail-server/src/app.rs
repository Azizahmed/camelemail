//! Shared state, the `/v1` router and the HTTP server.

use std::net::{IpAddr, SocketAddr};
use std::ops::Deref;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts};
use axum::http::request::Parts;
use axum::http::Request;
use axum::Router;
use remail_proto::ErrorCode;
use sqlx::PgPool;
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::crypto::{Envelope, KeyProvider};
use crate::error::AppError;
use crate::mailer::Mailer;
use crate::notices::Hub;
use crate::ratelimit::{Limiter, Lockouts};

pub struct Inner {
    pub cfg: Config,
    pub db: PgPool,
    pub env: Envelope,
    pub mailer: Mailer,
    pub hub: Arc<Hub>,
    ip_limiter: Limiter,
    account_limiter: Limiter,
    /// Google refreshes through the server, per user: one per mailbox an
    /// hour normally; a burst covers a start with many mailboxes.
    google_limiter: Limiter,
    /// Admin routes and the org AI key, per user.
    user_limiter: Limiter,
    pub lockouts: Lockouts,
    /// A real argon2 hash of a random password, verified against for unknown
    /// e-mails so they take as long as known ones.
    pub dummy_hash: String,
}

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

impl Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    /// Builds the state and starts the notice listener. Run
    /// [`crate::orgs::startup`] with its envelope before serving.
    pub async fn new(cfg: Config, db: PgPool, keys: Arc<dyn KeyProvider>) -> Result<Self> {
        let env = Envelope::new(keys);
        let mailer = Mailer::new(&cfg)?;
        let hub = Arc::new(Hub::new());
        hub.clone().spawn_listener(db.clone());
        let dummy_hash = crate::auth::password::dummy_hash(&cfg).await?;
        let r = cfg.rate;
        Ok(AppState(Arc::new(Inner {
            ip_limiter: Limiter::new(r.ip_burst, r.ip_per_sec),
            account_limiter: Limiter::new(r.account_burst, r.account_per_sec),
            google_limiter: Limiter::new(r.ip_burst.max(30), r.ip_per_sec.max(0.5)),
            user_limiter: Limiter::new(r.ip_burst.max(60), r.ip_per_sec.max(1.0)),
            lockouts: Lockouts::new(cfg.lockout_base, cfg.lockout_max),
            cfg,
            db,
            env,
            mailer,
            hub,
            dummy_hash,
        })))
    }

    /// Per-IP budget for unauthenticated auth routes.
    pub fn limit_ip(&self, ip: Option<IpAddr>) -> Result<(), AppError> {
        let key = ip.map(|i| i.to_string()).unwrap_or_else(|| "unknown".into());
        self.ip_limiter
            .check(&key)
            .map_err(|secs| AppError::retry_after(ErrorCode::RateLimited, "Too many requests. Try again shortly.", secs))
    }

    /// Per-user budget for Google refreshes through the server.
    pub fn limit_google_refresh(&self, user_id: &str) -> Result<(), AppError> {
        self.google_limiter
            .check(user_id)
            .map_err(|secs| AppError::retry_after(ErrorCode::RateLimited, "Too many requests. Try again shortly.", secs))
    }

    /// Per-user budget for admin routes and the org AI key.
    pub fn limit_user(&self, user_id: &str) -> Result<(), AppError> {
        self.user_limiter
            .check(user_id)
            .map_err(|secs| AppError::retry_after(ErrorCode::RateLimited, "Too many requests. Try again shortly.", secs))
    }

    /// Per-account budget (by e-mail), on top of the lockout.
    pub fn limit_account(&self, email: &str) -> Result<(), AppError> {
        self.account_limiter
            .check(email)
            .map_err(|secs| AppError::retry_after(ErrorCode::RateLimited, "Too many requests. Try again shortly.", secs))
    }
}

/// The caller's IP: the socket peer, or the first `X-Forwarded-For` hop when
/// `REMAIL_TRUST_PROXY=1`.
pub struct ClientIp(pub Option<IpAddr>);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        if state.cfg.trust_proxy {
            let forwarded = parts
                .headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next())
                .and_then(|v| v.trim().parse::<IpAddr>().ok());
            if forwarded.is_some() {
                return Ok(ClientIp(forwarded));
            }
        }
        let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
        Ok(ClientIp(peer))
    }
}

/// Sections are capped at 256 KiB; leave room for JSON framing.
const BODY_LIMIT: usize = 1024 * 1024;

pub fn router(state: AppState) -> Router {
    let v1 = Router::new()
        .merge(crate::auth::routes())
        .merge(crate::sections::routes())
        .merge(crate::org_api::routes())
        .merge(crate::notices::routes())
        .merge(crate::admin::routes());
    Router::new()
        .nest("/v1", v1)
        .merge(crate::web::routes())
        .route("/health", axum::routing::get(|| async { "ok" }))
        .route("/", axum::routing::get(|| async { "Camel Mail server. Sign in from the Camel Mail app.\n" }))
        .fallback(|| async { AppError::not_found() })
        .method_not_allowed_fallback(|| async { AppError::new(ErrorCode::BadRequest, "Method not allowed.") })
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        // Method and path only: no query strings, headers or bodies.
        .layer(TraceLayer::new_for_http().make_span_with(|req: &Request<_>| {
            tracing::info_span!("http", method = %req.method(), path = %req.uri().path())
        }))
        .with_state(state)
}

/// Serves until `shutdown` resolves.
pub async fn serve(
    state: AppState,
    listener: tokio::net::TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let app = router(state);
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

/// Hourly: drop expired one-time tokens and long-dead sessions.
pub fn spawn_housekeeping(db: PgPool) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            tick.tick().await;
            if db.is_closed() {
                return;
            }
            let _ = sqlx::query("delete from auth_tokens where expires_at < now() - interval '1 day'").execute(&db).await;
            let _ = sqlx::query("delete from sessions where expires_at < now() - interval '1 day'").execute(&db).await;
        }
    });
}
