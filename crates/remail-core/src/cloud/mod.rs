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
mod vault;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::Context;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use remail_proto::api::*;
use remail_proto::org::{OAuthApp, OrgAi, ProviderKind, Providers, ServerPreset};
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
/// Settings key: the org config version this device's AI key was fetched at.
const AI_KEY_VERSION: &str = "cloud_ai_key_version";
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

/// The proof a password gives with an account's key settings (from
/// `POST /v1/auth/prelogin`), for clients that sign in without `Cloud`.
pub fn password_proof(password: &str, kdf: &Kdf) -> Result<String, CloudError> {
    Ok(vault::derive(password, kdf)?.proof)
}

/// The password didn't unwrap the vault key (checked here: the server can't).
fn wrong_password() -> CloudError {
    CloudError::new("invalid_credentials", "Wrong password.")
}

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
    /// Whether this device can read and write the synced mailbox sign-ins.
    pub vault: VaultState,
}

/// Mailbox sign-ins sync end-to-end encrypted with the account's vault key,
/// which the Camel Mail password unlocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VaultState {
    SignedOut,
    /// This device holds the vault key.
    Ready,
    /// The password is needed once on this device: to unlock the vault key,
    /// or to convert an account that doesn't have one yet.
    Locked,
}

/// A sign-in waiting for its tokens (a second step may come in between).
enum PendingVault {
    /// Converting the account: this vault key goes with the sign-in.
    Have(vault::VaultKey),
    /// The password's keys, to unwrap the vault key the tokens bring.
    Unwrap(vault::Keys),
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
    /// The id of the vault key this device holds.
    device_vault: Option<String>,
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
    pending_vault: Mutex<Option<PendingVault>>,
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

