//! The Remail server connection: signing in, the user's setup kept on the
//! server as sections, the organisation's configuration, and change notices.
//!
//! Mail never goes through the server: the engine keeps talking to the
//! providers directly. What the server holds is setup (accounts and their
//! credentials, calendars, signatures, templates, sender rules, snoozes,
//! preferences, shortcuts). This module mirrors it into the local store
//! (`mapping`), keeps an encrypted copy so setup works offline (`cache`),
//! and merges and writes back local changes (`sync`).
//!
//! The session (`{server, refreshToken, deviceId, userId}`) lives in the
//! secret store under `remail-session`; access tokens stay in memory and are
//! refreshed here, never handed to the webview. Until the session is ready
//! the engine's gate stays closed: no account workers, no schedulers.
//!
//! Phases: `signedOut` → (`migrate` when this computer already had setup)
//! → `syncing` (first download, with progress) → `ready`.

pub mod address;
mod cache;
mod http;
pub mod mapping;
mod notices;
mod sync;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::Context;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use remail_proto::api::*;
use remail_proto::org::{OAuthApp, ProviderKind, ServerPreset};
use remail_proto::sections;
use remail_proto::sync::Clock;

use crate::engine::{CloudLink, Engine, OrgAccess};
use crate::store::now_ms;

pub use address::{default_server, Address};
pub use cache::CACHE_KEY_ID;

/// Secret store entry of the session.
pub const SESSION_ID: &str = "remail-session";
/// Settings key: calendar colours and visibility from other devices, by `override_key`.
pub(crate) const CAL_OVERRIDES: &str = "cloud_cal_overrides";
/// Settings key: the server the user chose (device scope).
const SERVER_KEY: &str = "cloud_server";
/// Settings key: the signed-in user, shown while offline.
const ME_KEY: &str = "cloud_me";
/// Settings key: `Flags`.
const FLAGS_KEY: &str = "cloud_flags";
/// Cache rows that aren't sections.
const ORG_ROW: &str = "org";
const PROVIDERS_ROW: &str = "providers";

// ----------------------------------------------------------------------------
// Types shared with the UI (client contract)
// ----------------------------------------------------------------------------

/// A server error as the UI gets it: `cloud:<code>:<message>`, `code` being
/// an `ErrorCode` wire name (`unavailable` when the server can't be reached).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudError {
    pub code: String,
    pub message: String,
}

impl CloudError {
    pub fn new(code: &str, message: impl Into<String>) -> CloudError {
        CloudError { code: code.to_string(), message: message.into() }
    }

    pub(crate) fn from_api(e: &remail_proto::ApiError) -> CloudError {
        let code = e.code.name();
        let message = if e.message.is_empty() { code.clone() } else { e.message.clone() };
        CloudError { code, message }
    }

    pub fn is_unavailable(&self) -> bool {
        self.code == "unavailable"
    }

    fn signed_out() -> CloudError {
        CloudError::new("unauthorized", "You're signed out of Remail")
    }
}

impl std::fmt::Display for CloudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cloud:{}:{}", self.code, self.message)
    }
}

impl std::error::Error for CloudError {}

