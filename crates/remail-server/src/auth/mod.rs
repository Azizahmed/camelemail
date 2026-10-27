//! Accounts and sign-in (design §7.2): sign-up, e-mail verification, login
//! with lockout, TOTP and backup codes, refresh rotation, password reset,
//! profile, devices and push registration.
//!
//! Every security event writes an audit row (IDs only). Unknown e-mails get
//! the same answers, timing and lockout as real ones.

pub mod password;
pub mod session;
pub mod tokens;
pub mod totp;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::Router;
use chrono::{DateTime, Utc};
use remail_proto::api::*;
use remail_proto::org::Policies;
use remail_proto::ErrorCode;
use serde_json::json;
use sqlx::FromRow;

use crate::app::{AppState, ClientIp};
use crate::audit::{self, Actor, Event};
use crate::config::Mode;
use crate::crypto::{open, seal, section_aad};
use crate::error::{ApiJson, ApiPath, AppError, AppResult};
use crate::mailer;
use crate::orgs::{self, Org};
use crate::ratelimit::lockout_for;
use crate::redact::Redacted;
pub use session::AuthCtx;
use tokens::NewOneTime;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/signup", post(signup))
        .route("/auth/verify", post(verify))
        .route("/auth/login", post(login))
        .route("/auth/login/2fa", post(login_2fa))
        .route("/auth/refresh", post(refresh))
        .route("/auth/logout", post(logout))
        .route("/auth/reset/request", post(reset_request))
        .route("/auth/reset", post(reset))
        .route("/me", get(me_get).patch(me_patch))
        .route("/me/password", post(change_password))
        .route("/me/2fa/setup", post(totp_setup))
        .route("/me/2fa/enable", post(totp_enable))
        .route("/me/2fa/disable", post(totp_disable))
        .route("/me/devices", get(devices))
        .route("/me/devices/{id}", delete(revoke_device))
        .route("/me/devices/current/push", put(push_register))
}

pub fn normalize_email(e: &str) -> String {
    e.trim().to_lowercase()
}

fn valid_email(e: &str) -> bool {
    let Some((local, domain)) = e.split_once('@') else { return false };
    e.len() <= 254
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && !e.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn clean_name(name: &str) -> AppResult<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 100 {
        return Err(AppError::bad_request("Enter a name (up to 100 characters)."));
    }
    Ok(n.to_string())
}

#[derive(Debug, Clone, FromRow)]
pub struct UserRow {
    pub id: String,
    pub org_id: String,
    pub email: String,
    pub name: String,
    pub role: String,
    pub locale: String,
    pub password_hash: String,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub totp_secret_enc: Option<Vec<u8>>,
    pub totp_enabled_at: Option<DateTime<Utc>>,
    pub totp_last_step: Option<i64>,
    pub disabled_at: Option<DateTime<Utc>>,
    pub failed_logins: i32,
    pub locked_until: Option<DateTime<Utc>>,
}

macro_rules! user_cols {
    () => {
        "id, org_id, email, name, role, locale, password_hash, email_verified_at, totp_secret_enc, \
         totp_enabled_at, totp_last_step, disabled_at, failed_logins, locked_until"
    };
}

async fn user_by_email(s: &AppState, email: &str) -> sqlx::Result<Option<UserRow>> {
    sqlx::query_as(concat!("select ", user_cols!(), " from users where email = $1"))
        .bind(email)
        .fetch_optional(&s.db)
        .await
}

async fn user_by_id(s: &AppState, id: &str) -> sqlx::Result<Option<UserRow>> {
    sqlx::query_as(concat!("select ", user_cols!(), " from users where id = $1"))
        .bind(id)
        .fetch_optional(&s.db)
        .await
}

async fn current_user(s: &AppState, ctx: &AuthCtx) -> AppResult<UserRow> {
    user_by_id(s, &ctx.user_id).await?.ok_or_else(AppError::unauthorized)
}

async fn org_of(s: &AppState, user: &UserRow) -> AppResult<(Org, Policies)> {
    let org = orgs::by_id(&s.db, &user.org_id).await?.ok_or_else(AppError::unauthorized)?;
    let (_, cfg) = orgs::config(&s.db, &s.env, &org).await?;
    Ok((org, cfg.policies))
}

pub async fn me_for(s: &AppState, user: &UserRow) -> AppResult<Me> {
    let (org, policies) = org_of(s, user).await?;
    Ok(Me {
        id: user.id.clone(),
        name: user.name.clone(),
        email: user.email.clone(),
        role: user.role.clone(),
        org_id: org.id,
        org_name: org.name,
        totp: user.totp_enabled_at.is_some(),
        policies,
    })
}

fn invalid_credentials() -> AppError {
    AppError::new(ErrorCode::InvalidCredentials, "Wrong e-mail or password.")
}

fn locked(secs: u64) -> AppError {
    AppError::retry_after(ErrorCode::AccountLocked, "Too many failed attempts. Try again later.", secs)
}

fn invalid_code() -> AppError {
    AppError::new(ErrorCode::InvalidCode, "That code isn't right.")
}

// ---------- sign-up ----------

