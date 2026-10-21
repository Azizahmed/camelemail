//! On-prem licence enforcement (design §8.8, §9).
//!
//! A build without issuer keys (`REMAIL_LICENCE_PUBKEYS` unset) and the hosted
//! service don't enforce anything: [`LicenceState::None`]. Otherwise:
//! - `Expired` / `Invalid` block new sign-ins and new devices (admins may
//!   still sign in to install a licence); existing sessions keep refreshing.
//! - Over the seat count, sign-ups and invites are refused.
//!
//! Clock rollback is caught with `deployment.last_seen_clock`, a monotonic
//! high-water mark MAC'd with a key derived from the org's data key, so it
//! can't be wound back with SQL alone.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use remail_proto::api::LicenceState;
use remail_proto::licence::{self, Licence, LicenceError};
use remail_proto::ErrorCode;
use sha2::Sha256;
use sqlx::PgPool;

use crate::config::{Config, Mode};
use crate::crypto::{sha256, Envelope};
use crate::error::AppError;

/// How often the clock high-water mark is moved forward.
const CLOCK_STEP_MINUTES: i64 = 60;

#[derive(Debug, Clone)]
pub struct Status {
    pub state: LicenceState,
    pub licence: Option<Licence>,
    /// Why the licence is invalid, for the console.
    pub problem: Option<String>,
}

impl Status {
    fn none() -> Self {
        Status { state: LicenceState::None, licence: None, problem: None }
    }

    /// New sign-ins and devices are refused in these states.
    pub fn blocks_sign_in(&self) -> bool {
        matches!(self.state, LicenceState::Expired | LicenceState::Invalid)
    }

    pub fn sign_in_error(&self) -> AppError {
        match self.state {
            LicenceState::Invalid => AppError::new(ErrorCode::LicenceInvalid, "This server's licence is not valid."),
            _ => AppError::new(ErrorCode::LicenceExpired, "This server's licence has expired."),
        }
    }
}

fn enforced(cfg: &Config) -> bool {
    cfg.mode == Mode::Onprem && !cfg.licence_keys.is_empty()
}

async fn mac_key(db: &PgPool, env: &Envelope) -> Result<Option<Vec<u8>>> {
    let Some(org) = crate::orgs::first(db).await? else { return Ok(None) };
    let dk = env.org_key(db, &org.id).await?;
    let mut data = b"remail/clock/v1|".to_vec();
    data.extend_from_slice(dk.bytes());
    let key = sha256(&data);
    zeroize::Zeroize::zeroize(&mut data);
    Ok(Some(key))
}

fn clock_mac(key: &[u8], deployment: &str, at: DateTime<Utc>) -> Vec<u8> {
    // HMAC accepts keys of any length.
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).unwrap_or_else(|_| unreachable!());
    m.update(format!("{deployment}|{}", at.timestamp_micros()).as_bytes());
    m.finalize().into_bytes().to_vec()
}

/// Creates the deployment row (once an org exists to key its MAC).
pub async fn ensure_deployment(cfg: &Config, db: &PgPool, env: &Envelope) -> Result<()> {
    let Some(key) = mac_key(db, env).await? else { return Ok(()) };
    let dep = env.provider().deployment();
    let now = trunc(Utc::now());
    sqlx::query("insert into deployment (id, mode, last_seen_clock, clock_mac) values ($1, $2, $3, $4) on conflict (id) do nothing")
        .bind(&dep)
        .bind(cfg.mode.as_str())
        .bind(now)
        .bind(clock_mac(&key, &dep, now))
        .execute(db)
        .await?;
    Ok(())
}

/// Postgres keeps microseconds; MAC what will be read back.
fn trunc(t: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(t.timestamp_micros()).unwrap_or(t)
}