impl From<anyhow::Error> for CloudError {
    fn from(e: anyhow::Error) -> CloudError {
        match e.downcast::<CloudError>() {
            Ok(c) => c,
            Err(e) => CloudError::new("internal", format!("{e:#}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    SignedOut,
    /// Signed in; this computer's setup waits for "move it to your account?".
    Migrate,
    /// Signed in; bringing the setup down.
    Syncing,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudStatus {
    /// Base URL in use; None until one is chosen.
    pub server: Option<String>,
    /// The build's default server, if any.
    pub default_server: Option<String>,
    pub signed_in: bool,
    pub me: Option<Me>,
    pub phase: Phase,
    /// The server can't be reached (the cached setup is in use).
    pub offline: bool,
    pub progress: Option<Progress>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status")]
pub enum LoginResult {
    #[serde(rename = "ok")]
    Ok { me: Box<Me> },
    #[serde(rename = "2fa", rename_all = "camelCase")]
    TwoFactor { challenge: String, setup_required: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationInfo {
    pub local_accounts: usize,
    pub server_accounts: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SignedOutReason {
    /// The server revoked this device (or the user was disabled).
    Revoked,
    /// The session ran out.
    Expired,
    /// The user signed out.
    User,
}

/// The `cloud-event` payload.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CloudEvent {
    Status { status: Box<CloudStatus> },
    SignedOut { reason: SignedOutReason },
}

/// Receives cloud events (the desktop shell forwards them to the UI).
pub trait CloudSink: Send + Sync + 'static {
    fn cloud_event(&self, event: CloudEvent);
}

/// One entry of "Add mailbox", as the UI gets it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    pub kind: ProviderKind,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthApp>,
    pub manual_servers: bool,
    pub presets: Vec<ServerPreset>,
    pub server_exchange: bool,
}

impl From<ProviderOption> for ProviderView {
    fn from(p: ProviderOption) -> Self {
        ProviderView { kind: p.kind, enabled: p.enabled, oauth: p.oauth, manual_servers: p.manual_servers, presets: p.presets, server_exchange: p.server_exchange }
    }
}

/// Colour and visibility of one calendar, chosen on some device.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CalOverride {
    pub color: Option<String>,
    pub enabled: Option<bool>,
}

/// `(calendar account key, href)` as one string.
pub(crate) fn override_key(cal_key: &str, href: &str) -> String {
    format!("{cal_key}|{href}")
}

/// Polling and waiting. The defaults suit the desktop app; tests shorten them.
#[derive(Debug, Clone)]
pub struct CloudOptions {
    /// Server used until the user picks one.
    pub default_server: Option<String>,
    /// Look for changes this often while change notices arrive…
    pub poll_with_notices: Duration,
    /// …and this often without them.
    pub poll_without_notices: Duration,
    /// Listen for change notices over WebSocket.
    pub notices: bool,
    /// Longest wait for every mailbox's first sync to start ("Setting up your mailboxes").
    pub first_sync_wait: Duration,
}

impl Default for CloudOptions {
    fn default() -> Self {
        CloudOptions {
            default_server: default_server(),
            poll_with_notices: Duration::from_secs(60),
            poll_without_notices: Duration::from_secs(300),
            notices: true,
            first_sync_wait: Duration::from_secs(60),
        }
    }
}

/// This computer, as the server lists it under the user's devices.
pub fn this_device(app_version: &str) -> DeviceInfo {
    let name = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "Remail desktop".into());
    let platform = match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        "ios" => "ios",
        "android" => "android",
        _ => "linux",
    };
    DeviceInfo { name, platform: platform.into(), app_version: app_version.into() }
}

// ----------------------------------------------------------------------------
// State
// ----------------------------------------------------------------------------

/// What this device keeps about the server session.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Session {
    pub server: String,
    pub refresh_token: String,
    pub device_id: String,
    pub user_id: String,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("server", &self.server).field("device_id", &self.device_id).finish_non_exhaustive()
    }
}

/// Where this device is between sign-in and ready (survives restarts).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Flags {
    pending_migration: bool,
    first_sync_done: bool,
    /// The first sync uploads this computer's setup (the migration's
    /// "move it"); otherwise the server's setup wins on the first sync.
    upload: bool,
}

#[derive(Default)]
struct State {
    server: Option<String>,
    session: Option<Session>,
    me: Option<Me>,
    phase: Option<Phase>,
    offline: bool,
    progress: Option<Progress>,
    org: Option<OrgConfigResponse>,
    providers: Option<Vec<ProviderOption>>,
}

pub(crate) struct CloudInner {
    engine: Engine,
    sink: Arc<dyn CloudSink>,
    api: http::Api,
    cache: cache::Cache,
    opts: CloudOptions,
    device: DeviceInfo,
    state: Mutex<State>,
    /// The last status sent to the UI (JSON), to send only changes.
    last_status: Mutex<String>,
    /// Access token and when it expires (ms). Its lock also keeps refreshes one at a time.
    access: tokio::sync::Mutex<Option<(String, i64)>>,
    /// One sync (or migration, or sign-out) at a time.
    sync_lock: tokio::sync::Mutex<()>,
    /// Held while a section's cached document is read, changed and saved.
    cache_lock: Mutex<()>,
    clock: Mutex<Clock>,
    /// Local setup changed, or a notice came: sync soon.
    wake: Notify,
    /// The server answered after being unreachable (the notices socket reconnects).
    server_back: Notify,
    /// Background work of this session (first sync, sync loop, notices).
    tasks: Mutex<Option<CancellationToken>>,
    loops_running: AtomicBool,
    ws_connected: AtomicBool,
    org_stale: AtomicBool,
    /// Bumped at every sign-in and sign-out: work started under another
    /// session drops its results.
    epoch: AtomicU64,
    link: Arc<Link>,
}

/// The engine's way back to the server (it holds this weakly).
struct Link(Weak<CloudInner>);