async fn signup(
    State(s): State<AppState>,
    ClientIp(ip): ClientIp,
    ApiJson(req): ApiJson<SignupRequest>,
) -> AppResult<(StatusCode, ApiJson<SignupResponse>)> {
    s.limit_ip(ip)?;
    let email = normalize_email(&req.email);
    if !valid_email(&email) {
        return Err(AppError::bad_request("Enter a valid e-mail address."));
    }
    let name = clean_name(&req.name)?;
    let locale = match req.locale.as_deref() {
        Some(l) if l.starts_with("ar") => "ar",
        _ => "en",
    };
    let password = Redacted::new(req.password);
    let invite = req.invite.map(Redacted::new);

    // Which org, which role.
    let (org, role, invite_hash) = match &invite {
        Some(t) => {
            let bad = || AppError::new(ErrorCode::InvalidToken, "This invite is not valid or has expired.");
            let inv = tokens::find(&s.db, tokens::INVITE, t.expose()).await?.ok_or_else(bad)?;
            if inv.email.as_deref().is_some_and(|e| e != email) {
                return Err(AppError::new(ErrorCode::InvalidToken, "This invite is for another e-mail address."));
            }
            let org = orgs::by_id(&s.db, &inv.org_id).await?.ok_or_else(bad)?;
            (org, inv.role.unwrap_or_else(|| "user".into()), Some(inv.hash))
        }
        None if s.cfg.mode == Mode::Onprem => {
            return Err(AppError::new(ErrorCode::InviteRequired, "Ask your administrator for an invite."));
        }
        None if !s.mailer.configured() => {
            return Err(AppError::new(
                ErrorCode::InviteRequired,
                "This server can't send e-mail yet, so it only accepts invitations.",
            ));
        }
        None => match orgs::by_slug(&s.db, orgs::PUBLIC_SLUG).await? {
            Some(org) if org.signup_mode == "open" => (org, "user".to_string(), None),
            _ => return Err(AppError::new(ErrorCode::SignupClosed, "Sign-up is closed.")),
        },
    };
    if org.disabled_at.is_some() {
        return Err(AppError::new(ErrorCode::SignupClosed, "Sign-up is closed."));
    }
    if !org.allows_email(&email) {
        return Err(AppError::new(ErrorCode::DomainNotAllowed, "This organisation doesn't accept that e-mail domain."));
    }
    if !matches!(role.as_str(), "user" | "admin" | "owner") {
        return Err(AppError::bad_request("Unknown role."));
    }
    // Hosted: the operator's addresses own the public org.
    let role = if s.cfg.mode == Mode::Hosted && org.slug == orgs::PUBLIC_SLUG && s.cfg.is_admin_email(&email) {
        "owner".to_string()
    } else {
        role
    };
    password::check_strength(password.expose(), &email, &name)?;

    if let Some(existing) = user_by_email(&s, &email).await? {
        if let Some(h) = &invite_hash {
            // An address nobody ever confirmed has no owner yet: it can't sign
            // in and holds no data, so an invitation for it takes it over.
            if existing.email_verified_at.is_none() && existing.org_id == org.id {
                claim_unconfirmed(&s, ip, &existing, h, &name, &role, locale, password.expose()).await?;
                return Ok((StatusCode::ACCEPTED, ApiJson(SignupResponse { verify: "none".into() })));
            }
            return Err(AppError::new(ErrorCode::EmailTaken, "There's already an account with this e-mail."));
        }
        // Open sign-up doesn't reveal which addresses have accounts: the
        // owner gets an e-mail instead.
        s.mailer.send(mailer::already_registered_mail(locale, &s.cfg.public_url, &email)).await;
        return Ok((StatusCode::ACCEPTED, ApiJson(SignupResponse { verify: "email".into() })));
    }
    crate::licence::check_seat(&s.cfg, &s.db, &s.env).await?;

    let hash = password::hash(&s.cfg, password.expose()).await?;
    let user_id = remail_proto::new_id("usr");
    let verified = invite_hash.is_some();
    let mut tx = s.db.begin().await?;
    let inserted = sqlx::query(
        "insert into users (id, org_id, email, name, role, locale, password_hash, email_verified_at)
         values ($1, $2, $3, $4, $5, $6, $7, case when $8 then now() end)",
    )
    .bind(&user_id)
    .bind(&org.id)
    .bind(&email)
    .bind(&name)
    .bind(&role)
    .bind(locale)
    .bind(&hash)
    .bind(verified)
    .execute(&mut *tx)
    .await;
    if let Err(e) = inserted {
        if e.as_database_error().is_some_and(|d| d.is_unique_violation()) {
            return Err(AppError::new(ErrorCode::EmailTaken, "There's already an account with this e-mail."));
        }
        return Err(e.into());
    }
    s.env.create_key(&mut tx, crate::crypto::OwnerKind::User, &user_id, &org.id).await?;
    if let Some(h) = &invite_hash {
        if !tokens::consume(&mut *tx, h).await? {
            return Err(AppError::new(ErrorCode::InvalidToken, "This invite was already used."));
        }
    }
    let verify_token = if verified {
        None
    } else {
        let t = tokens::issue(
            &mut *tx,
            NewOneTime {
                kind: tokens::VERIFY,
                user_id: Some(&user_id),
                org_id: &org.id,
                email: Some(&email),
                role: None,
                meta: json!({}),
            },
        )
        .await?;
        Some(t)
    };
    tx.commit().await?;
    if let Some(t) = verify_token {
        s.mailer.send(mailer::verify_mail(locale, &s.cfg.public_url, &email, t.expose())).await;
    }
    audit::log(
        &s.db,
        Event::new("auth.signup", Actor::User(&user_id))
            .org(&org.id)
            .target(&user_id)
            .ip(ip)
            .meta(json!({"invite": verified, "role": role})),
    )
    .await;
    let verify = if verified { "none" } else { "email" };
    Ok((StatusCode::ACCEPTED, ApiJson(SignupResponse { verify: verify.into() })))
}