    fn refresh_ai_key(&self) -> futures::future::BoxFuture<'static, anyhow::Result<Option<String>>> {
        let weak = self.0.clone();
        Box::pin(async move {
            let c = weak.upgrade().context("signed out of Remail")?;
            Ok(c.fetch_ai_key().await?)
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
            pending_vault: Mutex::new(None),
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
            st.device_vault = session.as_ref().and_then(|_| vault::load(&c.engine)).map(|k| k.id.clone());
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

    /// The password never leaves this device: the server gets its proof
    /// and the new account's vault key, wrapped with it.
    pub async fn signup(&self, mut req: SignupRequest) -> Result<SignupResponse, CloudError> {
        let server = self.inner.chosen_server()?;
        vault::check_new_password(&req.password, &req.email)?;
        let (proof, setup) = vault::new_secret(&req.password, &vault::new_vault_key()).await?;
        req.password = proof;
        req.vault = Some(setup);
        self.inner.api.call(Method::POST, &server, "/v1/auth/signup", None, Some(&req)).await
    }

    pub async fn verify_email(&self, token: &str) -> Result<(), CloudError> {
        let server = self.inner.chosen_server()?;
        self.inner.api.call(Method::POST, &server, "/v1/auth/verify", None, Some(&TokenRequest { token: token.into() })).await
    }

    /// Signs in with the password's proof, and unlocks the vault key with
    /// the password. An account that still signs in with its password is
    /// converted on the way (it sends the password once more).
    pub async fn login(&self, email: &str, password: &str) -> Result<LoginResult, CloudError> {
        let c = &self.inner;
        let server = c.chosen_server()?;
        let email = email.trim();
        let pre: Result<PreloginResponse, CloudError> =
            c.api.call(Method::POST, &server, "/v1/auth/prelogin", None, Some(&PreloginRequest { email: email.into() })).await;
        let (secret, upgrade, pending) = match pre {
            Ok(PreloginResponse { kdf: Some(kdf) }) => {
                let keys = vault::derive_blocking(password, &kdf).await?;
                (keys.proof.clone(), None, Some(PendingVault::Unwrap(keys)))
            }
            Ok(PreloginResponse { kdf: None }) => {
                let vk = vault::new_vault_key();
                let (proof, setup) = vault::new_secret(password, &vk).await?;
                (password.to_string(), Some(VaultUpgrade { proof, vault: setup }), Some(PendingVault::Have(vk)))
            }
            // A server from before end-to-end encryption.
            Err(e) if e.code == "not_found" => (password.to_string(), None, None),
            Err(e) => return Err(e),
        };
        *c.pending_vault.lock().unwrap() = pending;
        let req = LoginRequest { email: email.into(), password: secret, device: c.device.clone(), upgrade };
        match c.api.call::<LoginResponse>(Method::POST, &server, "/v1/auth/login", None, Some(&req)).await {
            Ok(LoginResponse::Tokens(t)) => {
                c.take_vault(&t)?;
                Ok(LoginResult::Ok { me: Box::new(c.after_sign_in(server, t).await?) })
            }
            Ok(LoginResponse::Challenge(ch)) => Ok(LoginResult::TwoFactor { challenge: ch.challenge, setup_required: ch.setup_required }),
            Err(e) => {
                c.pending_vault.lock().unwrap().take();
                Err(e)
            }
        }
    }

    pub async fn login_2fa(&self, challenge: &str, code: &str) -> Result<Me, CloudError> {
        let c = &self.inner;
        let server = c.chosen_server()?;
        let req = TwoFactorRequest { challenge: challenge.into(), code: code.trim().into() };
        let t: Tokens = c.api.call(Method::POST, &server, "/v1/auth/login/2fa", None, Some(&req)).await?;
        c.take_vault(&t)?;
        c.after_sign_in(server, t).await
    }

    /// Unlocks this device's synced mailbox sign-ins with the Camel Mail
    /// password (a device signed in before they were encrypted end to end,
    /// or that lost its vault key). An account without a vault key gets one.
    pub async fn unlock(&self, password: &str) -> Result<(), CloudError> {
        let c = &self.inner;
        let have: Option<VaultSetup> = c.authed(Method::GET, "/v1/me/vault", None::<&()>).await?;
        let vk = match have {
            Some(setup) => {
                let keys = vault::derive_blocking(password, &setup.kdf).await?;
                vault::unwrap(&keys, &setup).ok_or_else(wrong_password)?
            }
            None => {
                let vk = vault::new_vault_key();
                let (proof, setup) = vault::new_secret(password, &vk).await?;
                let req = VaultConvert { password: password.into(), upgrade: VaultUpgrade { proof, vault: setup } };
                let _: VaultSetup = c.authed(Method::POST, "/v1/me/vault", Some(&req)).await?;
                vk
            }
        };
        c.keep_vault(&vk)?;
        self.me().await?;
        c.wake.notify_one();
        Ok(())
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

    /// A forgotten password: a new vault key goes with the new one (the old
    /// key went with the old password, so mailboxes need signing in again).
    pub async fn reset_password(&self, token: &str, password: &str) -> Result<(), CloudError> {
        let server = self.inner.chosen_server()?;
        vault::check_new_password(password, "")?;
        let (proof, setup) = vault::new_secret(password, &vault::new_vault_key()).await?;
        let req = ResetPassword { token: token.into(), password: proof, vault: Some(setup) };
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

    /// The vault key stays; only its wrapping changes, so synced mailbox
    /// sign-ins stay readable. (An account without one gets one here.)
    pub async fn change_password(&self, current: &str, new: &str) -> Result<(), CloudError> {
        let c = &self.inner;
        let email = c.state.lock().unwrap().me.as_ref().map(|m| m.email.clone()).unwrap_or_default();
        vault::check_new_password(new, &email)?;
        let have: Option<VaultSetup> = c.authed(Method::GET, "/v1/me/vault", None::<&()>).await?;
        let (current_secret, vk) = match &have {
            Some(setup) => {
                let keys = vault::derive_blocking(current, &setup.kdf).await?;
                (keys.proof.clone(), vault::unwrap(&keys, setup).ok_or_else(wrong_password)?)
            }
            None => (current.to_string(), vault::new_vault_key()),
        };
        let (proof, setup) = vault::new_secret(new, &vk).await?;
        let req = ChangePassword { current: current_secret, new: proof, vault: Some(setup) };
        c.authed::<()>(Method::POST, "/v1/me/password", Some(&req)).await?;
        c.keep_vault(&vk)?;
        self.me().await?;
        c.wake.notify_one();
        Ok(())
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
                    c.take_vault(&t)?;
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
        let c = &self.inner;
        // An account with a vault key confirms with the password's proof.
        let have: Option<VaultSetup> = c.authed(Method::GET, "/v1/me/vault", None::<&()>).await?;
        let password = match have {
            Some(setup) => vault::derive_blocking(password, &setup.kdf).await?.proof,
            None => password.to_string(),
        };
        c.authed(Method::POST, "/v1/me/2fa/disable", Some(&TotpDisable { password, code: code.trim().into() })).await
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

    // ---- admin (owners and admins; the server answers 403 to anyone else)

    /// The organisation's providers and AI setup (secrets only as "set").
    pub async fn admin_config(&self) -> Result<AdminConfig, CloudError> {
        self.inner.authed(Method::GET, "/v1/admin/config", None::<&()>).await
    }

    /// Save the email providers. `google_client_secret`: None keeps it,
    /// `""` clears it. This device takes the change up at once.
    pub async fn admin_set_providers(&self, providers: Providers, google_client_secret: Option<String>) -> Result<AdminConfig, CloudError> {
        let c = &self.inner;
        let req = AdminProvidersUpdate { providers, google_client_secret };
        let out: AdminConfig = c.authed(Method::PUT, "/v1/admin/providers", Some(&req)).await?;
        c.after_admin_change().await;
        Ok(out)
    }

    /// Save the AI setup. `api_key`: None keeps it, `""` clears it.
    pub async fn admin_set_ai(&self, ai: OrgAi, api_key: Option<String>) -> Result<AdminConfig, CloudError> {
        let c = &self.inner;
        let req = AdminAiUpdate { ai, api_key };
        let out: AdminConfig = c.authed(Method::PUT, "/v1/admin/ai", Some(&req)).await?;
        c.after_admin_change().await;
        Ok(out)
    }

    /// The models a provider offers, for an AI setup that may be unsaved.
    /// Without `api_key`, the organisation's saved key.
    pub async fn admin_ai_models(&self, ai: OrgAi, api_key: Option<String>) -> anyhow::Result<Vec<String>> {
        let (settings, key) = self.inner.admin_ai_trial(ai, api_key).await?;
        self.inner.engine.ai_models(settings, key).await
    }

    /// A tiny request with an AI setup that may be unsaved.
    pub async fn admin_ai_test(&self, ai: OrgAi, api_key: Option<String>) -> anyhow::Result<String> {
        let (settings, key) = self.inner.admin_ai_trial(ai, api_key).await?;
        self.inner.engine.ai_test(settings, key).await
    }

    /// The organisation's people (account metadata only), owners and admins first.
    pub async fn admin_users(&self, query: Option<&str>) -> Result<Vec<AdminUser>, CloudError> {
        let path = match query.map(str::trim).filter(|q| !q.is_empty()) {
            Some(q) => format!("/v1/admin/users?query={}", url_encode(q)),
            None => "/v1/admin/users".into(),
        };
        self.inner.authed(Method::GET, &path, None::<&()>).await
    }

    /// Make someone an admin (`admin`) or take it back (`user`).
    pub async fn admin_set_role(&self, user_id: &str, role: &str) -> Result<AdminUser, CloudError> {
        let c = &self.inner;
        let id: String = user_id.chars().filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-').collect();
        let out: AdminUser = c.authed(Method::PUT, &format!("/v1/admin/users/{id}/role"), Some(&RoleUpdate { role: role.into() })).await?;
        // One's own role changed: the profile says so.
        if c.state.lock().unwrap().session.as_ref().is_some_and(|s| s.user_id == out.id) {
            if let Ok(me) = c.authed::<Me>(Method::GET, "/v1/me", None::<&()>).await {
                c.remember_me(&me);
            }
        }
        Ok(out)
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
        ai: org.config.ai.clone(),
    }
}

/// Percent-encodes a query value.
fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
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
            vault: match (&st.session, st.me.as_ref().and_then(|m| m.vault_key_id.as_ref())) {
                (None, _) => VaultState::SignedOut,
                (Some(_), Some(id)) if st.device_vault.as_ref() == Some(id) => VaultState::Ready,
                (Some(_), _) => VaultState::Locked,
            },
        }
    }

    /// Keeps the vault key a sign-in brought (unwrapped with the password's
    /// keys, or the one made to convert the account).
    fn take_vault(&self, t: &Tokens) -> Result<(), CloudError> {
        let pending = self.pending_vault.lock().unwrap().take();
        let Some(setup) = &t.vault else { return Ok(()) };
        let vk = match pending {
            Some(PendingVault::Have(vk)) if vk.id == setup.key_id => vk,
            Some(PendingVault::Unwrap(keys)) => match vault::unwrap(&keys, setup) {
                Some(vk) => vk,
                None => {
                    // The proof was right, so this shouldn't happen; the device asks for the password later.
                    tracing::warn!("the vault key didn't unwrap with this password");
                    return Ok(());
                }
            },
            _ => return Ok(()),
        };
        self.keep_vault(&vk)
    }

    fn keep_vault(&self, vk: &vault::VaultKey) -> Result<(), CloudError> {
        vault::save(&self.engine, vk)?;
        self.update(|st| st.device_vault = Some(vk.id.clone()));
        Ok(())
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
        let ai = org.config.ai.clone();
        let version = org.version;
        self.update(|st| {
            st.org = Some(org);
            st.providers = Some(providers);
        });
        self.sync_ai_key(&ai, version).await;
        Ok(())
    }

    /// Keep this device's copy of the organisation's AI key current: fetched
    /// when org AI is on and the config version moved on (a new key bumps
    /// it), removed when org AI is off.
    async fn sync_ai_key(self: &Arc<Self>, ai: &OrgAi, version: i64) {
        let store = self.engine.store();
        let secrets = self.engine.secrets();
        if !ai.enabled {
            let had = secrets.get(crate::ai::ORG_KEY_SECRET_ID).ok().flatten().is_some();
            if let Err(e) = secrets.delete(crate::ai::ORG_KEY_SECRET_ID) {
                tracing::warn!("couldn't remove the organisation's AI key: {e:#}");
            }
            let _ = store.delete_setting(AI_KEY_VERSION);
            if had {
                self.engine.ai_changed();
            }
            return;
        }
        let at: Option<i64> = store.setting(AI_KEY_VERSION);
        if at == Some(version) {
            return;
        }
        if let Err(e) = self.fetch_ai_key().await {
            tracing::info!("couldn't fetch the organisation's AI key: {e}");
        }
    }

    /// `GET /v1/org/ai-key` into the secret store (none: the organisation
    /// has no key, e.g. a local model). Returns the key.
    async fn fetch_ai_key(self: &Arc<Self>) -> Result<Option<String>, CloudError> {
        let version = self.state.lock().unwrap().org.as_ref().map(|o| o.version);
        let key = match self.authed::<OrgAiKey>(Method::GET, "/v1/org/ai-key", None::<&()>).await {
            Ok(k) => Some(k.api_key),
            Err(e) if matches!(e.code.as_str(), "not_found" | "forbidden") => None,
            Err(e) => return Err(e),
        };
        let secrets = self.engine.secrets();
        let before = secrets.get(crate::ai::ORG_KEY_SECRET_ID).ok().flatten();
        let saved = match &key {
            Some(k) => secrets.set(crate::ai::ORG_KEY_SECRET_ID, k),
            None => secrets.delete(crate::ai::ORG_KEY_SECRET_ID),
        };
        saved.map_err(|e| CloudError::new("internal", format!("couldn't keep the AI key: {e:#}")))?;
        if let Some(v) = version {
            let _ = self.engine.store().set_setting(AI_KEY_VERSION, &v);
        }
        if before != key {
            self.engine.ai_changed();
        }
        Ok(key)
    }

    /// An admin saved org settings: take them up here now (other devices get
    /// the change notice).
    async fn after_admin_change(self: &Arc<Self>) {
        if let Err(e) = self.refresh_org().await {
            tracing::info!("couldn't reload the organisation's settings: {e}");
        }
    }

    /// Settings and key for trying an AI setup an admin may not have saved.
    async fn admin_ai_trial(self: &Arc<Self>, ai: OrgAi, api_key: Option<String>) -> anyhow::Result<(crate::ai::AiSettings, Option<String>)> {
        let key = match api_key.filter(|k| !k.trim().is_empty()) {
            Some(k) => Some(k.trim().to_string()),
            // The saved key (admins may read it while AI is off).
            None => match self.authed::<OrgAiKey>(Method::GET, "/v1/org/ai-key", None::<&()>).await {
                Ok(k) => Some(k.api_key),
                Err(e) if e.code == "not_found" => None,
                Err(e) => return Err(e.into()),
            },
        };
        let settings = crate::ai::AiSettings {
            enabled: true,
            provider: ai.provider,
            base_url: ai.base_url,
            model: ai.model,
            max_tokens: ai.max_tokens,
            ..Default::default()
        };
        if key.is_none() && !settings.is_local() {
            anyhow::bail!("Enter the API key first.");
        }
        Ok((settings, key))
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
        vault::forget(&self.engine);
        self.pending_vault.lock().unwrap().take();
        if let Err(e) = self.engine.wipe_setup() {
            tracing::warn!("couldn't remove the synced setup: {e:#}");
        }
        self.engine.stop_mcp().await;
        self.engine.set_org(None);
        if let Err(e) = self.engine.secrets().delete(crate::ai::ORG_KEY_SECRET_ID) {
            tracing::warn!("couldn't delete the organisation's AI key: {e:#}");
        }
        let store = self.engine.store();
        for key in [ME_KEY, FLAGS_KEY, AI_KEY_VERSION] {
            let _ = store.delete_setting(key);
        }
        self.update(|st| {
            st.me = None;
            st.phase = Some(Phase::SignedOut);
            st.progress = None;
            st.offline = false;
            st.org = None;
            st.providers = None;
            st.device_vault = None;
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