impl CloudLink for Link {
    fn mark_dirty(&self, section: &str) {
        if let Some(c) = self.0.upgrade() {
            tracing::trace!(section, "setup changed");
            // Stamped now, not when the next sync gets to it (that may be
            // hours later, offline): a delete made elsewhere in between wins.
            c.collect_now(section);
            c.wake.notify_one();
        }
    }

    fn google_exchange(&self, req: GoogleExchange) -> futures::future::BoxFuture<'static, anyhow::Result<GoogleTokens>> {
        let weak = self.0.clone();
        Box::pin(async move {
            let c = weak.upgrade().context("signed out of Remail")?;
            Ok(c.authed::<GoogleTokens>(Method::POST, "/v1/providers/google/token", Some(&req)).await?)
        })
    }

    fn google_refresh(&self, refresh_token: String) -> futures::future::BoxFuture<'static, anyhow::Result<GoogleTokens>> {
        let weak = self.0.clone();
        Box::pin(async move {
            let c = weak.upgrade().context("signed out of Remail")?;
            Ok(c.authed::<GoogleTokens>(Method::POST, "/v1/providers/google/refresh", Some(&GoogleRefresh { refresh_token })).await?)
        })
    }
}

/// The Remail server connection. Cheap to clone.
#[derive(Clone)]
pub struct Cloud {
    inner: Arc<CloudInner>,
}

impl Cloud {
    /// Connect `engine` to the server (nothing is contacted until `start`).
    pub fn new(engine: Engine, sink: Arc<dyn CloudSink>, device: DeviceInfo, opts: CloudOptions) -> anyhow::Result<Cloud> {
        let cache = cache::Cache::open(engine.secrets().as_ref())?;
        let api = http::Api::new()?;
        let inner = Arc::new_cyclic(|weak| CloudInner {
            engine: engine.clone(),
            sink,
            api,
            cache,
            opts,
            device,
            state: Mutex::new(State::default()),
            last_status: Mutex::new(String::new()),
            access: tokio::sync::Mutex::new(None),
            sync_lock: tokio::sync::Mutex::new(()),
            cache_lock: Mutex::new(()),
            clock: Mutex::new(Clock::new("local")),
            wake: Notify::new(),
            server_back: Notify::new(),
            tasks: Mutex::new(None),
            loops_running: AtomicBool::new(false),
            ws_connected: AtomicBool::new(false),
            org_stale: AtomicBool::new(true),
            epoch: AtomicU64::new(0),
            link: Arc::new(Link(weak.clone())),
        });
        let link: Arc<dyn CloudLink> = inner.link.clone();
        engine.set_cloud_link(Arc::downgrade(&link));
        Ok(Cloud { inner })
    }

    pub fn engine(&self) -> &Engine {
        &self.inner.engine
    }

    /// Pick up where the last run left off: signed out, half-way, or ready
    /// (then mail starts at once from the cached setup, online or not).
    pub async fn start(&self) -> anyhow::Result<()> {
        let c = &self.inner;
        let store = c.engine.store();
        let session: Option<Session> = c.engine.secrets().get(SESSION_ID)?.and_then(|s| serde_json::from_str(&s).ok());
        let org: Option<OrgConfigResponse> = c.cache.load_json(store, ORG_ROW);
        let providers: Option<Vec<ProviderOption>> = c.cache.load_json(store, PROVIDERS_ROW);
        if let Some(o) = &org {
            c.engine.set_org(Some(org_access(o, providers.as_deref())));
        }
        let flags: Flags = store.setting(FLAGS_KEY).unwrap_or_default();
        let phase = match &session {
            None => Phase::SignedOut,
            Some(_) if flags.pending_migration => Phase::Migrate,
            Some(_) if !flags.first_sync_done => Phase::Syncing,
            Some(_) => Phase::Ready,
        };
        if let Some(s) = &session {
            *c.clock.lock().unwrap() = Clock::new(s.device_id.clone());
        }
        c.update(|st| {
            st.server = store.setting(SERVER_KEY).or_else(|| c.opts.default_server.clone());
            st.me = session.as_ref().and_then(|_| store.setting(ME_KEY));
            st.session = session;
            st.org = org;
            st.providers = providers;
            st.phase = Some(phase);
        });
        match phase {
            Phase::Syncing => c.spawn_first_sync(),
            Phase::Ready => {
                c.engine.open_gate()?;
                c.start_loops();
            }
            _ => {}
        }
        Ok(())
    }

    pub fn status(&self) -> CloudStatus {
        self.inner.status()
    }