/// An invitation used on an address whose account was never confirmed: the
/// account gets the invitation's role, the new name and password, and counts
/// as confirmed (the invitation stands in for the e-mail check).
#[allow(clippy::too_many_arguments)]
async fn claim_unconfirmed(
    s: &AppState,
    ip: Option<std::net::IpAddr>,
    user: &UserRow,
    invite_hash: &[u8],
    name: &str,
    role: &str,
    locale: &str,
    password: &str,
) -> AppResult<()> {
    crate::licence::check_seat(&s.cfg, &s.db, &s.env).await?;
    let hash = password::hash(&s.cfg, password).await?;
    let mut tx = s.db.begin().await?;
    if !tokens::consume(&mut *tx, invite_hash).await? {
        return Err(AppError::new(ErrorCode::InvalidToken, "This invite was already used."));
    }
    let updated = sqlx::query(
        "update users set name = $2, role = $3, locale = $4, password_hash = $5, email_verified_at = now(),
           failed_logins = 0, locked_until = null
         where id = $1 and email_verified_at is null",
    )
    .bind(&user.id)
    .bind(name)
    .bind(role)
    .bind(locale)
    .bind(&hash)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AppError::new(ErrorCode::EmailTaken, "There's already an account with this e-mail."));
    }
    // The old confirmation link must not work any more.
    sqlx::query("update auth_tokens set used_at = now() where user_id = $1 and kind = $2 and used_at is null")
        .bind(&user.id)
        .bind(tokens::VERIFY)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    session::revoke_user_devices(s, &user.id, None).await?;
    audit::log(
        &s.db,
        Event::new("auth.signup.claim", Actor::User(&user.id))
            .org(&user.org_id)
            .target(&user.id)
            .ip(ip)
            .meta(json!({"invite": true, "role": role})),
    )
    .await;
    Ok(())
}