/// Latest trusted wall-clock time, moved forward to `now` when due.
/// `Err` means the stored value was tampered with.
async fn last_seen(db: &PgPool, env: &Envelope, now: DateTime<Utc>) -> Result<Result<Option<DateTime<Utc>>, String>> {
    let Some(key) = mac_key(db, env).await? else { return Ok(Ok(None)) };
    let dep = env.provider().deployment();
    let row: Option<(DateTime<Utc>, Vec<u8>)> =
        sqlx::query_as("select last_seen_clock, clock_mac from deployment where id = $1")
            .bind(&dep)
            .fetch_optional(db)
            .await?;
    let Some((seen, mac)) = row else { return Ok(Ok(None)) };
    let expected = clock_mac(&key, &dep, seen);
    if !bool::from(subtle::ConstantTimeEq::ct_eq(expected.as_slice(), mac.as_slice())) {
        return Ok(Err("the deployment clock record was modified".into()));
    }
    // Cross-check with the database clock too.
    let db_now: DateTime<Utc> = sqlx::query_scalar("select now()").fetch_one(db).await?;
    let newest = now.min(db_now + Duration::minutes(5));
    if newest > seen + Duration::minutes(CLOCK_STEP_MINUTES) {
        let next = trunc(newest);
        sqlx::query("update deployment set last_seen_clock = $2, clock_mac = $3 where id = $1 and last_seen_clock = $4")
            .bind(&dep)
            .bind(next)
            .bind(clock_mac(&key, &dep, next))
            .bind(seen)
            .execute(db)
            .await?;
    }
    Ok(Ok(Some(seen)))
}

/// The licence state right now.
pub async fn status(cfg: &Config, db: &PgPool, env: &Envelope) -> Result<Status> {
    status_at(cfg, db, env, Utc::now()).await
}

pub async fn status_at(cfg: &Config, db: &PgPool, env: &Envelope, now: DateTime<Utc>) -> Result<Status> {
    if !enforced(cfg) {
        return Ok(Status::none());
    }
    let invalid = |problem: String| Status { state: LicenceState::Invalid, licence: None, problem: Some(problem) };
    let seen = match last_seen(db, env, now).await? {
        Ok(s) => s,
        Err(problem) => return Ok(invalid(problem)),
    };
    let raw: Option<String> =
        sqlx::query_scalar("select raw from licences where active order by installed_at desc limit 1")
            .fetch_optional(db)
            .await?;
    let Some(raw) = raw else { return Ok(invalid("no licence is installed".into())) };
    match licence::verify(&raw, &cfg.licence_keys, &env.provider().deployment(), now, seen) {
        Ok((lic, state)) => Ok(Status { state, licence: Some(lic), problem: None }),
        Err(e) => Ok(invalid(e.to_string())),
    }
}

/// Refuses a new user when the licence's seats are used up.
pub async fn check_seat(cfg: &Config, db: &PgPool, env: &Envelope) -> Result<(), AppError> {
    let st = status(cfg, db, env).await?;
    let Some(lic) = st.licence else { return Ok(()) };
    let used: i64 = sqlx::query_scalar("select count(*) from users where disabled_at is null")
        .fetch_one(db)
        .await?;
    if used >= i64::from(lic.seats) {
        return Err(AppError::new(ErrorCode::SeatLimit, "All licensed seats are in use."));
    }
    Ok(())
}

/// Installs a licence (CLI `licence install`); the previous one is kept but
/// made inactive.
pub async fn install(cfg: &Config, db: &PgPool, env: &Envelope, raw: &str) -> Result<(Licence, LicenceState)> {
    if cfg.licence_keys.is_empty() {
        bail!("this build has no licence keys (REMAIL_LICENCE_PUBKEYS); licences aren't used");
    }
    let raw = raw.trim();
    let seen = last_seen(db, env, Utc::now()).await?.ok().flatten();
    let (lic, state) = match licence::verify(raw, &cfg.licence_keys, &env.provider().deployment(), Utc::now(), seen) {
        Ok(ok) => ok,
        Err(LicenceError::NotYetValid(d)) => bail!("the licence starts on {d}"),
        Err(e) => return Err(e).context("licence refused"),
    };
    let mut tx = db.begin().await?;
    sqlx::query("update licences set active = false").execute(&mut *tx).await?;
    sqlx::query(
        "insert into licences (id, raw, customer, seats, features, starts_at, expires_at, active)
         values ($1, $2, $3, $4, $5, $6, $7, true)
         on conflict (id) do update set raw = excluded.raw, active = true, installed_at = now()",
    )
    .bind(&lic.lid)
    .bind(raw)
    .bind(&lic.customer)
    .bind(lic.seats as i32)
    .bind(&lic.features)
    .bind(lic.starts)
    .bind(lic.expires)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    crate::audit::log(
        db,
        crate::audit::Event::new("licence.install", crate::audit::Actor::System)
            .target(&lic.lid)
            .meta(serde_json::json!({"seats": lic.seats, "expires": lic.expires.to_string()})),
    )
    .await;
    Ok((lic, state))
}