    /// Choose the server: a URL, a bare host (`https://` is assumed) or an
    /// organisation code. The server must answer `/v1/meta` before it's kept.
    pub async fn set_server(&self, address: &str) -> Result<Meta, CloudError> {
        let c = &self.inner;
        let server = match address::parse(address)? {
            Address::Server(s) => s,
            Address::OrgCode(code) => {
                let dir = address::org_directory().or_else(|| c.opts.default_server.clone()).ok_or_else(|| CloudError::new("bad_request", "Enter your server's address"))?;
                let found: OrgCodeResolved = c.api.call(Method::GET, &dir, &format!("/v1/orgcode/{code}"), None, None::<&()>).await.map_err(|e| {
                    if e.code == "not_found" {
                        CloudError::new("not_found", "No organisation uses that code")
                    } else {
                        e
                    }
                })?;
                address::check_url(&found.server)?
            }
        };
        // Asking again for the server in use is fine (the UI does it for `Meta`);
        // another server needs signing out first.
        let current = c.state.lock().unwrap().session.as_ref().map(|s| s.server.clone());
        if current.as_ref().is_some_and(|cur| *cur != server) {
            return Err(CloudError::new("conflict", "Sign out before choosing another server"));
        }
        let meta: Meta = c.api.call(Method::GET, &server, "/v1/meta", None, None::<&()>).await?;
        c.engine.store().set_setting(SERVER_KEY, &server)?;
        c.update(|st| st.server = Some(server));
        Ok(meta)
    }

    pub async fn signup(&self, req: SignupRequest) -> Result<SignupResponse, CloudError> {
        let server = self.inner.chosen_server()?;
        self.inner.api.call(Method::POST, &server, "/v1/auth/signup", None, Some(&req)).await
    }

    pub async fn verify_email(&self, token: &str) -> Result<(), CloudError> {
        let server = self.inner.chosen_server()?;
        self.inner.api.call(Method::POST, &server, "/v1/auth/verify", None, Some(&TokenRequest { token: token.into() })).await
    }

    pub async fn login(&self, email: &str, password: &str) -> Result<LoginResult, CloudError> {
        let c = &self.inner;
        let server = c.chosen_server()?;
        let req = LoginRequest { email: email.trim().into(), password: password.into(), device: c.device.clone() };
        match c.api.call::<LoginResponse>(Method::POST, &server, "/v1/auth/login", None, Some(&req)).await? {
            LoginResponse::Tokens(t) => Ok(LoginResult::Ok { me: Box::new(c.after_sign_in(server, t).await?) }),
            LoginResponse::Challenge(ch) => Ok(LoginResult::TwoFactor { challenge: ch.challenge, setup_required: ch.setup_required }),
        }
    }

    pub async fn login_2fa(&self, challenge: &str, code: &str) -> Result<Me, CloudError> {
        let c = &self.inner;
        let server = c.chosen_server()?;
        let req = TwoFactorRequest { challenge: challenge.into(), code: code.trim().into() };
        let t: Tokens = c.api.call(Method::POST, &server, "/v1/auth/login/2fa", None, Some(&req)).await?;
        c.after_sign_in(server, t).await
    }

    /// Sign this device out: the server forgets it, and this computer's
    /// synced setup and credentials go (mail stops).
    pub async fn logout(&self) -> Result<(), CloudError> {
        let c = &self.inner;
        // Take the session first: the server's "signed out" notice for this very
        // sign-out must not end it as revoked (the UI would say another device did it).
        let server = c.state.lock().unwrap().session.take().map(|s| s.server);
        let token = c.access.lock().await.as_ref().map(|(t, _)| t.clone());
        if let (Some(server), Some(token)) = (server, token) {
            // Best effort: signing out works offline too (the session then expires on the server).
            if let Err(e) = c.api.call::<()>(Method::POST, &server, "/v1/auth/logout", Some(&token), None::<&()>).await {
                tracing::info!("couldn't tell the Remail server about signing out: {e}");
            }
        }
        c.end_session(SignedOutReason::User).await;
        Ok(())
    }

    pub async fn request_reset(&self, email: &str) -> Result<(), CloudError> {
        let server = self.inner.chosen_server()?;
        self.inner.api.call(Method::POST, &server, "/v1/auth/reset/request", None, Some(&ResetRequest { email: email.trim().into() })).await
    }

    pub async fn reset_password(&self, token: &str, password: &str) -> Result<(), CloudError> {
        let server = self.inner.chosen_server()?;
        let req = ResetPassword { token: token.into(), password: password.into() };
        self.inner.api.call(Method::POST, &server, "/v1/auth/reset", None, Some(&req)).await
    }