async fn verify(State(s): State<AppState>, ClientIp(ip): ClientIp, ApiJson(req): ApiJson<TokenRequest>) -> AppResult<StatusCode> {
    verify_email_token(&s, ip, req.token).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Confirms an e-mail address (the app's API and the web page share it).
pub(crate) async fn verify_email_token(s: &AppState, ip: Option<std::net::IpAddr>, token: String) -> AppResult<()> {
    s.limit_ip(ip)?;
    let token = Redacted::new(token);
    let bad = || AppError::new(ErrorCode::InvalidToken, "This link is not valid or has expired.");
    let t = tokens::find(&s.db, tokens::VERIFY, token.expose()).await?.ok_or_else(bad)?;
    let user_id = t.user_id.clone().ok_or_else(bad)?;
    let mut tx = s.db.begin().await?;
    if !tokens::consume(&mut *tx, &t.hash).await? {
        return Err(bad());
    }
    sqlx::query("update users set email_verified_at = coalesce(email_verified_at, now()) where id = $1")
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    audit::log(&s.db, Event::new("auth.verify", Actor::User(&user_id)).org(&t.org_id).target(&user_id).ip(ip)).await;
    Ok(())
}

// ---------- sign-in ----------

/// Platform policy and licence: may this user sign in a new device now?
async fn precheck(s: &AppState, user: &UserRow, policies: &Policies, platform: &str) -> AppResult<()> {
    if !policies.allows_platform(platform) {
        return Err(AppError::new(ErrorCode::PlatformNotAllowed, "Your organisation doesn't allow this kind of device."));
    }
    let lic = crate::licence::status(&s.cfg, &s.db, &s.env).await?;
    // Admins can still sign in to install a licence.
    if lic.blocks_sign_in() && !matches!(user.role.as_str(), "admin" | "owner") {
        return Err(lic.sign_in_error());
    }
    Ok(())
}

/// Registers the device and opens its first session.
async fn sign_in(s: &AppState, user: &UserRow, device: &DeviceInfo, ip: Option<std::net::IpAddr>) -> AppResult<Tokens> {
    let (org, policies) = org_of(s, user).await?;
    if user.disabled_at.is_some() || org.disabled_at.is_some() {
        return Err(AppError::new(ErrorCode::AccountDisabled, "This account is disabled."));
    }
    precheck(s, user, &policies, &device.platform).await?;
    let mut tx = s.db.begin().await?;
    let device_id = session::create_device(&mut tx, &user.id, device).await?;
    let issued = session::issue(s, &mut tx, &user.id, &device_id, None, policies.session_days).await?;
    sqlx::query("update users set last_active_at = now() where id = $1").bind(&user.id).execute(&mut *tx).await?;
    tx.commit().await?;
    audit::log(
        &s.db,
        Event::new("auth.login", Actor::User(&user.id))
            .org(&user.org_id)
            .target(&device_id)
            .ip(ip)
            .meta(json!({"platform": device.platform})),
    )
    .await;
    Ok(Tokens {
        access_token: issued.access.expose().clone(),
        access_expires_at: issued.access_expires_at.timestamp_millis(),
        refresh_token: issued.refresh.expose().clone(),
        device_id,
        user: me_for(s, user).await?,
    })
}

/// Counts a wrong password or 2FA code; locks the account progressively.
async fn count_failure(s: &AppState, user: &UserRow, ip: Option<std::net::IpAddr>, action: &str) -> AppResult<()> {
    let fails: i32 = sqlx::query_scalar("update users set failed_logins = failed_logins + 1 where id = $1 returning failed_logins")
        .bind(&user.id)
        .fetch_one(&s.db)
        .await?;
    let lock = lockout_for(fails, s.cfg.lockout_base, s.cfg.lockout_max);
    if let Some(d) = lock {
        sqlx::query("update users set locked_until = now() + make_interval(secs => $2) where id = $1")
            .bind(&user.id)
            .bind(d.as_secs_f64())
            .execute(&s.db)
            .await?;
    }
    audit::log(
        &s.db,
        Event::new(action, Actor::User(&user.id))
            .org(&user.org_id)
            .target(&user.id)
            .ip(ip)
            .meta(json!({"failures": fails, "locked": lock.map(|d| d.as_secs())})),
    )
    .await;
    Ok(())
}

async fn login(
    State(s): State<AppState>,
    ClientIp(ip): ClientIp,
    ApiJson(req): ApiJson<LoginRequest>,
) -> AppResult<ApiJson<LoginResponse>> {
    s.limit_ip(ip)?;
    let email = normalize_email(&req.email);
    s.limit_account(&email)?;
    session::check_device(&req.device)?;
    let password = Redacted::new(req.password);

    let Some(user) = user_by_email(&s, &email).await? else {
        // Same work, same answers, same lockout as a real account.
        if let Some(left) = s.lockouts.locked(&email) {
            return Err(locked(left));
        }
        let _ = password::verify(password.expose(), &s.dummy_hash).await;
        s.lockouts.fail(&email);
        audit::log(&s.db, Event::new("auth.login.fail", Actor::System).ip(ip).meta(json!({"reason": "unknown_email"}))).await;
        return Err(invalid_credentials());
    };
    if let Some(until) = user.locked_until.filter(|u| *u > Utc::now()) {
        return Err(locked((until - Utc::now()).num_seconds().max(1) as u64));
    }
    if !password::verify(password.expose(), &user.password_hash).await {
        count_failure(&s, &user, ip, "auth.login.fail").await?;
        return Err(invalid_credentials());
    }
    if user.failed_logins > 0 || user.locked_until.is_some() {
        sqlx::query("update users set failed_logins = 0, locked_until = null where id = $1")
            .bind(&user.id)
            .execute(&s.db)
            .await?;
    }
    // Only now, with the right password, say why sign-in can't proceed.
    let (org, policies) = org_of(&s, &user).await?;
    if user.disabled_at.is_some() || org.disabled_at.is_some() {
        return Err(AppError::new(ErrorCode::AccountDisabled, "This account is disabled."));
    }
    if s.cfg.mode == Mode::Hosted && user.email_verified_at.is_none() {
        return Err(AppError::new(ErrorCode::EmailNotVerified, "Confirm your e-mail address first."));
    }
    precheck(&s, &user, &policies, &req.device.platform).await?;

    let needs_setup = user.totp_enabled_at.is_none() && policies.require_2fa;
    if user.totp_enabled_at.is_some() || needs_setup {
        let kind = if needs_setup { tokens::TWO_FACTOR_SETUP } else { tokens::TWO_FACTOR };
        let challenge = tokens::issue(
            &s.db,
            NewOneTime {
                kind,
                user_id: Some(&user.id),
                org_id: &user.org_id,
                email: None,
                role: None,
                meta: json!({"device": req.device}),
            },
        )
        .await?;
        audit::log(&s.db, Event::new("auth.login.challenge", Actor::User(&user.id)).org(&user.org_id).target(&user.id).ip(ip)).await;
        let methods = if needs_setup { vec!["totp".into()] } else { vec!["totp".into(), "backup".into()] };
        return Ok(ApiJson(LoginResponse::Challenge(Challenge {
            challenge: challenge.expose().clone(),
            methods,
            setup_required: needs_setup,
        })));
    }
    Ok(ApiJson(LoginResponse::Tokens(sign_in(&s, &user, &req.device, ip).await?)))
}

fn totp_aad(user_id: &str) -> Vec<u8> {
    section_aad(user_id, "totp", 0)
}

async fn totp_secret(s: &AppState, user: &UserRow) -> AppResult<Option<zeroize::Zeroizing<Vec<u8>>>> {
    let Some(enc) = &user.totp_secret_enc else { return Ok(None) };
    let dk = s.env.org_key(&s.db, &user.org_id).await?;
    Ok(Some(open(dk.bytes(), &totp_aad(&user.id), enc)?))
}

/// Checks a TOTP code (recording its step against replay) or uses up a
/// backup code.
async fn second_factor(s: &AppState, user: &UserRow, code: &str) -> AppResult<bool> {
    if user.totp_enabled_at.is_none() {
        return Ok(false);
    }
    if totp::is_totp_code(code) {
        let Some(secret) = totp_secret(s, user).await? else { return Ok(false) };
        let now = Utc::now().timestamp().max(0) as u64;
        let Some(step) = totp::verify(&secret, code, now, user.totp_last_step) else { return Ok(false) };
        let r = sqlx::query(
            "update users set totp_last_step = $2 where id = $1 and (totp_last_step is null or totp_last_step < $2)",
        )
        .bind(&user.id)
        .bind(step)
        .execute(&s.db)
        .await?;
        return Ok(r.rows_affected() == 1);
    }
    let r = sqlx::query("update backup_codes set used_at = now() where user_id = $1 and code_hash = $2 and used_at is null")
        .bind(&user.id)
        .bind(totp::backup_hash(&user.id, code))
        .execute(&s.db)
        .await?;
    if r.rows_affected() == 1 {
        audit::log(&s.db, Event::new("auth.2fa.backup_used", Actor::User(&user.id)).org(&user.org_id).target(&user.id)).await;
        return Ok(true);
    }
    Ok(false)
}

fn challenge_expired() -> AppError {
    AppError::new(ErrorCode::InvalidToken, "The sign-in expired. Sign in again.")
}

fn challenge_device(t: &tokens::OneTime) -> AppResult<DeviceInfo> {
    serde_json::from_value(t.meta.get("device").cloned().unwrap_or_default()).map_err(|_| challenge_expired())
}

async fn login_2fa(
    State(s): State<AppState>,
    ClientIp(ip): ClientIp,
    ApiJson(req): ApiJson<TwoFactorRequest>,
) -> AppResult<ApiJson<Tokens>> {
    s.limit_ip(ip)?;
    let challenge = Redacted::new(req.challenge);
    let code = Redacted::new(req.code);
    let ch = tokens::find(&s.db, tokens::TWO_FACTOR, challenge.expose()).await?.ok_or_else(challenge_expired)?;
    let user_id = ch.user_id.clone().ok_or_else(challenge_expired)?;
    let user = user_by_id(&s, &user_id).await?.ok_or_else(challenge_expired)?;
    if user.disabled_at.is_some() {
        return Err(AppError::new(ErrorCode::AccountDisabled, "This account is disabled."));
    }
    if let Some(until) = user.locked_until.filter(|u| *u > Utc::now()) {
        return Err(locked((until - Utc::now()).num_seconds().max(1) as u64));
    }
    if !second_factor(&s, &user, code.expose()).await? {
        // Wrong codes burn the challenge after five tries and count towards
        // the account lockout like wrong passwords.
        tokens::wrong_attempt(&s.db, &ch.hash).await?;
        count_failure(&s, &user, ip, "auth.2fa.fail").await?;
        return Err(invalid_code());
    }
    sqlx::query("update users set failed_logins = 0, locked_until = null where id = $1 and failed_logins > 0")
        .bind(&user.id)
        .execute(&s.db)
        .await?;
    if !tokens::consume(&s.db, &ch.hash).await? {
        return Err(challenge_expired());
    }
    let device = challenge_device(&ch)?;
    Ok(ApiJson(sign_in(&s, &user, &device, ip).await?))
}

#[derive(Debug, FromRow)]
struct SessionRow {
    id: String,
    device_id: String,
    user_id: String,
    family: String,
    expires_at: DateTime<Utc>,
    rotated_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    device_revoked_at: Option<DateTime<Utc>>,
}

async fn refresh(
    State(s): State<AppState>,
    ClientIp(ip): ClientIp,
    ApiJson(req): ApiJson<RefreshRequest>,
) -> AppResult<ApiJson<Tokens>> {
    s.limit_ip(ip)?;
    let token = Redacted::new(req.refresh_token);
    let row: Option<SessionRow> = sqlx::query_as(
        "select s.id, s.device_id, s.user_id, s.family, s.expires_at, s.rotated_at, s.revoked_at,
                d.revoked_at as device_revoked_at
         from sessions s join devices d on d.id = s.device_id where s.refresh_hash = $1",
    )
    .bind(tokens::hash(token.expose()))
    .fetch_optional(&s.db)
    .await?;
    let Some(row) = row else { return Err(AppError::unauthorized()) };
    if row.revoked_at.is_some() || row.device_revoked_at.is_some() || row.expires_at <= Utc::now() {
        return Err(AppError::unauthorized());
    }
    let reuse = |s: AppState, row: SessionRow| async move {
        // A rotated token came back: somebody has a copy. Sign the device out.
        session::revoke_family(&s, &row.family).await?;
        session::revoke_device(&s, &row.device_id).await?;
        audit::log(
            &s.db,
            Event::new("auth.refresh.reuse", Actor::User(&row.user_id)).target(&row.device_id).ip(ip),
        )
        .await;
        Err::<ApiJson<Tokens>, AppError>(AppError::unauthorized())
    };
    let user = user_by_id(&s, &row.user_id).await?.ok_or_else(AppError::unauthorized)?;
    let (org, policies) = org_of(&s, &user).await?;
    if user.disabled_at.is_some() || org.disabled_at.is_some() {
        return Err(AppError::new(ErrorCode::AccountDisabled, "This account is disabled."));
    }
    let mut tx = s.db.begin().await?;
    let taken = if row.rotated_at.is_none() {
        sqlx::query("update sessions set rotated_at = now() where id = $1 and rotated_at is null and revoked_at is null")
            .bind(&row.id)
            .execute(&mut *tx)
            .await?
    } else {
        // Refreshed already, but the session that made was never used: the
        // reply didn't reach the device (an app frozen or killed mid-request,
        // a network switch). Replace that session rather than sign the device
        // out. Once the device has used it, this token coming back is reuse.
        sqlx::query(
            "update sessions set revoked_at = now()
             where parent_id = $1 and rotated_at is null and revoked_at is null and access_used_at is null",
        )
        .bind(&row.id)
        .execute(&mut *tx)
        .await?
    };
    if taken.rows_affected() != 1 {
        drop(tx);
        return reuse(s, row).await;
    }
    let issued = session::issue(&s, &mut tx, &user.id, &row.device_id, Some(&row.family), policies.session_days).await?;
    sqlx::query("update sessions set parent_id = $1 where id = $2").bind(&row.id).bind(&issued.id).execute(&mut *tx).await?;
    sqlx::query("update devices set last_seen_at = now() where id = $1").bind(&row.device_id).execute(&mut *tx).await?;
    sqlx::query("update users set last_active_at = now() where id = $1").bind(&user.id).execute(&mut *tx).await?;
    tx.commit().await?;
    if row.rotated_at.is_some() {
        audit::log(&s.db, Event::new("auth.refresh.repeat", Actor::User(&row.user_id)).target(&row.device_id).ip(ip)).await;
    }
    Ok(ApiJson(Tokens {
        access_token: issued.access.expose().clone(),
        access_expires_at: issued.access_expires_at.timestamp_millis(),
        refresh_token: issued.refresh.expose().clone(),
        device_id: row.device_id.clone(),
        user: me_for(&s, &user).await?,
    }))
}

async fn logout(State(s): State<AppState>, ctx: AuthCtx) -> AppResult<StatusCode> {
    session::revoke_device(&s, &ctx.device_id).await?;
    audit::log(&s.db, Event::new("auth.logout", Actor::User(&ctx.user_id)).org(&ctx.org_id).target(&ctx.device_id)).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- password reset ----------

async fn reset_request(
    State(s): State<AppState>,
    ClientIp(ip): ClientIp,
    ApiJson(req): ApiJson<ResetRequest>,
) -> AppResult<StatusCode> {
    s.limit_ip(ip)?;
    let email = normalize_email(&req.email);
    s.limit_account(&email)?;
    if let Some(user) = user_by_email(&s, &email).await?.filter(|u| u.disabled_at.is_none()) {
        let t = tokens::issue(
            &s.db,
            NewOneTime {
                kind: tokens::RESET,
                user_id: Some(&user.id),
                org_id: &user.org_id,
                email: Some(&user.email),
                role: None,
                meta: json!({}),
            },
        )
        .await?;
        s.mailer.send(mailer::reset_mail(&user.locale, &s.cfg.public_url, &user.email, t.expose())).await;
        audit::log(&s.db, Event::new("auth.reset.request", Actor::User(&user.id)).org(&user.org_id).target(&user.id).ip(ip)).await;
    }
    // Same answer whether or not the address has an account.
    Ok(StatusCode::ACCEPTED)
}

async fn reset(State(s): State<AppState>, ClientIp(ip): ClientIp, ApiJson(req): ApiJson<ResetPassword>) -> AppResult<StatusCode> {
    reset_with_token(&s, ip, req.token, req.password).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Sets a new password from a reset link (the app's API and the web page share it).
pub(crate) async fn reset_with_token(s: &AppState, ip: Option<std::net::IpAddr>, token: String, password: String) -> AppResult<()> {
    s.limit_ip(ip)?;
    let token = Redacted::new(token);
    let password = Redacted::new(password);
    let bad = || AppError::new(ErrorCode::InvalidToken, "This link is not valid or has expired.");
    let t = tokens::find(&s.db, tokens::RESET, token.expose()).await?.ok_or_else(bad)?;
    let user = user_by_id(s, t.user_id.as_deref().unwrap_or("")).await?.ok_or_else(bad)?;
    password::check_strength(password.expose(), &user.email, &user.name)?;
    let hash = password::hash(&s.cfg, password.expose()).await?;
    let mut tx = s.db.begin().await?;
    if !tokens::consume(&mut *tx, &t.hash).await? {
        return Err(bad());
    }
    // The data key isn't derived from the password: settings stay intact.
    sqlx::query(
        "update users set password_hash = $2, failed_logins = 0, locked_until = null,
           email_verified_at = coalesce(email_verified_at, now()) where id = $1",
    )
    .bind(&user.id)
    .bind(&hash)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    session::revoke_user_devices(s, &user.id, None).await?;
    audit::log(&s.db, Event::new("auth.reset", Actor::User(&user.id)).org(&user.org_id).target(&user.id).ip(ip)).await;
    Ok(())
}

// ---------- profile ----------

async fn me_get(State(s): State<AppState>, ctx: AuthCtx) -> AppResult<ApiJson<Me>> {
    let user = current_user(&s, &ctx).await?;
    Ok(ApiJson(me_for(&s, &user).await?))
}

async fn me_patch(State(s): State<AppState>, ctx: AuthCtx, ApiJson(req): ApiJson<UpdateMe>) -> AppResult<ApiJson<Me>> {
    if let Some(name) = req.name {
        let name = clean_name(&name)?;
        sqlx::query("update users set name = $2 where id = $1").bind(&ctx.user_id).bind(&name).execute(&s.db).await?;
        audit::log(&s.db, Event::new("user.profile.update", Actor::User(&ctx.user_id)).org(&ctx.org_id).target(&ctx.user_id)).await;
    }
    let user = current_user(&s, &ctx).await?;
    Ok(ApiJson(me_for(&s, &user).await?))
}

async fn change_password(State(s): State<AppState>, ctx: AuthCtx, ApiJson(req): ApiJson<ChangePassword>) -> AppResult<StatusCode> {
    let current = Redacted::new(req.current);
    let new = Redacted::new(req.new);
    let user = current_user(&s, &ctx).await?;
    if !password::verify(current.expose(), &user.password_hash).await {
        audit::log(&s.db, Event::new("auth.password.fail", Actor::User(&user.id)).org(&user.org_id).target(&user.id)).await;
        return Err(invalid_credentials());
    }
    password::check_strength(new.expose(), &user.email, &user.name)?;
    let hash = password::hash(&s.cfg, new.expose()).await?;
    sqlx::query("update users set password_hash = $2 where id = $1").bind(&user.id).bind(&hash).execute(&s.db).await?;
    session::revoke_user_devices(&s, &user.id, Some(&ctx.device_id)).await?;
    audit::log(&s.db, Event::new("auth.password.change", Actor::User(&user.id)).org(&user.org_id).target(&user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- two-step verification ----------

/// The user a 2FA setup step is for: the signed-in caller, or the holder of a
/// required-setup sign-in challenge.
async fn setup_user(s: &AppState, ctx: Option<AuthCtx>, challenge: Option<&str>) -> AppResult<(UserRow, Option<tokens::OneTime>)> {
    if let Some(c) = challenge {
        let ch = tokens::find(&s.db, tokens::TWO_FACTOR_SETUP, c).await?.ok_or_else(challenge_expired)?;
        let user = user_by_id(s, ch.user_id.as_deref().unwrap_or("")).await?.ok_or_else(challenge_expired)?;
        return Ok((user, Some(ch)));
    }
    let ctx = ctx.ok_or_else(AppError::unauthorized)?;
    Ok((current_user(s, &ctx).await?, None))
}

async fn totp_setup(State(s): State<AppState>, ctx: Option<AuthCtx>, body: Bytes) -> AppResult<ApiJson<TotpSetup>> {
    // The body is optional: signed-in callers may send none at all.
    let req: TotpSetupRequest = if body.iter().all(|b| b.is_ascii_whitespace()) {
        TotpSetupRequest::default()
    } else {
        serde_json::from_slice(&body).map_err(|_| AppError::bad_request("Expected {\"challenge\"?: string}."))?
    };
    let challenge = req.challenge.map(Redacted::new);
    let (user, _) = setup_user(&s, ctx, challenge.as_ref().map(|c| c.expose().as_str())).await?;
    if user.totp_enabled_at.is_some() {
        return Err(AppError::new(ErrorCode::Conflict, "Two-step verification is already on."));
    }
    let secret = zeroize::Zeroizing::new(totp::new_secret());
    let dk = s.env.org_key(&s.db, &user.org_id).await?;
    let enc = seal(dk.bytes(), &totp_aad(&user.id), &secret);
    sqlx::query("update users set totp_secret_enc = $2, totp_last_step = null where id = $1 and totp_enabled_at is null")
        .bind(&user.id)
        .bind(enc)
        .execute(&s.db)
        .await?;
    Ok(ApiJson(TotpSetup { secret: totp::base32(&secret), otpauth_url: totp::otpauth_url(&secret, &user.email) }))
}

async fn totp_enable(
    State(s): State<AppState>,
    ClientIp(ip): ClientIp,
    ctx: Option<AuthCtx>,
    ApiJson(req): ApiJson<TotpEnable>,
) -> AppResult<ApiJson<BackupCodes>> {
    let challenge = req.challenge.map(Redacted::new);
    let code = Redacted::new(req.code);
    let (user, ch) = setup_user(&s, ctx, challenge.as_ref().map(|c| c.expose().as_str())).await?;
    if user.totp_enabled_at.is_some() {
        return Err(AppError::new(ErrorCode::Conflict, "Two-step verification is already on."));
    }
    let Some(secret) = totp_secret(&s, &user).await? else {
        return Err(AppError::bad_request("Start two-step setup first."));
    };
    let now = Utc::now().timestamp().max(0) as u64;
    let Some(step) = totp::verify(&secret, code.expose(), now, None) else {
        if let Some(ch) = &ch {
            tokens::wrong_attempt(&s.db, &ch.hash).await?;
        }
        return Err(invalid_code());
    };
    let codes = totp::new_backup_codes();
    let mut tx = s.db.begin().await?;
    if let Some(ch) = &ch {
        if !tokens::consume(&mut *tx, &ch.hash).await? {
            return Err(challenge_expired());
        }
    }
    sqlx::query("update users set totp_enabled_at = now(), totp_last_step = $2 where id = $1")
        .bind(&user.id)
        .bind(step)
        .execute(&mut *tx)
        .await?;
    sqlx::query("delete from backup_codes where user_id = $1").bind(&user.id).execute(&mut *tx).await?;
    for c in &codes {
        sqlx::query("insert into backup_codes (user_id, code_hash) values ($1, $2)")
            .bind(&user.id)
            .bind(totp::backup_hash(&user.id, c))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    audit::log(&s.db, Event::new("auth.2fa.enable", Actor::User(&user.id)).org(&user.org_id).target(&user.id).ip(ip)).await;
    // During a required-setup sign-in, enabling finishes the sign-in.
    let tokens = match &ch {
        Some(ch) => {
            let user = user_by_id(&s, &user.id).await?.ok_or_else(challenge_expired)?;
            Some(sign_in(&s, &user, &challenge_device(ch)?, ip).await?)
        }
        None => None,
    };
    Ok(ApiJson(BackupCodes { backup_codes: codes, tokens }))
}

async fn totp_disable(State(s): State<AppState>, ctx: AuthCtx, ApiJson(req): ApiJson<TotpDisable>) -> AppResult<StatusCode> {
    let password = Redacted::new(req.password);
    let code = Redacted::new(req.code);
    let user = current_user(&s, &ctx).await?;
    if !password::verify(password.expose(), &user.password_hash).await {
        return Err(invalid_credentials());
    }
    let (_, policies) = org_of(&s, &user).await?;
    if policies.require_2fa {
        return Err(AppError::forbidden("Your organisation requires two-step verification."));
    }
    if !second_factor(&s, &user, code.expose()).await? {
        return Err(invalid_code());
    }
    let mut tx = s.db.begin().await?;
    sqlx::query("update users set totp_secret_enc = null, totp_enabled_at = null, totp_last_step = null where id = $1")
        .bind(&user.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("delete from backup_codes where user_id = $1").bind(&user.id).execute(&mut *tx).await?;
    tx.commit().await?;
    audit::log(&s.db, Event::new("auth.2fa.disable", Actor::User(&user.id)).org(&user.org_id).target(&user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- devices ----------

#[derive(FromRow)]
struct DeviceRow {
    id: String,
    name: String,
    platform: String,
    app_version: Option<String>,
    created_at: DateTime<Utc>,
    last_seen_at: Option<DateTime<Utc>>,
}

async fn devices(State(s): State<AppState>, ctx: AuthCtx) -> AppResult<ApiJson<Vec<Device>>> {
    let rows: Vec<DeviceRow> = sqlx::query_as(
        "select id, name, platform, app_version, created_at, last_seen_at from devices
         where user_id = $1 and revoked_at is null order by created_at",
    )
    .bind(&ctx.user_id)
    .fetch_all(&s.db)
    .await?;
    Ok(ApiJson(
        rows.into_iter()
            .map(|d| Device {
                current: d.id == ctx.device_id,
                id: d.id,
                name: d.name,
                platform: d.platform,
                app_version: d.app_version.unwrap_or_default(),
                created_at: d.created_at.timestamp_millis(),
                last_seen_at: d.last_seen_at.map(|t| t.timestamp_millis()),
            })
            .collect(),
    ))
}

async fn revoke_device(State(s): State<AppState>, ctx: AuthCtx, ApiPath(id): ApiPath<String>) -> AppResult<StatusCode> {
    let mine: Option<String> =
        sqlx::query_scalar("select id from devices where id = $1 and user_id = $2 and revoked_at is null")
            .bind(&id)
            .bind(&ctx.user_id)
            .fetch_optional(&s.db)
            .await?;
    let Some(id) = mine else { return Err(AppError::not_found()) };
    session::revoke_device(&s, &id).await?;
    audit::log(&s.db, Event::new("auth.device.revoke", Actor::User(&ctx.user_id)).org(&ctx.org_id).target(&id)).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn push_register(State(s): State<AppState>, ctx: AuthCtx, ApiJson(req): ApiJson<PushRegistration>) -> AppResult<StatusCode> {
    let token = Redacted::new(req.token);
    match req.kind.as_str() {
        "none" => {
            sqlx::query("update devices set push_kind = null, push_token_enc = null where id = $1")
                .bind(&ctx.device_id)
                .execute(&s.db)
                .await?;
        }
        "apns" | "fcm" => {
            let t = token.expose().trim();
            if t.is_empty() || t.len() > 4096 {
                return Err(AppError::bad_request("Push token is missing or too long."));
            }
            let dk = s.env.org_key(&s.db, &ctx.org_id).await?;
            let enc = seal(dk.bytes(), &section_aad(&ctx.device_id, "push", 0), t.as_bytes());
            sqlx::query("update devices set push_kind = $2, push_token_enc = $3 where id = $1")
                .bind(&ctx.device_id)
                .bind(&req.kind)
                .bind(enc)
                .execute(&s.db)
                .await?;
        }
        _ => return Err(AppError::bad_request("Push kind must be apns, fcm or none.")),
    }
    audit::log(
        &s.db,
        Event::new("auth.device.push", Actor::User(&ctx.user_id)).org(&ctx.org_id).target(&ctx.device_id).meta(json!({"kind": req.kind})),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_shape() {
        assert!(valid_email("a@b.co"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("@b.co"));
        assert!(!valid_email("a b@c.co"));
        assert!(!valid_email("a@b@c.co"));
        assert_eq!(normalize_email("  Sara@Example.COM "), "sara@example.com");
    }
}