    pub async fn me(&self) -> Result<Me, CloudError> {
        let me: Me = self.inner.authed(Method::GET, "/v1/me", None::<&()>).await?;
        self.inner.remember_me(&me);
        Ok(me)
    }

    pub async fn update_me(&self, name: &str) -> Result<Me, CloudError> {
        let me: Me = self.inner.authed(Method::PATCH, "/v1/me", Some(&UpdateMe { name: Some(name.trim().into()) })).await?;
        self.inner.remember_me(&me);
        Ok(me)
    }

    pub async fn change_password(&self, current: &str, new: &str) -> Result<(), CloudError> {
        self.inner.authed(Method::POST, "/v1/me/password", Some(&ChangePassword { current: current.into(), new: new.into() })).await
    }

    /// Start setting up two-step sign-in. With a challenge: during a sign-in
    /// the organisation requires it for.
    pub async fn totp_setup(&self, challenge: Option<String>) -> Result<TotpSetup, CloudError> {
        let c = &self.inner;
        match challenge {
            Some(ch) => {
                let server = c.chosen_server()?;
                c.api.call(Method::POST, &server, "/v1/me/2fa/setup", None, Some(&TotpSetupRequest { challenge: Some(ch) })).await
            }
            None => c.authed(Method::POST, "/v1/me/2fa/setup", Some(&TotpSetupRequest { challenge: None })).await,
        }
    }

    /// Turn on two-step sign-in. With a challenge this also finishes the
    /// sign-in (the tokens stay here; the UI gets the backup codes only).
    pub async fn totp_enable(&self, code: &str, challenge: Option<String>) -> Result<BackupCodes, CloudError> {
        let c = &self.inner;
        let req = TotpEnable { code: code.trim().into(), challenge: challenge.clone() };
        let mut codes: BackupCodes = match &challenge {
            Some(_) => {
                let server = c.chosen_server()?;
                let codes: BackupCodes = c.api.call(Method::POST, &server, "/v1/me/2fa/enable", None, Some(&req)).await?;
                if let Some(t) = codes.tokens.clone() {
                    c.after_sign_in(server, t).await?;
                }
                codes
            }
            None => c.authed(Method::POST, "/v1/me/2fa/enable", Some(&req)).await?,
        };
        codes.tokens = None;
        Ok(codes)
    }

    pub async fn totp_disable(&self, password: &str, code: &str) -> Result<(), CloudError> {
        self.inner.authed(Method::POST, "/v1/me/2fa/disable", Some(&TotpDisable { password: password.into(), code: code.trim().into() })).await
    }

    pub async fn devices(&self) -> Result<Vec<Device>, CloudError> {
        self.inner.authed(Method::GET, "/v1/me/devices", None::<&()>).await
    }

    /// Sign a device out (this one too: then it's like signing out).
    pub async fn revoke_device(&self, id: &str) -> Result<(), CloudError> {
        let c = &self.inner;
        let id: String = id.chars().filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-').collect();
        c.authed::<()>(Method::DELETE, &format!("/v1/me/devices/{id}"), None::<&()>).await?;
        let own = c.state.lock().unwrap().session.as_ref().is_some_and(|s| s.device_id == id);
        if own {
            c.end_session(SignedOutReason::Revoked).await;
        }
        Ok(())
    }

    /// What "Add mailbox" may offer: from the server, or the cached copy offline.
    pub async fn providers(&self) -> Result<Vec<ProviderView>, CloudError> {
        let c = &self.inner;
        if let Err(e) = c.refresh_org().await {
            if !e.is_unavailable() {
                return Err(e);
            }
        }
        let st = c.state.lock().unwrap();
        let list = match (&st.providers, &st.org) {
            (Some(p), _) => p.clone(),
            (None, Some(o)) => providers_for(&o.config, false),
            (None, None) => return Err(CloudError::new("unavailable", "Couldn't reach the Remail server")),
        };
        Ok(list.into_iter().map(ProviderView::from).collect())
    }

    /// How much setup is here and on the server (for the "move this
    /// computer's setup?" choice).
    pub async fn migration(&self) -> Result<MigrationInfo, CloudError> {
        let c = &self.inner;
        let server = c.fetch_section(sections::ACCOUNTS).await?;
        Ok(MigrationInfo { local_accounts: c.engine.accounts()?.len(), server_accounts: server.doc.live().count() })
    }

    /// Answer the migration question: `upload` moves this computer's setup
    /// into the account (accounts already there are matched by address and
    /// take the server's ids); otherwise it's discarded for the server's.
    pub async fn migrate(&self, upload: bool) -> Result<(), CloudError> {
        self.inner.migrate(upload).await
    }

    /// Sync the setup with the server now.
    pub async fn sync_now(&self) -> Result<(), CloudError> {
        let c = &self.inner;
        match c.status().phase {
            Phase::Ready => c.cycle(sync::Mode::Regular).await,
            Phase::Syncing => {
                c.wake.notify_one();
                Ok(())
            }
            _ => Err(CloudError::signed_out()),
        }
    }
}

// ----------------------------------------------------------------------------
// Session handling
// ----------------------------------------------------------------------------

/// The engine's view of the organisation's providers.
fn org_access(org: &OrgConfigResponse, providers: Option<&[ProviderOption]>) -> OrgAccess {
    OrgAccess {
        providers: org.config.providers.clone(),
        google_server_exchange: providers.into_iter().flatten().any(|p| p.kind == ProviderKind::Google && p.server_exchange),
    }
}

impl CloudInner {
    fn status(&self) -> CloudStatus {
        let st = self.state.lock().unwrap();
        CloudStatus {
            server: st.session.as_ref().map(|s| s.server.clone()).or_else(|| st.server.clone()),
            default_server: self.opts.default_server.clone(),
            signed_in: st.session.is_some(),
            me: st.me.clone(),
            phase: st.phase.unwrap_or(Phase::SignedOut),
            offline: st.offline,
            progress: st.progress.clone(),
        }
    }

    /// Change the state, and tell the UI when what it sees changed.
    fn update(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock().unwrap());
        let status = self.status();
        let json = serde_json::to_string(&status).unwrap_or_default();
        let mut last = self.last_status.lock().unwrap();
        if *last != json {
            *last = json;
            drop(last);
            self.sink.cloud_event(CloudEvent::Status { status: Box::new(status) });
        }
    }

    fn set_offline(&self, offline: bool) {
        if self.state.lock().unwrap().offline != offline {
            self.update(|st| st.offline = offline);
            if !offline {
                self.server_back.notify_one();
            }
        }
    }

    /// Note whether the server answered.
    fn note<T>(&self, r: &Result<T, CloudError>) {
        self.set_offline(matches!(r, Err(e) if e.is_unavailable()));
    }

    fn signed_in(&self) -> bool {
        self.state.lock().unwrap().session.is_some()
    }

    /// The server for signing in: the user's choice, else the build's.
    fn chosen_server(&self) -> Result<String, CloudError> {
        let st = self.state.lock().unwrap();
        st.server.clone().or_else(|| self.opts.default_server.clone()).ok_or_else(|| CloudError::new("bad_request", "Choose your Remail server first"))
    }

    fn session_server(&self) -> Result<String, CloudError> {
        self.state.lock().unwrap().session.as_ref().map(|s| s.server.clone()).ok_or_else(CloudError::signed_out)
    }

    fn save_session(&self, s: &Session) -> Result<(), CloudError> {
        let json = serde_json::to_string(s).map_err(|e| CloudError::new("internal", e.to_string()))?;
        self.engine.secrets().set(SESSION_ID, &json).map_err(|e| CloudError::new("internal", format!("couldn't save the session: {e:#}")))?;
        Ok(())
    }

    fn remember_me(&self, me: &Me) {
        if let Err(e) = self.engine.store().set_setting(ME_KEY, me) {
            tracing::warn!("couldn't save the profile: {e:#}");
        }
        self.update(|st| st.me = Some(me.clone()));
    }

    fn flags(&self) -> Flags {
        self.engine.store().setting(FLAGS_KEY).unwrap_or_default()
    }

    fn set_flags(&self, f: &Flags) {
        if let Err(e) = self.engine.store().set_setting(FLAGS_KEY, f) {
            tracing::warn!("couldn't save the sync state: {e:#}");
        }
    }

    /// A sign-in finished: keep the session, fetch the organisation's
    /// config, then migrate (this computer had setup) or bring setup down.
    async fn after_sign_in(self: &Arc<Self>, server: String, t: Tokens) -> Result<Me, CloudError> {
        let session = Session { server: server.clone(), refresh_token: t.refresh_token.clone(), device_id: t.device_id.clone(), user_id: t.user.id.clone() };
        self.save_session(&session)?;
        *self.access.lock().await = Some((t.access_token.clone(), t.access_expires_at));
        self.epoch.fetch_add(1, Ordering::SeqCst);
        *self.clock.lock().unwrap() = Clock::new(t.device_id.clone());
        let store = self.engine.store();
        cache::Cache::clear(store)?;
        if let Err(e) = store.set_setting(ME_KEY, &t.user) {
            tracing::warn!("couldn't save the profile: {e:#}");
        }
        self.update(|st| {
            st.server = Some(server);
            st.session = Some(session);
            st.me = Some(t.user.clone());
            st.offline = false;
        });
        // It decides which mailboxes may be added (and checked when they sync).
        if let Err(e) = self.refresh_org().await {
            tracing::info!("couldn't get the organisation's settings yet: {e}");
        }
        let has_setup = !self.engine.accounts()?.is_empty();
        self.set_flags(&Flags { pending_migration: has_setup, ..Default::default() });
        if has_setup {
            self.update(|st| st.phase = Some(Phase::Migrate));
        } else {
            self.update(|st| st.phase = Some(Phase::Syncing));
            self.spawn_first_sync();
        }
        Ok(t.user)
    }

    /// The access token, refreshed when it's (nearly) expired.
    async fn access_token(self: &Arc<Self>) -> Result<String, CloudError> {
        let mut guard = self.access.lock().await;
        if let Some((t, exp)) = guard.as_ref() {
            if *exp - now_ms() > 60_000 {
                return Ok(t.clone());
            }
        }
        let session = self.state.lock().unwrap().session.clone().ok_or_else(CloudError::signed_out)?;
        let req = RefreshRequest { refresh_token: session.refresh_token.clone() };
        let r: Result<Tokens, CloudError> = self.api.call(Method::POST, &session.server, "/v1/auth/refresh", None, Some(&req)).await;
        match r {
            Ok(t) => {
                if !self.signed_in() {
                    return Err(CloudError::signed_out());
                }
                // Refresh tokens rotate: keep the new one before anything else.
                let new = Session { refresh_token: t.refresh_token.clone(), device_id: t.device_id.clone(), ..session };
                self.save_session(&new)?;
                *guard = Some((t.access_token.clone(), t.access_expires_at));
                drop(guard);
                if let Err(e) = self.engine.store().set_setting(ME_KEY, &t.user) {
                    tracing::warn!("couldn't save the profile: {e:#}");
                }
                self.update(|st| {
                    st.session = Some(new);
                    st.me = Some(t.user);
                    st.offline = false;
                });
                Ok(t.access_token)
            }
            Err(e) if e.is_unavailable() => {
                self.set_offline(true);
                Err(e)
            }
            Err(e) if matches!(e.code.as_str(), "invalid_token" | "unauthorized" | "account_disabled" | "forbidden" | "not_found") => {
                let reason = if matches!(e.code.as_str(), "account_disabled" | "forbidden") { SignedOutReason::Revoked } else { SignedOutReason::Expired };
                tracing::info!("the Remail session ended ({})", e.code);
                drop(guard);
                let me = self.clone();
                // Not here: whoever called may hold the sync lock the sign-out needs.
                tokio::spawn(async move { me.end_session(reason).await });
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    /// A request with the access token; one refresh and retry when it's refused.
    pub(crate) async fn authed<T: serde::de::DeserializeOwned>(
        self: &Arc<Self>,
        method: Method,
        path: &str,
        body: Option<&(impl Serialize + ?Sized)>,
    ) -> Result<T, CloudError> {
        let server = self.session_server()?;
        let token = self.access_token().await?;
        let r = match self.api.call(method.clone(), &server, path, Some(&token), body).await {
            Err(e) if e.code == "unauthorized" => {
                self.forget_access(&token).await;
                let token = self.access_token().await?;
                self.api.call(method, &server, path, Some(&token), body).await
            }
            r => r,
        };
        self.note(&r);
        r
    }

    /// Drop an access token the server refused (unless it was replaced already).
    async fn forget_access(&self, token: &str) {
        let mut g = self.access.lock().await;
        if g.as_ref().is_some_and(|(t, _)| t == token) {
            *g = None;
        }
    }

    /// `GET /v1/sections/{id}` (never written: version 0, empty).
    async fn fetch_section(self: &Arc<Self>, id: &str) -> Result<Section, CloudError> {
        match self.authed::<Section>(Method::GET, &format!("/v1/sections/{id}"), None::<&()>).await {
            Err(e) if e.code == "not_found" => Ok(Section { id: id.into(), version: 0, doc: Default::default() }),
            r => r,
        }
    }

    /// Fetch and cache the organisation's config and provider list.
    async fn refresh_org(self: &Arc<Self>) -> Result<(), CloudError> {
        let org: OrgConfigResponse = self.authed(Method::GET, "/v1/org/config", None::<&()>).await?;
        let providers: Vec<ProviderOption> = self.authed(Method::GET, "/v1/providers", None::<&()>).await?;
        let store = self.engine.store();
        self.cache.save_json(store, ORG_ROW, &org, org.version)?;
        self.cache.save_json(store, PROVIDERS_ROW, &providers, org.version)?;
        self.engine.set_org(Some(org_access(&org, Some(&providers))));
        self.org_stale.store(false, Ordering::SeqCst);
        self.update(|st| {
            st.org = Some(org);
            st.providers = Some(providers);
        });
        Ok(())
    }

    /// Background work of the current session, created on first use.
    fn tasks_token(&self) -> CancellationToken {
        self.tasks.lock().unwrap().get_or_insert_with(CancellationToken::new).clone()
    }

    fn spawn_task(self: &Arc<Self>, work: impl std::future::Future<Output = ()> + Send + 'static) {
        let token = self.tasks_token();
        tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {}
                _ = work => {}
            }
        });
    }

    fn spawn_first_sync(self: &Arc<Self>) {
        let me = self.clone();
        self.spawn_task(async move { me.first_sync().await });
    }

    /// The sync loop and the notices listener (once per session).
    fn start_loops(self: &Arc<Self>) {
        if self.loops_running.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        self.spawn_task(async move { me.sync_loop().await });
        if self.opts.notices {
            let me = self.clone();
            self.spawn_task(async move { notices::listen(me).await });
        }
    }

    /// Signed out (by the user, or the server ended the session): stop
    /// everything, and remove this device's synced setup and credentials.
    async fn end_session(self: &Arc<Self>, reason: SignedOutReason) {
        let had = self.state.lock().unwrap().session.take().is_some();
        if !had && reason != SignedOutReason::User {
            return;
        }
        self.epoch.fetch_add(1, Ordering::SeqCst);
        if let Some(t) = self.tasks.lock().unwrap().take() {
            t.cancel();
        }
        self.loops_running.store(false, Ordering::SeqCst);
        self.ws_connected.store(false, Ordering::SeqCst);
        *self.access.lock().await = None;
        let _one = self.sync_lock.lock().await;
        if let Err(e) = self.engine.secrets().delete(SESSION_ID) {
            tracing::warn!("couldn't delete the session: {e:#}");
        }
        if let Err(e) = self.engine.wipe_setup() {
            tracing::warn!("couldn't remove the synced setup: {e:#}");
        }
        self.engine.stop_mcp().await;
        self.engine.set_org(None);
        let store = self.engine.store();
        for key in [ME_KEY, FLAGS_KEY] {
            let _ = store.delete_setting(key);
        }
        self.update(|st| {
            st.me = None;
            st.phase = Some(Phase::SignedOut);
            st.progress = None;
            st.offline = false;
            st.org = None;
            st.providers = None;
        });
        self.sink.cloud_event(CloudEvent::SignedOut { reason });
    }

    async fn migrate(self: &Arc<Self>, upload: bool) -> Result<(), CloudError> {
        if self.status().phase != Phase::Migrate {
            return Err(CloudError::new("conflict", "There's nothing to move"));
        }
        let server = if upload { Some(self.fetch_section(sections::ACCOUNTS).await?) } else { None };
        {
            let _one = self.sync_lock.lock().await;
            let store = self.engine.store();
            match &server {
                Some(s) => {
                    let n = sync::adopt_server_ids(&self.engine, &s.doc)?;
                    tracing::info!(matched = n, "moving this computer's setup to the Remail account");
                }
                None => {
                    self.engine.wipe_setup()?;
                    // The org config was cached in the same table.
                    let st = self.state.lock().unwrap();
                    if let Some(o) = &st.org {
                        self.cache.save_json(store, ORG_ROW, o, o.version)?;
                    }
                    if let Some(p) = &st.providers {
                        self.cache.save_json(store, PROVIDERS_ROW, p, 0)?;
                    }
                }
            }
            // The AI key moves to the organisation (an admin enters it in the console).
            if let Err(e) = self.engine.secrets().delete(crate::ai::KEY_SECRET_ID) {
                tracing::warn!("couldn't remove the local AI key: {e:#}");
            }
            self.set_flags(&Flags { upload, ..Default::default() });
        }
        self.update(|st| st.phase = Some(Phase::Syncing));
        self.spawn_first_sync();
        Ok(())
    }
}
