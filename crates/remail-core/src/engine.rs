//! The engine ties storage, IMAP workers, SMTP and the local schedulers
//! together behind one API that the desktop shell calls.
//!
//! Each account gets a worker task that owns its IMAP connection and runs
//! commands one at a time (sync, queued server ops, body fetches, appends),
//! plus an IDLE watcher on a second connection that pokes the worker when
//! new mail lands. Two engine-wide loops deliver the outbox (undo send / send
//! later) and wake snoozed threads.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::time::Instant;

use tokio_util::sync::CancellationToken;

use crate::ai::{self, AiSettings};
use crate::assistant::{self, ChatEvent};
use crate::cloud::CloudError;
use crate::compose;
use crate::imap::{self, ImapConn};
use crate::mcp::client::{McpHub, McpServerConfig, McpTransport};
use crate::mcp::server::{McpServerHandle, ToolHandler};
use crate::mcp::{self as mcpmod, ServerInfo, ToolInfo, ToolOutput};
use crate::model::*;
use crate::oauth;
use crate::presets;
use crate::render;
use crate::secrets::SecretStore;
use crate::smtp::{self, SendError};
use crate::store::{new_id, now_ms, Store};
use crate::sync::{self, ServerOp};
use remail_proto::org::{ProviderKind as OrgKind, Providers};
use remail_proto::sections;

/// Receives engine events (the desktop shell forwards them to the UI).
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: EngineEvent);
}

/// User preferences, stored as one JSON value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    /// 0 disables undo send.
    pub undo_send_seconds: u32,
    /// `system` | `light` | `dark`.
    pub theme: String,
    pub load_remote_images: bool,
    pub notifications: bool,
    pub sync_interval_seconds: u64,
    /// `reply` | `replyAll` — what `r` / the primary reply button does.
    pub default_reply: String,
    pub mark_read_on_open: bool,
    pub notify_on_snooze_wake: bool,
    pub snooze_later_minutes: i64,
    pub snooze_afternoon: String,
    pub snooze_evening: String,
    pub snooze_morning: String,
    pub snooze_tomorrow: String,
    pub snooze_day_after: String,
    pub snooze_saturday: String,
    pub snooze_monday: String,
    /// Show a confirmation before sending.
    pub confirm_before_send: bool,
    /// Closing the main window keeps Remail running in the tray.
    pub close_to_tray: bool,
    /// What opens after archiving/deleting the open email:
    /// `nextUnread` | `next` | `previous` | `list`.
    pub after_remove: String,
    /// `split` (list + reading pane) or `list` (full-width list, email on top).
    pub layout: String,
    /// Split the Inbox into Important and Other.
    pub smart_inbox: bool,
    /// Only notify for mail that lands in Important.
    pub notify_important_only: bool,
    pub auto_clean: AutoClean,
    pub calendar: crate::calendar::CalendarPrefs,
    /// `en` | `ar` | `system`.
    pub language: String,
    // The email providers and the Microsoft app id used to live here; they
    // come from the organisation now (`OrgAccess`). Old values are ignored.
}

/// What the engine needs from the Remail server connection (`cloud::Cloud`
/// installs it).
pub trait CloudLink: Send + Sync {
    /// Local setup in `section` changed: sync it soon.
    fn mark_dirty(&self, section: &str);
    /// Trade a Google sign-in code through the server, which holds the client secret.
    fn google_exchange(&self, req: remail_proto::api::GoogleExchange) -> futures::future::BoxFuture<'static, Result<remail_proto::api::GoogleTokens>>;
    /// Refresh a Google sign-in through the server (same client secret).
    fn google_refresh(&self, refresh_token: String) -> futures::future::BoxFuture<'static, Result<remail_proto::api::GoogleTokens>>;
}

/// The organisation's email providers, as the engine enforces them when
/// mailboxes are added (the server checks again when they sync).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OrgAccess {
    pub providers: Providers,
    /// Google codes are exchanged by the server (it has the client secret).
    pub google_server_exchange: bool,
}

/// Which provider an account belongs to, for the allow-list.
pub fn account_provider(email: &str, imap_host: &str, auth: &str) -> OrgKind {
    match auth {
        oauth::GOOGLE => OrgKind::Google,
        oauth::MICROSOFT => OrgKind::Microsoft,
        _ => match presets::provider_kind(email, imap_host) {
            presets::ProviderKind::Gmail => OrgKind::Google,
            presets::ProviderKind::Outlook => OrgKind::Microsoft,
            presets::ProviderKind::Other => OrgKind::Imap,
        },
    }
}

fn not_allowed(kind: OrgKind) -> anyhow::Error {
    let what = match kind {
        OrgKind::Google => "Google accounts",
        OrgKind::Microsoft => "Microsoft accounts",
        OrgKind::Imap => "Accounts from this provider",
    };
    CloudError::new("provider_not_allowed", format!("{what} aren't allowed by your organisation.")).into()
}

/// Window and sidebar choices that follow the user (they lived in the
/// webview's localStorage before the Remail server).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiPrefs {
    /// Action id → key bindings.
    pub shortcuts: BTreeMap<String, Vec<String>>,
    /// Hidden sidebar entries (`SPAM`, `folder:<account>:<label>`); None = never chosen.
    pub sidebar_hidden: Option<Vec<String>>,
    pub last_compose_account: Option<String>,
    /// Phone: the views pinned to the bottom bar, in order; None = the defaults.
    pub mobile_pins: Option<Vec<String>>,
    /// Phone: list swipe gesture → action (`shortRight`, `longRight`, `shortLeft`, `longLeft`).
    pub swipe: Option<BTreeMap<String, String>>,
}

/// A partial `UiPrefs`: a missing key keeps its value, `null` clears it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiPrefsPatch {
    pub shortcuts: Option<BTreeMap<String, Vec<String>>>,
    #[serde(deserialize_with = "present")]
    pub sidebar_hidden: Option<Option<Vec<String>>>,
    #[serde(deserialize_with = "present")]
    pub last_compose_account: Option<Option<String>>,
    #[serde(deserialize_with = "present")]
    pub mobile_pins: Option<Option<Vec<String>>>,
    #[serde(deserialize_with = "present")]
    pub swipe: Option<Option<BTreeMap<String, String>>>,
}

/// `Some(value)` for a key that is present, even when it's `null`.
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> std::result::Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// Settings key of `UiPrefs`.
pub(crate) const UI_PREFS: &str = "ui";

/// Remove read mail of chosen kinds from the Inbox automatically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AutoClean {
    pub enabled: bool,
    /// `trash` or `archive`.
    pub action: String,
    pub rules: Vec<CleanRule>,
}

impl Default for AutoClean {
    fn default() -> Self {
        Self { enabled: false, action: "trash".into(), rules: vec![] }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CleanRule {
    /// `promotions` | `updates` | `social` | `forums` | `otp`.
    pub kind: String,
    /// 0 = as soon as you've read it and moved on.
    pub after_minutes: i64,
    /// When the rule was turned on; mail read earlier is left alone.
    pub since: i64,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            undo_send_seconds: 5,
            theme: "light".into(),
            load_remote_images: false,
            notifications: true,
            sync_interval_seconds: 15,
            default_reply: "reply".into(),
            mark_read_on_open: true,
            notify_on_snooze_wake: true,
            snooze_later_minutes: 120,
            snooze_afternoon: "15:00".into(),
            snooze_evening: "18:00".into(),
            snooze_morning: "08:00".into(),
            snooze_tomorrow: "10:00".into(),
            snooze_day_after: "11:00".into(),
            snooze_saturday: "10:00".into(),
            snooze_monday: "10:00".into(),
            confirm_before_send: false,
            close_to_tray: false,
            after_remove: "nextUnread".into(),
            layout: "split".into(),
            smart_inbox: true,
            notify_important_only: false,
            auto_clean: AutoClean::default(),
            calendar: Default::default(),
            language: "system".into(),
        }
    }
}

impl AppSettings {
    pub fn snooze_times(&self) -> presets::SnoozeTimes {
        let d = presets::SnoozeTimes::default();
        let t = |s: &str, fallback| presets::parse_hhmm(s).unwrap_or(fallback);
        presets::SnoozeTimes {
            later_minutes: self.snooze_later_minutes,
            afternoon: t(&self.snooze_afternoon, d.afternoon),
            evening: t(&self.snooze_evening, d.evening),
            morning: t(&self.snooze_morning, d.morning),
            tomorrow: t(&self.snooze_tomorrow, d.tomorrow),
            day_after: t(&self.snooze_day_after, d.day_after),
            saturday: t(&self.snooze_saturday, d.saturday),
            monday: t(&self.snooze_monday, d.monday),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRef {
    pub account_id: String,
    pub thread_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadAction {
    Archive,
    /// Back to the inbox from wherever the thread is shown (`from` label).
    #[serde(rename_all = "camelCase")]
    MoveToInbox { from: Option<String> },
    Trash,
    Spam,
    NotSpam,
    #[serde(rename_all = "camelCase")]
    Move { from: Option<String>, to: String },
    MarkRead,
    MarkUnread,
    Star,
    Unstar,
    DeleteForever,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsubscribeResult {
    /// `one-click` | `url` | `email` | `none`
    pub method: String,
    /// For `url`: the page the UI should open in the browser.
    pub url: Option<String>,
    pub error: Option<String>,
}

/// One-shot AI writing tasks.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AiTask {
    #[serde(rename_all = "camelCase")]
    Summarize { account_id: String, thread_id: String },
    /// A reply draft (recipients, quote and signature included) with an AI-written body.
    #[serde(rename_all = "camelCase")]
    Reply { message_id: String, kind: ReplyKind, instructions: String },
    #[serde(rename_all = "camelCase")]
    Compose { account_id: Option<String>, instructions: String, to: Vec<EmailAddress>, subject: String, current: String },
    /// `mode`: improve | fix | shorter | longer | formal | friendly | translate:<language> | custom.
    #[serde(rename_all = "camelCase")]
    Rewrite { account_id: Option<String>, text: String, mode: String, instructions: String },
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiOutput {
    pub text: String,
    pub html: Option<String>,
    pub subject: Option<String>,
    pub draft: Option<Draft>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentData {
    pub filename: String,
    pub mime_type: String,
    pub data_base64: String,
}

/// User-facing explanation for connection/login failures.
pub fn friendly_error(e: &anyhow::Error) -> String {
    // `cloud:<code>:<message>`: the UI translates the code.
    if let Some(c) = e.chain().find_map(|c| c.downcast_ref::<CloudError>()) {
        return c.to_string();
    }
    if let Some(s) = e.downcast_ref::<oauth::SignInRequired>() {
        return s.0.clone();
    }
    if imap::is_auth_error(e) {
        return "The server rejected the username or password. Many providers (Gmail, iCloud, Yahoo) need an app password.".into();
    }
    if let Some(s) = e.downcast_ref::<SendError>() {
        return match s {
            SendError::Auth(_) => "The SMTP server rejected the username or password.".into(),
            other => other.to_string(),
        };
    }
    let chain = format!("{e:#}");
    if chain.contains("certificate") || chain.contains("InvalidCertificate") {
        return format!("The server's TLS certificate could not be verified ({chain}).");
    }
    chain
}

enum Cmd {
    Sync { periodic: bool, reply: Option<oneshot::Sender<Result<()>>> },
    FlushOps,
    LoadMore { label: String, reply: oneshot::Sender<Result<(usize, bool)>> },
    ServerSearch { query: String, reply: oneshot::Sender<Result<usize>> },
    FetchBody { message_id: String, reply: oneshot::Sender<Result<Option<Vec<u8>>>> },
    Append { label: String, flags: String, raw: Vec<u8>, message_id: String, reply: oneshot::Sender<Result<Option<String>>> },
    CreateFolder { name: String, reply: oneshot::Sender<Result<()>> },
}

struct Worker {
    tx: mpsc::UnboundedSender<Cmd>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

pub(crate) struct Inner {
    store: Store,
    secrets: Arc<dyn SecretStore>,
    sink: Arc<dyn EventSink>,
    workers: Mutex<HashMap<String, Worker>>,
    outbox_wake: Notify,
    snooze_wake: Notify,
    runtime: Handle,
    http: reqwest::Client,
    ai: AiState,
    /// Profile directory (None for in-memory test engines).
    data_dir: Option<PathBuf>,
    mcp_hub: McpHub,
    mcp_server: Mutex<Option<McpServerHandle>>,
    mcp_error: Mutex<Option<String>>,
    /// Conversation open in the UI (auto-clean waits until you move on).
    viewing: Mutex<Option<ThreadRef>>,
    clean_wake: Notify,
    pub(crate) cal: crate::calendar::CalState,
    /// OAuth access tokens: account id → (token, expires at ms).
    oauth_tokens: Mutex<HashMap<String, (String, i64)>>,
    /// One token refresh at a time (refresh tokens may rotate).
    oauth_refresh: tokio::sync::Mutex<()>,
    /// Cancels a sign-in that is waiting for the browser.
    oauth_cancel: Mutex<Option<CancellationToken>>,
    /// Set while mail may run (signed in to Remail): account workers and the
    /// schedulers live under it, and cancelling it stops them.
    gate: Mutex<Option<CancellationToken>>,
    /// Accounts whose worker has started a sync since the gate opened.
    sync_started: Mutex<HashSet<String>>,
    /// The Remail server connection, when there is one.
    link: Mutex<Option<Weak<dyn CloudLink>>>,
    /// The organisation's providers; None = no organisation (tests, local use).
    org: Mutex<Option<OrgAccess>>,
    /// One server save at a time per draft (by draft id), so two saves can't
    /// both replace the same previous copy.
    draft_saves: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// MCP: Remail as a server for other agents, and other servers for Remail's assistant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct McpSettings {
    /// Let AI agents in other apps use Remail.
    pub server_enabled: bool,
    pub port: u16,
    pub permissions: assistant::Permissions,
    /// Outside MCP servers whose tools the assistant may use.
    pub servers: Vec<McpServerConfig>,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self { server_enabled: false, port: 47311, permissions: assistant::Permissions::default(), servers: vec![] }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub running: bool,
    pub url: Option<String>,
    pub token: String,
    pub error: Option<String>,
}

const MCP_TOKEN_ID: &str = "mcp-server-token";

/// Assistant bookkeeping: running requests, pending approvals, chat histories.
#[derive(Default)]
struct AiState {
    runs: Mutex<HashMap<String, CancellationToken>>,
    approvals: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    chats: Mutex<HashMap<String, Vec<ai::Message>>>,
    /// Thread summaries keyed by `account|thread|latest message`.
    summaries: Mutex<HashMap<String, String>>,
    sort_wake: Notify,
    /// Conversations that have seen email content: outside tools always ask there.
    untrusted: Mutex<std::collections::HashSet<String>>,
    /// Shared by every AI request (one connection pool; `Inner::http` has a short total timeout).
    http: std::sync::OnceLock<reqwest::Client>,
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

const OP_MAX_ATTEMPTS: i64 = 5;
const SEND_RETRY_DELAYS_MS: [i64; 3] = [60_000, 300_000, 900_000];
const POLL_BACKOFF_MAX: Duration = Duration::from_secs(300);
const IDLE_REFRESH: Duration = Duration::from_secs(25 * 60);

impl Engine {
    /// Open the database in `data_dir` and start background work. Must be
    /// called from inside a tokio runtime.
    pub fn start(data_dir: PathBuf, secrets: Arc<dyn SecretStore>, sink: Arc<dyn EventSink>) -> Result<Engine> {
        let engine = Self::start_locked(data_dir, secrets, sink)?;
        engine.open_gate()?;
        Ok(engine)
    }

    /// Open the database without starting any mail: no account workers and
    /// no schedulers until `open_gate` (the desktop app waits for the Remail
    /// sign-in).
    pub fn start_locked(data_dir: PathBuf, secrets: Arc<dyn SecretStore>, sink: Arc<dyn EventSink>) -> Result<Engine> {
        let store = Store::open(&data_dir.join("remail.db"))?;
        Self::build(store, secrets, sink, Some(data_dir))
    }

    pub fn with_store(store: Store, secrets: Arc<dyn SecretStore>, sink: Arc<dyn EventSink>) -> Result<Engine> {
        let engine = Self::with_store_locked(store, secrets, sink)?;
        engine.open_gate()?;
        Ok(engine)
    }

    pub fn with_store_locked(store: Store, secrets: Arc<dyn SecretStore>, sink: Arc<dyn EventSink>) -> Result<Engine> {
        Self::build(store, secrets, sink, None)
    }

    fn build(store: Store, secrets: Arc<dyn SecretStore>, sink: Arc<dyn EventSink>, data_dir: Option<PathBuf>) -> Result<Engine> {
        store.reset_stuck_outbox()?;
        let inner = Arc::new(Inner {
            store,
            secrets,
            sink,
            workers: Mutex::new(HashMap::new()),
            outbox_wake: Notify::new(),
            snooze_wake: Notify::new(),
            runtime: Handle::current(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .user_agent("Remail")
                .build()?,
            ai: AiState::default(),
            data_dir,
            mcp_hub: McpHub::new("Remail", env!("CARGO_PKG_VERSION")),
            mcp_server: Mutex::new(None),
            mcp_error: Mutex::new(None),
            viewing: Mutex::new(None),
            clean_wake: Notify::new(),
            cal: Default::default(),
            oauth_tokens: Mutex::new(HashMap::new()),
            oauth_refresh: tokio::sync::Mutex::new(()),
            oauth_cancel: Mutex::new(None),
            draft_saves: Mutex::new(HashMap::new()),
            gate: Mutex::new(None),
            sync_started: Mutex::new(HashSet::new()),
            link: Mutex::new(None),
            org: Mutex::new(None),
        });
        let engine = Engine { inner };
        // Local upkeep only; nothing here talks to a mail server.
        engine.inner.runtime.spawn(categorize_backlog(Arc::downgrade(&engine.inner)));
        engine.inner.runtime.spawn(db_maintenance(Arc::downgrade(&engine.inner)));
        Ok(engine)
    }

    /// Start mail: a worker per account and the schedulers (outbox, snooze,
    /// AI sorting, auto-clean, calendar). Does nothing when already open.
    pub fn open_gate(&self) -> Result<()> {
        let accounts = self.inner.store.accounts()?;
        let token = {
            let mut gate = self.inner.gate.lock().unwrap();
            if gate.is_some() {
                return Ok(());
            }
            let t = CancellationToken::new();
            *gate = Some(t.clone());
            t
        };
        self.inner.sync_started.lock().unwrap().clear();
        for account in accounts {
            self.spawn_worker(&account.id);
        }
        let weak = || Arc::downgrade(&self.inner);
        self.spawn_gated(&token, outbox_loop(weak()));
        self.spawn_gated(&token, snooze_loop(weak()));
        self.spawn_gated(&token, ai_sort_loop(weak()));
        self.spawn_gated(&token, autoclean_loop(weak()));
        self.spawn_gated(&token, crate::calendar::calendar_loop(weak()));
        Ok(())
    }

    /// Stop every account worker and scheduler (signed out of Remail).
    pub fn close_gate(&self) {
        if let Some(t) = self.inner.gate.lock().unwrap().take() {
            t.cancel();
        }
        self.inner.workers.lock().unwrap().clear();
        self.inner.sync_started.lock().unwrap().clear();
    }

    pub fn gate_open(&self) -> bool {
        self.inner.gate.lock().unwrap().is_some()
    }

    fn spawn_gated(&self, token: &CancellationToken, work: impl std::future::Future<Output = ()> + Send + 'static) {
        let token = token.clone();
        self.inner.runtime.spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {}
                _ = work => {}
            }
        });
    }

    /// Whether a worker runs for the account (mail started, account here).
    pub fn worker_running(&self, account_id: &str) -> bool {
        self.inner.workers.lock().unwrap().get(account_id).is_some_and(|w| w.tasks.iter().any(|t| !t.is_finished()))
    }

    /// Whether the account's worker has started a sync since mail started.
    pub fn sync_started(&self, account_id: &str) -> bool {
        self.inner.sync_started.lock().unwrap().contains(account_id)
    }

    /// Connect the engine to the Remail server (setup changes are synced through it).
    pub fn set_cloud_link(&self, link: Weak<dyn CloudLink>) {
        *self.inner.link.lock().unwrap() = Some(link);
    }

    fn link(&self) -> Option<Arc<dyn CloudLink>> {
        self.inner.link.lock().unwrap().as_ref().and_then(Weak::upgrade)
    }

    /// Setup in `section` changed here: the Remail server should get it.
    pub(crate) fn mark_dirty(&self, section: &str) {
        mark_dirty(&self.inner, section);
    }

    /// Save an account's or calendar account's credential (and sync it).
    pub(crate) fn set_credential(&self, owner_id: &str, secret: &str) -> Result<()> {
        set_credential(&self.inner, owner_id, secret)
    }

    /// The organisation's provider rules (from the Remail server's org config).
    pub fn set_org(&self, org: Option<OrgAccess>) {
        *self.inner.org.lock().unwrap() = org;
    }

    pub fn org(&self) -> Option<OrgAccess> {
        self.inner.org.lock().unwrap().clone()
    }

    /// Refuse providers (and, for other providers, servers) the organisation doesn't allow.
    fn check_provider(&self, kind: OrgKind, imap_host: &str) -> Result<()> {
        let Some(org) = self.org() else { return Ok(()) };
        let ok = org.providers.allows(kind) && (kind != OrgKind::Imap || org.providers.allows_imap_host(imap_host.trim()));
        if ok {
            Ok(())
        } else {
            Err(not_allowed(kind))
        }
    }

    // ------------------------------------------------------------------
    // Window and sidebar preferences
    // ------------------------------------------------------------------

    pub fn ui_prefs(&self) -> UiPrefs {
        self.inner.store.setting(UI_PREFS).unwrap_or_default()
    }

    pub fn set_ui_prefs(&self, patch: UiPrefsPatch) -> Result<UiPrefs> {
        let mut p = self.ui_prefs();
        let before = p.clone();
        if let Some(s) = patch.shortcuts {
            p.shortcuts = s;
        }
        if let Some(h) = patch.sidebar_hidden {
            p.sidebar_hidden = h;
        }
        if let Some(a) = patch.last_compose_account {
            p.last_compose_account = a;
        }
        if let Some(x) = patch.mobile_pins {
            p.mobile_pins = x;
        }
        if let Some(x) = patch.swipe {
            p.swipe = x;
        }
        self.inner.store.set_setting(UI_PREFS, &p)?;
        if p.shortcuts != before.shortcuts {
            self.mark_dirty(sections::SHORTCUTS);
        }
        if p.sidebar_hidden != before.sidebar_hidden
            || p.last_compose_account != before.last_compose_account
            || p.mobile_pins != before.mobile_pins
            || p.swipe != before.swipe
        {
            self.mark_dirty(sections::PREFS);
        }
        // Hidden folders travel with their accounts (`hiddenFolders`).
        let folders = |h: &Option<Vec<String>>| -> HashSet<String> { h.iter().flatten().filter(|e| e.starts_with("folder:")).cloned().collect() };
        if folders(&p.sidebar_hidden) != folders(&before.sidebar_hidden) {
            self.mark_dirty(sections::ACCOUNTS);
        }
        Ok(p)
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub(crate) fn from_inner(inner: Arc<Inner>) -> Engine {
        Engine { inner }
    }

    pub(crate) fn inner_ref(&self) -> &Inner {
        &self.inner
    }

    pub(crate) fn secrets(&self) -> &Arc<dyn SecretStore> {
        &self.inner.secrets
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    pub(crate) fn emit_event(&self, e: EngineEvent) {
        self.inner.sink.emit(e);
    }

    fn emit(&self, e: EngineEvent) {
        self.inner.sink.emit(e);
    }

    pub fn settings(&self) -> AppSettings {
        self.inner.store.setting("app").unwrap_or_default()
    }

    pub fn set_settings(&self, s: &AppSettings) -> Result<AppSettings> {
        let before = self.settings();
        let mut s = s.clone();
        // A rule (or the whole feature) switched on starts counting now.
        let now = now_ms();
        for r in &mut s.auto_clean.rules {
            let was = before.auto_clean.rules.iter().find(|b| b.kind == r.kind);
            if r.since == 0 || was.is_none() || (s.auto_clean.enabled && !before.auto_clean.enabled) {
                r.since = now;
            } else if let Some(b) = was {
                r.since = b.since;
            }
        }
        self.inner.store.set_setting("app", &s)?;
        self.inner.clean_wake.notify_one();
        self.mark_dirty(sections::PREFS);
        // Calendar preferences sync with the calendars.
        self.mark_dirty(sections::CALENDARS);
        Ok(s)
    }

    // ------------------------------------------------------------------
    // Accounts
    // ------------------------------------------------------------------

    /// Start the account's worker (only while mail runs, see `open_gate`).
    fn spawn_worker(&self, account_id: &str) {
        if !self.gate_open() {
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let weak = Arc::downgrade(&self.inner);
        let main = self.inner.runtime.spawn(worker_loop(weak.clone(), account_id.to_string(), rx));
        let idle = self.inner.runtime.spawn(idle_loop(weak, account_id.to_string(), tx.clone()));
        self.inner.workers.lock().unwrap().insert(account_id.to_string(), Worker { tx, tasks: vec![main, idle] });
    }

    fn send_cmd(&self, account_id: &str, cmd: Cmd) -> Result<()> {
        let workers = self.inner.workers.lock().unwrap();
        let w = workers.get(account_id).ok_or_else(|| anyhow!("unknown account"))?;
        w.tx.send(cmd).map_err(|_| anyhow!("account worker stopped"))
    }

    async fn request<T>(&self, account_id: &str, make: impl FnOnce(oneshot::Sender<Result<T>>) -> Cmd) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(account_id, make(tx))?;
        rx.await.map_err(|_| anyhow!("account worker stopped"))?
    }

    pub fn provider_presets(&self) -> Vec<presets::ProviderPreset> {
        presets::provider_presets()
    }

    /// Suggested server settings for an email address.
    pub fn guess_servers(&self, email: &str) -> (ServerConfig, ServerConfig) {
        presets::guess_servers(email)
    }

    /// Log in to IMAP and SMTP with the given settings. Returns whether the
    /// server is Gmail.
    pub async fn verify_account(&self, a: &NewAccount) -> Result<bool> {
        let username = if a.username.trim().is_empty() { a.email.trim() } else { a.username.trim() };
        let login = Login::Password(a.password.clone());
        let gmail = verify_imap(&a.imap, username, &login, a.allow_invalid_certs).await?;
        smtp::verify(&a.smtp, username, &login, a.allow_invalid_certs).await.map_err(|e| anyhow!(e)).context("SMTP")?;
        Ok(gmail)
    }

    pub async fn add_account(&self, a: NewAccount) -> Result<Account> {
        if a.email.trim().is_empty() || !a.email.contains('@') {
            bail!("Enter a valid email address");
        }
        if self.inner.store.accounts()?.iter().any(|x| x.email.eq_ignore_ascii_case(a.email.trim())) {
            bail!("This account is already added");
        }
        self.check_provider(account_provider(&a.email, &a.imap.host, "password"), &a.imap.host)?;
        let gmail = self.verify_account(&a).await?;
        let account = self.inner.store.insert_account(&a, gmail, "password")?;
        self.set_credential(&account.id, &a.password)?;
        self.spawn_worker(&account.id);
        self.account_changed(&account.id, true);
        Ok(account)
    }

    /// Re-authenticate / change server settings.
    pub async fn update_account(&self, id: &str, a: NewAccount) -> Result<()> {
        let existing = self.inner.store.account(id)?.context("account not found")?;
        if existing.auth != "password" {
            // Signed in with OAuth: only the servers can change here (see `reauthorize_account`).
            let login = login_for(&self.inner, &existing).await?;
            verify_imap(&a.imap, &existing.username, &login, a.allow_invalid_certs).await?;
            smtp::verify(&a.smtp, &existing.username, &login, a.allow_invalid_certs).await.map_err(|e| anyhow!(e)).context("SMTP")?;
            let a = NewAccount { username: existing.username.clone(), ..a };
            self.inner.store.update_account_servers(id, &a)?;
            self.inner.workers.lock().unwrap().remove(id);
            self.spawn_worker(id);
            self.mark_dirty(sections::ACCOUNTS);
            return Ok(());
        }
        if !a.imap.host.trim().eq_ignore_ascii_case(existing.imap.host.trim()) {
            self.check_provider(account_provider(&existing.email, &a.imap.host, "password"), &a.imap.host)?;
        }
        let gmail = self.verify_account(&a).await?;
        self.inner.store.update_account_servers(id, &a)?;
        // Moving to or from Gmail changes whether sent mail needs a copy in Sent.
        self.inner.store.set_account_gmail(id, gmail)?;
        self.set_credential(id, &a.password)?;
        // Restart the worker so it reconnects with the new settings.
        self.inner.workers.lock().unwrap().remove(id);
        self.spawn_worker(id);
        self.account_changed(id, true);
        Ok(())
    }

    pub fn remove_account(&self, id: &str) -> Result<()> {
        self.remove_account_here(id)?;
        self.account_changed(id, true);
        Ok(())
    }

    /// Stop and delete an account on this device (its credential included).
    pub(crate) fn remove_account_here(&self, id: &str) -> Result<()> {
        self.inner.workers.lock().unwrap().remove(id);
        self.inner.store.delete_account(id)?;
        self.inner.secrets.delete(id)?;
        let mut cache = self.inner.oauth_tokens.lock().unwrap();
        cache.remove(id);
        cache.remove(&token_key(id, Service::Calendar));
        drop(cache);
        // Its hidden folders went with it (they sync as part of its record).
        let mut ui = self.ui_prefs();
        let prefix = format!("folder:{id}:");
        if let Some(h) = ui.sidebar_hidden.as_mut().filter(|h| h.iter().any(|e| e.starts_with(&prefix))) {
            h.retain(|e| !e.starts_with(&prefix));
            self.inner.store.set_setting(UI_PREFS, &ui)?;
        }
        self.emit(EngineEvent::MailboxChanged { account_id: id.to_string() });
        Ok(())
    }

    /// An account was added, edited or removed here (`credential`: and its sign-in).
    fn account_changed(&self, id: &str, credential: bool) {
        self.mark_dirty(sections::ACCOUNTS);
        if credential {
            self.mark_dirty(&sections::cred_section(id));
        }
        self.emit(EngineEvent::MailboxChanged { account_id: id.to_string() });
    }

    pub fn accounts(&self) -> Result<Vec<Account>> {
        self.inner.store.accounts()
    }

    // ------------------------------------------------------------------
    // Microsoft accounts (Outlook.com, Hotmail, Live, MSN, Microsoft 365)
    // ------------------------------------------------------------------

    /// The app (client) ID for Microsoft sign-in, from the organisation's settings.
    pub fn microsoft_client_id(&self) -> Option<String> {
        let org = self.org()?;
        Some(org.providers.microsoft_app.client_id.trim().to_string()).filter(|s| !s.is_empty())
    }

    /// The client ID for Google sign-in, from the organisation's settings.
    pub fn google_client_id(&self) -> Option<String> {
        let org = self.org()?;
        Some(org.providers.google_app.client_id.trim().to_string()).filter(|s| !s.is_empty())
    }

    /// Sign in at Microsoft in the browser (`open` shows the page) and wait
    /// for it. Mail and calendar are asked for together; if the calendar isn't
    /// allowed (declined, or the organization wants an administrator to
    /// approve it), the browser opens once more for mail alone.
    async fn microsoft_sign_in(&self, open: impl Fn(&str) -> Result<()>, login_hint: Option<&str>) -> Result<(oauth::Tokens, String, CancellationToken)> {
        let client_id = self.microsoft_client_id().context(
            "Microsoft sign-in isn't set up for your organisation yet: an administrator needs to add Remail's app (client) ID.",
        )?;
        let cancel = self.new_sign_in();
        let provider = microsoft_provider(self.org().map(|o| o.providers.microsoft_app.tenant).as_deref());
        let tokens = match self.browser_sign_in(&provider, &client_id, login_hint, &open, &cancel).await {
            Err(e) if !provider.also_consent.is_empty() && e.downcast_ref::<oauth::OAuthError>().is_some_and(|o| o.consent_refused()) => {
                tracing::info!("signing in for mail only: {e:#}");
                let mail_only = oauth::Provider { also_consent: String::new(), ..provider };
                self.browser_sign_in(&mail_only, &client_id, login_hint, &open, &cancel).await?
            }
            r => r?,
        };
        // Still cancellable while the account's servers are checked.
        Ok((tokens, client_id, cancel))
    }

    /// Sign in at Google in the browser. The code is exchanged here (PKCE),
    /// or by the Remail server when the organisation gave it a client secret.
    async fn google_sign_in(&self, open: impl Fn(&str) -> Result<()>, login_hint: Option<&str>) -> Result<(oauth::Tokens, String, CancellationToken)> {
        let client_id = self
            .google_client_id()
            .context("Google sign-in isn't set up for your organisation yet: an administrator needs to add a Google client ID.")?;
        let server_exchange = self.org().is_some_and(|o| o.google_server_exchange);
        let cancel = self.new_sign_in();
        let provider = google_provider();
        let pending = oauth::begin(&provider, &client_id, login_hint).await?;
        open(&pending.url)?;
        let authorized = tokio::select! {
            r = pending.authorized(Duration::from_secs(600)) => r?,
            _ = cancel.cancelled() => bail!("Sign-in cancelled"),
        };
        let tokens = if server_exchange {
            let link = self.link().context("Google sign-in needs the Remail server, which isn't connected")?;
            let req = remail_proto::api::GoogleExchange {
                code: authorized.code.clone(),
                code_verifier: authorized.verifier.clone(),
                redirect_uri: authorized.redirect_uri.clone(),
            };
            tokio::select! {
                r = link.google_exchange(req) => oauth::tokens_from_server(r?),
                _ = cancel.cancelled() => bail!("Sign-in cancelled"),
            }
        } else {
            oauth::exchange(&self.inner.http, &provider, &client_id, &authorized).await?
        };
        Ok((tokens, client_id, cancel))
    }

    /// A new browser sign-in replaces one still waiting.
    fn new_sign_in(&self) -> CancellationToken {
        let cancel = CancellationToken::new();
        if let Some(previous) = self.inner.oauth_cancel.lock().unwrap().replace(cancel.clone()) {
            previous.cancel();
        }
        cancel
    }

    async fn browser_sign_in(
        &self,
        provider: &oauth::Provider,
        client_id: &str,
        login_hint: Option<&str>,
        open: &impl Fn(&str) -> Result<()>,
        cancel: &CancellationToken,
    ) -> Result<oauth::Tokens> {
        let pending = oauth::begin(provider, client_id, login_hint).await?;
        open(&pending.url)?;
        tokio::select! {
            r = pending.finish(&self.inner.http, provider, client_id, Duration::from_secs(600)) => r,
            _ = cancel.cancelled() => bail!("Sign-in cancelled"),
        }
    }

    /// An access token for the account's calendar (Microsoft Graph, Google CalDAV).
    pub(crate) async fn calendar_token(&self, account_id: &str) -> Result<String> {
        access_token(&self.inner, account_id, Service::Calendar).await
    }

    /// The provider refused this calendar token: the next request gets a new one.
    pub(crate) fn forget_calendar_token(&self, account_id: &str, failed: &str) {
        forget_service_token(&self.inner, account_id, Service::Calendar, failed);
    }

    /// Connect (or re-sync) the account's Outlook calendars in the background.
    fn spawn_outlook_calendar(&self, account_id: &str) {
        let engine = self.clone();
        let id = account_id.to_string();
        self.inner.runtime.spawn(async move {
            if let Err(e) = engine.connect_outlook_calendar(&id, false).await {
                tracing::info!("Outlook calendar: {e:#}");
            }
        });
    }

    /// Connect the Google account's calendars in the background (once).
    fn spawn_google_calendar(&self, account_id: &str) {
        let engine = self.clone();
        let id = account_id.to_string();
        self.inner.runtime.spawn(async move {
            let have = engine.cal_accounts().map(|c| c.iter().any(|c| c.mail_account_id.as_deref() == Some(id.as_str()))).unwrap_or(true);
            if have {
                return;
            }
            let req = crate::calendar::CalConnect { mail_account_id: Some(id), url: None, username: None, password: None, allow_invalid_certs: false };
            if let Err(e) = engine.cal_connect(req).await {
                tracing::info!("Google calendar: {e:#}");
            }
        });
    }

    /// Stop waiting for a browser sign-in.
    pub fn cancel_sign_in(&self) {
        if let Some(c) = self.inner.oauth_cancel.lock().unwrap().take() {
            c.cancel();
        }
    }

    /// Add an Outlook.com / Hotmail / Live / MSN or Microsoft 365 account by
    /// signing in at Microsoft. Reading must work; if sending doesn't (a
    /// Microsoft 365 admin can turn SMTP off), the account is added with a warning.
    pub async fn add_microsoft_account(&self, open: impl Fn(&str) -> Result<()>, req: OAuthAccountRequest) -> Result<OAuthAccountAdded> {
        self.check_provider(OrgKind::Microsoft, "")?;
        let (tokens, client_id, cancel) = self.microsoft_sign_in(open, req.email.as_deref()).await?;
        let personal = |email: &str, tokens: &oauth::Tokens| {
            let domain = email.rsplit('@').next().unwrap_or_default().to_string();
            tokens.identity.tenant.as_deref() == Some(oauth::MICROSOFT_CONSUMER_TENANT) || presets::MICROSOFT_DOMAINS.contains(&domain.as_str())
        };
        self.add_oauth_account(oauth::MICROSOFT, tokens, client_id, cancel, req, |email, tokens| presets::microsoft_servers(personal(email, tokens)))
            .await
    }

    /// Add a Gmail or Google Workspace account by signing in at Google: mail
    /// over IMAP/SMTP with XOAUTH2, and its calendars over CalDAV.
    pub async fn add_google_account(&self, open: impl Fn(&str) -> Result<()>, req: OAuthAccountRequest) -> Result<OAuthAccountAdded> {
        self.check_provider(OrgKind::Google, "")?;
        let (tokens, client_id, cancel) = self.google_sign_in(open, req.email.as_deref()).await?;
        self.add_oauth_account(oauth::GOOGLE, tokens, client_id, cancel, req, |_, _| presets::google_servers()).await
    }

    async fn add_oauth_account(
        &self,
        provider: &str,
        tokens: oauth::Tokens,
        client_id: String,
        cancel: CancellationToken,
        req: OAuthAccountRequest,
        servers: impl FnOnce(&str, &oauth::Tokens) -> (ServerConfig, ServerConfig),
    ) -> Result<OAuthAccountAdded> {
        let name = provider_name(provider);
        let email = tokens
            .identity
            .email
            .clone()
            .or_else(|| req.email.clone())
            .map(|e| e.trim().to_lowercase())
            .filter(|e| e.contains('@'))
            .with_context(|| format!("{name} didn't share this account's email address"))?;
        if self.inner.store.accounts()?.iter().any(|x| x.email.eq_ignore_ascii_case(&email)) {
            bail!("{email} is already added");
        }
        let refresh_token = tokens.refresh_token.clone().with_context(|| format!("{name} didn't allow staying signed in (no refresh token)"))?;
        let (imap, smtp) = match (req.imap, req.smtp) {
            (Some(i), Some(s)) => (i, s),
            _ => servers(&email, &tokens),
        };
        let login = Login::OAuth(tokens.access_token.clone());
        verify_imap(&imap, &email, &login, req.allow_invalid_certs).await?;
        let warning = smtp::verify(&smtp, &email, &login, req.allow_invalid_certs).await.err().map(|e| format!("Reading works, but sending doesn't yet: {e}"));
        let display_name = req.display_name.filter(|n| !n.trim().is_empty()).or_else(|| tokens.identity.name.clone()).unwrap_or_default();
        let new = NewAccount {
            email: email.clone(),
            display_name,
            username: email.clone(),
            password: String::new(),
            imap,
            smtp,
            allow_invalid_certs: req.allow_invalid_certs,
        };
        let grant = oauth::Grant { provider: provider.into(), client_id, refresh_token, subject: tokens.identity.subject() };
        if cancel.is_cancelled() {
            bail!("Sign-in cancelled");
        }
        let account = self.inner.store.insert_account(&new, provider == oauth::GOOGLE, provider)?;
        let saved = serde_json::to_string(&grant).map_err(anyhow::Error::from).and_then(|g| self.set_credential(&account.id, &g));
        if let Err(e) = saved {
            let _ = self.inner.store.delete_account(&account.id);
            return Err(e);
        }
        {
            let mut cache = self.inner.oauth_tokens.lock().unwrap();
            if provider == oauth::GOOGLE {
                // Google's token covers the calendar too.
                cache.insert(token_key(&account.id, Service::Calendar), (tokens.access_token.clone(), tokens.expires_at));
            }
            cache.insert(account.id.clone(), (tokens.access_token, tokens.expires_at));
        }
        self.spawn_worker(&account.id);
        if provider == oauth::GOOGLE {
            self.spawn_google_calendar(&account.id);
        } else {
            self.spawn_outlook_calendar(&account.id);
        }
        self.account_changed(&account.id, true);
        let account = self.inner.store.account(&account.id)?.context("account vanished")?;
        Ok(OAuthAccountAdded { account, warning })
    }

    /// Sign an OAuth account in again (expired or revoked sign-in). A
    /// Hotmail/Outlook or Gmail account added with a password is switched to
    /// Microsoft or Google sign-in this way.
    pub async fn reauthorize_account(&self, id: &str, open: impl Fn(&str) -> Result<()>) -> Result<()> {
        let account = self.inner.store.account(id)?.context("account not found")?;
        let converting = account.auth == "password";
        let kind = account_provider(&account.email, &account.imap.host, &account.auth);
        let provider = match kind {
            OrgKind::Google => oauth::GOOGLE,
            OrgKind::Microsoft => oauth::MICROSOFT,
            OrgKind::Imap => bail!("This account signs in with a password"),
        };
        if !converting && account.auth != provider {
            bail!("This account can't sign in with {}", provider_name(provider));
        }
        self.check_provider(kind, &account.imap.host)?;
        let previous: Option<oauth::Grant> =
            if converting { None } else { self.inner.secrets.get(id)?.and_then(|raw| serde_json::from_str(&raw).ok()) };
        let (tokens, client_id, cancel) = if provider == oauth::GOOGLE {
            self.google_sign_in(open, Some(&account.email)).await?
        } else {
            self.microsoft_sign_in(open, Some(&account.email)).await?
        };
        // The same account: by its id when we know it (the address may have changed), else by address.
        let same = match (previous.as_ref().and_then(|g| g.subject.clone()), tokens.identity.subject()) {
            (Some(was), Some(now)) => was == now,
            _ => tokens.identity.email.as_deref().is_none_or(|e| e.eq_ignore_ascii_case(&account.email)),
        };
        if !same {
            let who = tokens.identity.email.clone().unwrap_or_else(|| "a different account".into());
            bail!("You signed in as {who}, but this account is {}. Please sign in with {} instead.", account.email, account.email);
        }
        let refresh_token = tokens
            .refresh_token
            .clone()
            .with_context(|| format!("{} didn't allow staying signed in (no refresh token)", provider_name(provider)))?;
        // Prove the new sign-in opens this mailbox before replacing anything.
        verify_imap(&account.imap, &account.username, &Login::OAuth(tokens.access_token.clone()), account.allow_invalid_certs).await?;
        if cancel.is_cancelled() {
            bail!("Sign-in cancelled");
        }
        let grant = oauth::Grant { provider: provider.into(), client_id, refresh_token, subject: tokens.identity.subject() };
        {
            // A refresh in flight with the old grant must not overwrite the new one.
            let _no_refresh = self.inner.oauth_refresh.lock().await;
            self.set_credential(id, &serde_json::to_string(&grant)?)?;
            if converting {
                self.inner.store.set_account_auth(id, provider)?;
            }
            let mut cache = self.inner.oauth_tokens.lock().unwrap();
            cache.insert(id.to_string(), (tokens.access_token, tokens.expires_at));
            // The calendar may have just been allowed: ask for a new token.
            cache.remove(&token_key(id, Service::Calendar));
        }
        self.inner.store.set_account_status(id, AccountStatus::Active, None)?;
        self.inner.workers.lock().unwrap().remove(id);
        self.spawn_worker(id);
        if provider == oauth::GOOGLE {
            self.spawn_google_calendar(id);
        } else {
            self.spawn_outlook_calendar(id);
        }
        self.account_changed(id, true);
        Ok(())
    }

    pub fn update_account_prefs(
        &self,
        id: &str,
        display_name: Option<String>,
        color: Option<String>,
        hidden_in_unified: Option<bool>,
        auto_bcc: Option<Vec<String>>,
    ) -> Result<()> {
        self.inner.store.update_account_prefs(
            id,
            display_name.as_deref(),
            color.as_deref(),
            hidden_in_unified,
            auto_bcc.as_deref(),
        )?;
        self.account_changed(id, false);
        Ok(())
    }

    pub fn sync_now(&self, account_id: Option<&str>) {
        let workers = self.inner.workers.lock().unwrap();
        for (id, w) in workers.iter() {
            if account_id.is_none_or(|a| a == id) {
                let _ = w.tx.send(Cmd::Sync { periodic: false, reply: None });
            }
        }
    }

    pub async fn sync_and_wait(&self, account_id: &str) -> Result<()> {
        self.request(account_id, |reply| Cmd::Sync { periodic: false, reply: Some(reply) }).await
    }

    // ------------------------------------------------------------------
    // Reading
    // ------------------------------------------------------------------

    pub fn labels(&self, account_id: &str) -> Result<Vec<Label>> {
        self.inner.store.labels(account_id)
    }

    pub fn unread_counts(&self) -> Result<HashMap<String, i64>> {
        self.inner.store.unread_counts(now_ms())
    }

    pub fn list_threads(&self, q: &ThreadQuery) -> Result<Vec<ThreadSummary>> {
        self.inner.store.list_threads(q, now_ms())
    }

    pub async fn load_more(&self, account_id: Option<&str>, label: &str) -> Result<bool> {
        let ids: Vec<String> = match account_id {
            Some(a) => vec![a.to_string()],
            None => self.accounts()?.into_iter().filter(|a| !a.hidden_in_unified).map(|a| a.id).collect(),
        };
        let mut any_more = false;
        for id in ids {
            let label = label.to_string();
            match self.request(&id, |reply| Cmd::LoadMore { label, reply }).await {
                Ok((_, more)) => any_more |= more,
                Err(e) => tracing::warn!("load more failed: {e:#}"),
            }
        }
        Ok(any_more)
    }

    /// Search the servers and pull matches into the local cache.
    pub async fn server_search(&self, account_id: Option<&str>, query: &str) -> Result<usize> {
        let ids: Vec<String> = match account_id {
            Some(a) => vec![a.to_string()],
            None => self.accounts()?.into_iter().map(|a| a.id).collect(),
        };
        let mut total = 0;
        for id in ids {
            let query = query.to_string();
            match self.request(&id, |reply| Cmd::ServerSearch { query, reply }).await {
                Ok(n) => total += n,
                Err(e) => tracing::warn!("server search failed: {e:#}"),
            }
        }
        Ok(total)
    }

    async fn raw_for(&self, message_id: &str) -> Result<Vec<u8>> {
        if let Some(raw) = self.inner.store.raw_body(message_id)? {
            return Ok(raw);
        }
        let loc = self.inner.store.location(message_id)?.context("message not found")?;
        let id = message_id.to_string();
        let raw = tokio::time::timeout(
            Duration::from_secs(60),
            self.request(&loc.account_id, |reply| Cmd::FetchBody { message_id: id, reply }),
        )
        .await
        .context("fetching message timed out")??;
        raw.context("message is no longer on the server")
    }

    async fn message_full(&self, meta: MessageMeta, allow_remote: bool) -> MessageFull {
        let raw = self.raw_for(&meta.id).await;
        let (in_reply_to, references) = self
            .inner
            .store
            .threading_headers(&meta.id)
            .ok()
            .flatten()
            .map(|(_, irt, refs)| (irt, refs))
            .unwrap_or_default();
        // Parsing and sanitising are CPU work: keep them off the async threads.
        let rendered = match raw {
            Ok(r) => tokio::task::spawn_blocking(move || render_body(&r)).await.ok().flatten().ok_or(None),
            Err(e) => Err(Some(e)),
        };
        let (html, text, attachments) = match rendered {
            Ok(r) => r,
            Err(e) => {
                let note = match e {
                    Some(e) => format!("<p><em>Couldn't load this message: {}</em></p>", compose::escape_html(&format!("{e:#}"))),
                    None => String::new(),
                };
                return MessageFull {
                    html: Some(note),
                    text: Some(meta.snippet.clone()),
                    attachments: vec![],
                    in_reply_to,
                    references,
                    has_remote_images: false,
                    meta,
                };
            }
        };
        let has_remote = html.as_deref().is_some_and(render::has_remote_content) && !allow_remote;
        MessageFull { html, text, attachments, in_reply_to, references, has_remote_images: has_remote, meta }
    }

    /// Thread with bodies. Messages present in several folders (Gmail
    /// labels, mail to yourself) appear once; Trash copies are hidden unless
    /// the whole thread is in Trash.
    pub async fn get_thread(&self, account_id: &str, thread_id: &str) -> Result<ThreadDetail> {
        let metas = self.inner.store.thread_messages(account_id, thread_id)?;
        if metas.is_empty() {
            bail!("conversation not found");
        }
        let all_trash = metas.iter().all(|m| m.folder == labels::TRASH || m.folder == labels::SPAM);
        let mut seen = std::collections::HashSet::new();
        let mut picked = Vec::new();
        // Prefer copies that already have a body.
        let mut ordered = metas.clone();
        ordered.sort_by_key(|m| (!m.body_synced, m.folder == labels::SENT));
        for m in ordered {
            if !all_trash && (m.folder == labels::TRASH || m.folder == labels::SPAM) {
                continue;
            }
            let key = m.message_id.clone().unwrap_or_else(|| m.id.clone());
            if seen.insert(key) {
                picked.push(m);
            }
        }
        picked.sort_by_key(|m| m.date);
        let allow_remote = self.settings().load_remote_images;
        let mut messages = Vec::new();
        for m in picked {
            messages.push(self.message_full(m, allow_remote).await);
        }
        let mut labels_: Vec<String> = metas.iter().map(|m| m.folder.clone()).collect();
        labels_.sort();
        labels_.dedup();
        if metas.iter().any(|m| m.is_starred) {
            labels_.push(labels::STARRED.into());
        }
        let subject = messages.iter().rev().find(|m| !m.meta.is_draft).or(messages.last()).map(|m| m.meta.subject.clone()).unwrap_or_default();
        tracing::debug!(thread = thread_id, messages = messages.len(), "get_thread");
        Ok(ThreadDetail {
            thread_id: thread_id.to_string(),
            account_id: account_id.to_string(),
            subject,
            messages,
            labels: labels_,
            snoozed_until: self.inner.store.snoozed_until(account_id, thread_id)?.filter(|u| *u > now_ms()),
        })
    }

    /// One message with remote images allowed (the "Load images" button).
    pub async fn message_with_images(&self, message_id: &str) -> Result<MessageFull> {
        let meta = self.inner.store.message_meta(message_id)?.context("message not found")?;
        Ok(self.message_full(meta, true).await)
    }

    pub async fn attachment(&self, message_id: &str, index: usize) -> Result<AttachmentData> {
        let raw = self.raw_for(message_id).await?;
        tokio::task::spawn_blocking(move || {
            let parsed = crate::mime::parse(&raw).context("unreadable message")?;
            let meta = parsed.attachments.iter().find(|a| a.index == index).context("attachment not found")?;
            let (_, bytes) = crate::mime::attachments_bytes(&raw, &[index]).pop().context("attachment not found")?;
            Ok(AttachmentData {
                filename: meta.filename.clone(),
                mime_type: meta.mime_type.clone(),
                data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            })
        })
        .await?
    }

    pub fn suggest_contacts(&self, prefix: &str) -> Result<Vec<Contact>> {
        self.inner.store.suggest_contacts(prefix, 8)
    }

    // ------------------------------------------------------------------
    // Mutations
    // ------------------------------------------------------------------

    fn queue(&self, account_id: &str, op: &ServerOp) -> Result<()> {
        self.inner.store.enqueue_op(account_id, &serde_json::to_string(op)?)?;
        let _ = self.send_cmd(account_id, Cmd::FlushOps);
        Ok(())
    }

    fn own_addresses(&self) -> Vec<String> {
        self.accounts().map(|a| a.into_iter().map(|a| a.email).collect()).unwrap_or_default()
    }

    pub fn modify_threads(&self, threads: &[ThreadRef], action: &ThreadAction) -> Result<()> {
        if matches!(action, ThreadAction::MarkRead) {
            self.inner.clean_wake.notify_one();
        }
        // Put back in the Inbox on purpose: auto-clean leaves it there.
        let to_inbox = match action {
            ThreadAction::MoveToInbox { .. } | ThreadAction::NotSpam => true,
            ThreadAction::Move { to, .. } => to == labels::INBOX,
            _ => false,
        };
        if to_inbox {
            let pairs: Vec<(String, String)> = threads.iter().map(|t| (t.account_id.clone(), t.thread_id.clone())).collect();
            self.inner.store.autoclean_exempt(&pairs)?;
        }
        let own = self.own_addresses();
        let mut touched = std::collections::HashSet::new();
        for t in threads {
            let locs = self.inner.store.thread_locations(&t.account_id, &t.thread_id)?;
            if locs.is_empty() {
                continue;
            }
            let account = self.inner.store.account(&t.account_id)?.context("account not found")?;
            let ids = |pred: &dyn Fn(&crate::store::Location) -> bool| -> Vec<String> {
                locs.iter().filter(|l| pred(l)).map(|l| l.id.clone()).collect()
            };
            let mut moves: Vec<(Vec<String>, &str)> = Vec::new();
            match action {
                ThreadAction::Archive => {
                    moves.push((ids(&|l| l.folder == labels::INBOX), labels::ARCHIVE));
                    if self.inner.store.unsnooze(&t.account_id, &t.thread_id)? {
                        self.mark_dirty(sections::SNOOZES);
                    }
                }
                ThreadAction::MoveToInbox { from } => {
                    let from = from.clone();
                    moves.push((
                        ids(&|l| match &from {
                            Some(f) => &l.folder == f,
                            None => !matches!(l.folder.as_str(), labels::INBOX | labels::SENT | labels::DRAFTS),
                        }),
                        labels::INBOX,
                    ));
                }
                ThreadAction::Trash => {
                    moves.push((ids(&|l| l.folder != labels::TRASH), labels::TRASH));
                    if self.inner.store.unsnooze(&t.account_id, &t.thread_id)? {
                        self.mark_dirty(sections::SNOOZES);
                    }
                }
                ThreadAction::Spam => {
                    moves.push((ids(&|l| !matches!(l.folder.as_str(), labels::SPAM | labels::SENT | labels::DRAFTS | labels::TRASH)), labels::SPAM));
                }
                ThreadAction::NotSpam => moves.push((ids(&|l| l.folder == labels::SPAM), labels::INBOX)),
                ThreadAction::Move { from, to } => {
                    let from = from.clone();
                    let to_label = to.as_str();
                    let selected = ids(&|l| match &from {
                        Some(f) if f != labels::STARRED && f != labels::UNREAD && f != labels::SNOOZED && f != labels::ALL => &l.folder == f,
                        _ => !matches!(l.folder.as_str(), labels::SENT | labels::DRAFTS | labels::TRASH) && l.folder != to_label,
                    });
                    moves.push((selected, to_label));
                }
                ThreadAction::MarkRead => {
                    let sel = ids(&|l| l.is_unread);
                    if !sel.is_empty() {
                        self.inner.store.set_flags_local(&sel, Some(false), None)?;
                        self.queue(&t.account_id, &ServerOp::Flags { message_ids: sel, seen: Some(true), flagged: None, answered: None })?;
                    }
                }
                ThreadAction::MarkUnread => {
                    // Received mail only; fall back to the whole thread if every message is ours.
                    let mut sel = ids(&|l| l.folder != labels::SENT && l.folder != labels::DRAFTS);
                    let metas = self.inner.store.thread_messages(&t.account_id, &t.thread_id)?;
                    sel.retain(|id| metas.iter().any(|m| &m.id == id && !own.iter().any(|o| o.eq_ignore_ascii_case(&m.from.email))));
                    if sel.is_empty() {
                        sel = ids(&|_| true);
                    }
                    self.inner.store.set_flags_local(&sel, Some(true), None)?;
                    self.queue(&t.account_id, &ServerOp::Flags { message_ids: sel, seen: Some(false), flagged: None, answered: None })?;
                }
                ThreadAction::Star => {
                    let latest = locs.iter().rev().find(|l| l.folder != labels::TRASH).or(locs.last()).map(|l| l.id.clone());
                    if let Some(id) = latest {
                        self.inner.store.set_flags_local(std::slice::from_ref(&id), None, Some(true))?;
                        self.queue(&t.account_id, &ServerOp::Flags { message_ids: vec![id], seen: None, flagged: Some(true), answered: None })?;
                    }
                }
                ThreadAction::Unstar => {
                    let sel = ids(&|l| l.is_starred);
                    if !sel.is_empty() {
                        self.inner.store.set_flags_local(&sel, None, Some(false))?;
                        self.queue(&t.account_id, &ServerOp::Flags { message_ids: sel, seen: None, flagged: Some(false), answered: None })?;
                    }
                }
                ThreadAction::DeleteForever => {
                    let gone: Vec<&crate::store::Location> =
                        locs.iter().filter(|l| l.folder == labels::TRASH || l.folder == labels::SPAM).collect();
                    let (locations, by_message_id) = expunge_targets(&gone);
                    let ids: Vec<String> = gone.iter().map(|l| l.id.clone()).collect();
                    self.inner.store.delete_messages(&ids)?;
                    if !locations.is_empty() || !by_message_id.is_empty() {
                        self.queue(&t.account_id, &ServerOp::Expunge { locations, by_message_id })?;
                    }
                }
            }
            for (ids, dest) in moves {
                if ids.is_empty() {
                    continue;
                }
                self.inner.store.set_folder_local(&ids, dest)?;
                self.queue(&t.account_id, &ServerOp::Move { message_ids: ids, dest_label: dest.to_string() })?;
            }
            touched.insert(account.id.clone());
        }
        for a in touched {
            self.emit(EngineEvent::MailboxChanged { account_id: a });
        }
        Ok(())
    }

    /// Flags on single messages (reading pane star / mark unread).
    pub fn set_message_flags(&self, message_id: &str, unread: Option<bool>, starred: Option<bool>) -> Result<()> {
        let loc = self.inner.store.location(message_id)?.context("message not found")?;
        self.inner.store.set_flags_local(&[message_id.to_string()], unread, starred)?;
        self.queue(
            &loc.account_id,
            &ServerOp::Flags { message_ids: vec![message_id.to_string()], seen: unread.map(|u| !u), flagged: starred, answered: None },
        )?;
        self.emit(EngineEvent::MailboxChanged { account_id: loc.account_id });
        Ok(())
    }

    pub fn empty_folder(&self, account_id: &str, label: &str) -> Result<()> {
        if label != labels::TRASH && label != labels::SPAM {
            bail!("only Trash and Spam can be emptied");
        }
        if let Some(path) = self.inner.store.folder_path(account_id, label)? {
            self.inner.store.delete_path_messages(account_id, &path)?;
        }
        self.queue(account_id, &ServerOp::EmptyFolder { label: label.to_string() })?;
        self.emit(EngineEvent::MailboxChanged { account_id: account_id.to_string() });
        Ok(())
    }

    pub async fn create_folder(&self, account_id: &str, name: &str) -> Result<()> {
        let name = name.trim().to_string();
        if name.is_empty() {
            bail!("Folder name is empty");
        }
        self.request(account_id, |reply| Cmd::CreateFolder { name, reply }).await?;
        self.emit(EngineEvent::MailboxChanged { account_id: account_id.to_string() });
        Ok(())
    }

    // ------------------------------------------------------------------
    // Snooze
    // ------------------------------------------------------------------

    pub fn snooze_presets(&self) -> Vec<presets::TimePreset> {
        presets::snooze_presets(&self.settings().snooze_times())
    }

    pub fn send_later_presets(&self) -> Vec<presets::TimePreset> {
        presets::send_later_presets()
    }

    pub fn snooze(&self, threads: &[ThreadRef], until: i64) -> Result<()> {
        if until <= now_ms() {
            bail!("Pick a time in the future");
        }
        for t in threads {
            self.inner.store.snooze(&t.account_id, &t.thread_id, until)?;
            self.emit(EngineEvent::MailboxChanged { account_id: t.account_id.clone() });
        }
        self.inner.snooze_wake.notify_one();
        self.mark_dirty(sections::SNOOZES);
        Ok(())
    }

    pub fn unsnooze(&self, threads: &[ThreadRef]) -> Result<()> {
        for t in threads {
            self.inner.store.unsnooze(&t.account_id, &t.thread_id)?;
            self.emit(EngineEvent::MailboxChanged { account_id: t.account_id.clone() });
        }
        self.inner.snooze_wake.notify_one();
        self.mark_dirty(sections::SNOOZES);
        Ok(())
    }

    // ------------------------------------------------------------------
    // Compose
    // ------------------------------------------------------------------

    fn default_account(&self, account_id: Option<&str>) -> Result<Account> {
        let accounts = self.accounts()?;
        if let Some(id) = account_id {
            if let Some(a) = accounts.iter().find(|a| a.id == id) {
                return Ok(a.clone());
            }
        }
        accounts.into_iter().next().context("Add an account first")
    }

    fn blank_draft(&self, account: &Account) -> Result<Draft> {
        let signature = self.inner.store.default_signature(&account.id)?;
        let html_body = match &signature {
            Some(s) => format!("<div><br></div>{}", compose::signature_block(&s.html)),
            None => String::new(),
        };
        Ok(Draft {
            id: new_id("draft"),
            account_id: account.id.clone(),
            to: vec![],
            cc: vec![],
            bcc: account.auto_bcc.iter().filter_map(|a| compose::parse_address(a)).collect(),
            subject: String::new(),
            html_body,
            quoted_html: String::new(),
            attachments: vec![],
            reply_to_message_id: None,
            thread_id: None,
            reply_kind: None,
            signature_id: signature.map(|s| s.id),
            updated_at: now_ms(),
        })
    }

    pub fn new_draft(&self, account_id: Option<&str>) -> Result<Draft> {
        let account = self.default_account(account_id)?;
        self.blank_draft(&account)
    }

    pub fn mailto_draft(&self, url: &str, account_id: Option<&str>) -> Result<Draft> {
        let m = compose::parse_mailto(url).context("not a mailto: link")?;
        let account = self.default_account(account_id)?;
        let mut d = self.blank_draft(&account)?;
        let addrs = |v: &[String]| v.iter().filter_map(|a| compose::parse_address(a)).collect::<Vec<_>>();
        d.to = addrs(&m.to);
        d.cc = addrs(&m.cc);
        d.bcc.extend(addrs(&m.bcc));
        d.subject = m.subject;
        if !m.body.is_empty() {
            d.html_body = format!("<div>{}</div>{}", compose::text_to_html(&m.body), d.html_body);
        }
        Ok(d)
    }

    /// Reply / reply all / forward to a stored message.
    pub async fn reply_draft(&self, message_id: &str, kind: ReplyKind) -> Result<Draft> {
        let meta = self.inner.store.message_meta(message_id)?.context("message not found")?;
        let account = self.default_account(Some(&meta.account_id))?;
        let original = self.message_full(meta, true).await;
        let mut d = self.blank_draft(&account)?;
        let (to, cc) = compose::reply_recipients(&original, kind, &account.email, &self.own_addresses());
        d.to = to;
        d.cc = cc;
        d.subject = compose::prefixed_subject(&original.meta.subject, kind);
        d.reply_kind = Some(kind);
        d.reply_to_message_id = Some(original.meta.id.clone());
        d.thread_id = Some(original.meta.thread_id.clone());
        d.quoted_html = match kind {
            ReplyKind::Forward => compose::forward_block_html(&original),
            _ => compose::reply_quote_html(&original),
        };
        if kind == ReplyKind::Forward {
            // Forward the original's real attachments.
            if let Ok(raw) = self.raw_for(&original.meta.id).await {
                let indexes: Vec<usize> = original.attachments.iter().map(|a| a.index).collect();
                let parts = tokio::task::spawn_blocking(move || crate::mime::attachments_bytes(&raw, &indexes)).await?;
                for (index, bytes) in parts {
                    let Some(a) = original.attachments.iter().find(|a| a.index == index) else { continue };
                    d.attachments.push(OutgoingAttachment {
                        filename: a.filename.clone(),
                        mime_type: a.mime_type.clone(),
                        data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                    });
                }
            }
            d.thread_id = None;
        }
        Ok(d)
    }

    /// Open a synced draft (from the Drafts folder) for editing.
    pub async fn edit_draft_message(&self, message_id: &str) -> Result<Draft> {
        if let Some(d) = self.inner.store.draft_for_server_message(message_id)? {
            return Ok(d);
        }
        let meta = self.inner.store.message_meta(message_id)?.context("draft not found")?;
        let full = self.message_full(meta.clone(), true).await;
        let account = self.default_account(Some(&meta.account_id))?;
        let mut d = self.blank_draft(&account)?;
        d.to = meta.to;
        d.cc = meta.cc;
        d.bcc = meta.bcc;
        d.subject = meta.subject;
        d.html_body = full.html.or_else(|| full.text.map(|t| compose::text_to_html(&t))).unwrap_or_default();
        d.thread_id = Some(meta.thread_id);
        d.signature_id = None;
        self.inner.store.save_draft(&d, Some(message_id), false)?;
        Ok(d)
    }

    pub fn drafts(&self) -> Result<Vec<Draft>> {
        self.inner.store.drafts()
    }

    /// Save locally; with `sync_server`, also store it in the server's
    /// Drafts folder (replacing the previous server copy).
    pub async fn save_draft(&self, draft: Draft, sync_server: bool) -> Result<Draft> {
        if !sync_server {
            let mut draft = draft;
            draft.updated_at = now_ms();
            self.inner.store.save_draft(&draft, None, false)?;
            return Ok(draft);
        }
        let lock = self.inner.draft_saves.lock().unwrap().entry(draft.id.clone()).or_default().clone();
        let id = draft.id.clone();
        let r = {
            let _one_at_a_time = lock.lock().await;
            self.save_draft_to_server(draft).await
        };
        let mut saves = self.inner.draft_saves.lock().unwrap();
        if saves.get(&id).is_some_and(|l| Arc::ptr_eq(l, &lock) && Arc::strong_count(l) == 2) {
            saves.remove(&id);
        }
        r
    }

    async fn save_draft_to_server(&self, mut draft: Draft) -> Result<Draft> {
        draft.updated_at = now_ms();
        let previous = self.inner.store.draft(&draft.id)?.and_then(|(_, s)| s);
        self.inner.store.save_draft(&draft, None, false)?;
        let account = self.default_account(Some(&draft.account_id))?;
        let message_id = format!("{}@remail.local", uuid::Uuid::new_v4().simple());
        let from = EmailAddress::new(account.display_name.clone(), account.email.clone());
        // Drafts may be incomplete; allow no recipients.
        let mut tmp = draft.clone();
        if tmp.to.is_empty() && tmp.cc.is_empty() && tmp.bcc.is_empty() {
            tmp.to = vec![from.clone()];
        }
        let (raw, _) = compose::build_message(&tmp, &compose::OutgoingMeta { from, message_id: message_id.clone(), in_reply_to: None, references: &[] })?;
        let raw = if draft.to.is_empty() && draft.cc.is_empty() && draft.bcc.is_empty() {
            strip_header(&raw, "To")
        } else {
            raw
        };
        let new_row = self
            .request(&account.id, |reply| Cmd::Append { label: labels::DRAFTS.into(), flags: "(\\Draft \\Seen)".into(), raw, message_id, reply })
            .await?;
        if let Some(prev) = previous {
            self.delete_server_copy(&prev)?;
        }
        // Only the server copy changes: the draft may have been edited (or sent,
        // or discarded) while this copy was being stored.
        if !self.inner.store.set_draft_server_copy(&draft.id, new_row.as_deref())? {
            // Sent or discarded meanwhile: don't leave a copy behind.
            if let Some(row) = &new_row {
                self.delete_server_copy(row)?;
            }
            return Ok(draft);
        }
        self.emit(EngineEvent::MailboxChanged { account_id: account.id });
        Ok(draft)
    }

    fn delete_server_copy(&self, message_row_id: &str) -> Result<()> {
        if let Some(loc) = self.inner.store.location(message_row_id)? {
            self.inner.store.delete_messages(std::slice::from_ref(&loc.id))?;
            let (locations, by_message_id) = expunge_targets(&[&loc]);
            if !locations.is_empty() || !by_message_id.is_empty() {
                self.queue(&loc.account_id, &ServerOp::Expunge { locations, by_message_id })?;
            }
        }
        Ok(())
    }

    pub fn delete_draft(&self, id: &str) -> Result<()> {
        if let Some(server) = self.inner.store.delete_draft(id)? {
            self.delete_server_copy(&server)?;
        } else if self.inner.store.location(id)?.is_some() {
            // A synced draft opened directly from the Drafts folder.
            self.delete_server_copy(id)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Sending
    // ------------------------------------------------------------------

    fn validate(&self, draft: &Draft) -> Result<()> {
        let account = self.default_account(Some(&draft.account_id))?;
        compose::build_message(
            draft,
            &compose::OutgoingMeta {
                from: EmailAddress::new(account.display_name, account.email),
                message_id: "check@remail.local".into(),
                in_reply_to: None,
                references: &[],
            },
        )?;
        Ok(())
    }

    /// Queue a message: immediately (after the undo window) or at `send_at`.
    pub fn send(&self, mut draft: Draft, send_at: Option<i64>) -> Result<OutboxItem> {
        self.validate(&draft)?;
        let now = now_ms();
        let (kind, at) = match send_at {
            Some(at) if at < now + presets::MIN_SCHEDULE_LEAD_MS => bail!("Pick a time at least 30 seconds from now"),
            Some(at) => (OutboxKind::Scheduled, at),
            None => (OutboxKind::Undo, now + self.settings().undo_send_seconds as i64 * 1000),
        };
        draft.updated_at = now;
        self.inner.store.save_draft(&draft, None, true)?;
        let item = OutboxItem {
            id: new_id("out"),
            account_id: draft.account_id.clone(),
            draft,
            send_at: at,
            kind,
            status: OutboxStatus::Pending,
            attempts: 0,
            last_error: None,
            created_at: now,
        };
        self.inner.store.enqueue_outbox(&item)?;
        self.inner.outbox_wake.notify_one();
        self.emit(EngineEvent::OutboxChanged);
        Ok(item)
    }

    /// Cancel a queued send. Returns the draft so the UI can reopen it, or
    /// None when it is already being sent.
    pub fn undo_send(&self, outbox_id: &str) -> Result<Option<Draft>> {
        let item = self.inner.store.take_outbox(outbox_id)?;
        if let Some(i) = &item {
            self.inner.store.set_draft_pending(&i.draft.id, false)?;
            self.emit(EngineEvent::OutboxChanged);
        }
        Ok(item.map(|i| i.draft))
    }

    pub fn outbox(&self) -> Result<Vec<OutboxItem>> {
        self.inner.store.outbox()
    }

    pub fn reschedule(&self, outbox_id: &str, send_at: i64) -> Result<()> {
        if send_at < now_ms() + presets::MIN_SCHEDULE_LEAD_MS {
            bail!("Pick a time at least 30 seconds from now");
        }
        if !self.inner.store.reschedule_outbox(outbox_id, send_at)? {
            bail!("This email is already being sent");
        }
        self.inner.outbox_wake.notify_one();
        self.emit(EngineEvent::OutboxChanged);
        Ok(())
    }

    pub fn send_now(&self, outbox_id: &str) -> Result<()> {
        self.inner.store.reschedule_outbox(outbox_id, now_ms())?;
        self.inner.outbox_wake.notify_one();
        self.emit(EngineEvent::OutboxChanged);
        Ok(())
    }

    /// Deliver every pending undo-window send right away (called on quit).
    pub async fn flush_outbox(&self, timeout: Duration) {
        let items: Vec<OutboxItem> = self
            .outbox()
            .unwrap_or_default()
            .into_iter()
            .filter(|i| i.kind == OutboxKind::Undo && i.status == OutboxStatus::Pending)
            .collect();
        let inner = self.inner.clone();
        let _ = tokio::time::timeout(timeout, async move {
            for item in items {
                deliver(&inner, item).await;
            }
        })
        .await;
    }

    // ------------------------------------------------------------------
    // Signatures, templates
    // ------------------------------------------------------------------

    pub fn signatures(&self) -> Result<Vec<Signature>> {
        self.inner.store.signatures()
    }

    pub fn save_signature(&self, mut s: Signature) -> Result<Signature> {
        if s.name.trim().is_empty() {
            bail!("Give the signature a name");
        }
        if s.name.chars().count() > 100 || s.html.len() > 100_000 {
            bail!("Signature is too long");
        }
        if s.id.is_empty() {
            s.id = new_id("sig");
        }
        self.inner.store.save_signature(&s)?;
        self.mark_dirty(sections::SIGNATURES);
        Ok(s)
    }

    pub fn delete_signature(&self, id: &str) -> Result<()> {
        self.inner.store.delete_signature(id)?;
        self.mark_dirty(sections::SIGNATURES);
        Ok(())
    }

    pub fn templates(&self) -> Result<Vec<Template>> {
        self.inner.store.templates()
    }

    pub fn save_template(&self, mut t: Template) -> Result<Template> {
        if t.name.trim().is_empty() {
            bail!("Give the template a name");
        }
        if t.id.is_empty() {
            t.id = new_id("tmpl");
        }
        t.updated_at = now_ms();
        self.inner.store.save_template(&t)?;
        self.mark_dirty(sections::TEMPLATES);
        Ok(t)
    }

    pub fn delete_template(&self, id: &str) -> Result<()> {
        self.inner.store.delete_template(id)?;
        self.mark_dirty(sections::TEMPLATES);
        Ok(())
    }

    // ------------------------------------------------------------------
    // Auto-clean
    // ------------------------------------------------------------------

    /// The UI reports which conversation is open; leaving one lets auto-clean act on it.
    pub fn set_viewing(&self, thread: Option<ThreadRef>) {
        let mut v = self.inner.viewing.lock().unwrap();
        let changed = match (&*v, &thread) {
            (Some(a), Some(b)) => a.account_id != b.account_id || a.thread_id != b.thread_id,
            (None, None) => false,
            _ => true,
        };
        *v = thread;
        drop(v);
        if changed {
            self.inner.clean_wake.notify_one();
        }
    }

    /// Conversations auto-clean would remove now (ignoring the open one).
    pub fn autoclean_preview(&self) -> Result<usize> {
        Ok(autoclean_due(&self.inner, &self.settings().auto_clean, now_ms(), None)?.len())
    }

    /// Bring auto-cleaned messages back to the Inbox (exactly the ones it
    /// moved) and keep their conversations out of auto-clean.
    pub fn autoclean_undo(&self, threads: &[ThreadRef], messages: &[String]) -> Result<()> {
        let pairs: Vec<(String, String)> = threads.iter().map(|t| (t.account_id.clone(), t.thread_id.clone())).collect();
        self.inner.store.autoclean_exempt(&pairs)?;
        self.move_messages(messages, labels::INBOX)
    }

    /// Move specific messages (by row id) to a folder, locally now and on the server.
    fn move_messages(&self, ids: &[String], dest: &str) -> Result<()> {
        let mut by_account: HashMap<String, Vec<String>> = HashMap::new();
        for id in ids {
            if let Some(loc) = self.inner.store.location(id)? {
                if loc.folder != dest {
                    by_account.entry(loc.account_id).or_default().push(loc.id);
                }
            }
        }
        for (account, ids) in by_account {
            self.inner.store.set_folder_local(&ids, dest)?;
            self.queue(&account, &ServerOp::Move { message_ids: ids, dest_label: dest.to_string() })?;
            self.emit(EngineEvent::MailboxChanged { account_id: account });
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Smart Inbox
    // ------------------------------------------------------------------

    pub fn smart_counts(&self, account_id: Option<&str>) -> Result<crate::store::SmartCounts> {
        self.inner.store.smart_unread_counts(account_id, now_ms())
    }

    /// Always put a sender's mail in Important (`primary`) or a bucket of
    /// Other; `None` forgets the rule.
    pub fn set_sender_category(&self, email: &str, category: Option<&str>) -> Result<()> {
        if email.trim().is_empty() {
            bail!("no sender address");
        }
        let category = match category {
            Some(c) => Some(crate::classify::Category::parse(c).context("unknown category")?.as_str()),
            None => None,
        };
        self.inner.store.set_sender_rule(email, category)?;
        self.mark_dirty(sections::SENDER_RULES);
        for a in self.accounts()? {
            self.emit(EngineEvent::MailboxChanged { account_id: a.id });
        }
        Ok(())
    }

    pub fn sender_rules(&self) -> Result<Vec<crate::store::SenderRule>> {
        self.inner.store.sender_rules()
    }

    /// Ask the UI to open a draft in a compose window.
    pub fn request_open_compose(&self, draft_id: &str) {
        self.emit(EngineEvent::OpenCompose { draft_id: draft_id.to_string() });
    }

    // ------------------------------------------------------------------
    // AI assistant
    // ------------------------------------------------------------------

    pub fn ai_settings(&self) -> AiSettings {
        self.inner.store.setting("ai").unwrap_or_default()
    }

    pub fn ai_has_key(&self) -> bool {
        self.inner.secrets.get(ai::KEY_SECRET_ID).ok().flatten().is_some_and(|k| !k.is_empty())
    }

    /// Save AI settings; `api_key`: None keeps the stored key, `Some("")` removes it.
    pub fn set_ai_settings(&self, s: &AiSettings, api_key: Option<&str>) -> Result<()> {
        let before = self.ai_settings();
        self.inner.store.set_setting("ai", s)?;
        if before.smart_sort && !(s.enabled && s.smart_sort) {
            self.inner.store.clear_ai_categories()?;
            for a in self.accounts()? {
                self.emit(EngineEvent::MailboxChanged { account_id: a.id });
            }
        }
        match api_key.map(str::trim) {
            Some("") => self.inner.secrets.delete(ai::KEY_SECRET_ID)?,
            Some(k) => self.inner.secrets.set(ai::KEY_SECRET_ID, k)?,
            None => {}
        }
        self.inner.ai.summaries.lock().unwrap().clear();
        self.inner.ai.sort_wake.notify_one();
        if (before.enabled, &before.instructions, before.smart_sort) != (s.enabled, &s.instructions, s.smart_sort) {
            self.mark_dirty(sections::PREFS);
        }
        Ok(())
    }

    fn ai_client_with(&self, settings: AiSettings, api_key: Option<String>) -> Result<ai::Client> {
        let key = match api_key.filter(|k| !k.trim().is_empty()) {
            Some(k) => Some(k),
            None => self.inner.secrets.get(ai::KEY_SECRET_ID)?,
        };
        ai::Client::new(self.inner.ai.http.get_or_init(ai::http_client).clone(), settings, key)
    }

    fn ai_client(&self) -> Result<ai::Client> {
        let s = self.ai_settings();
        if !s.enabled {
            bail!("Turn on the AI assistant in Settings → AI Assistant");
        }
        self.ai_client_with(s, None)
    }

    /// Models offered by a provider (settings may be unsaved).
    pub async fn ai_models(&self, settings: AiSettings, api_key: Option<String>) -> Result<Vec<String>> {
        let mut s = settings;
        if s.model.is_empty() {
            s.model = "-".into(); // the client insists on a model; listing doesn't use it
        }
        self.ai_client_with(s, api_key)?.list_models().await
    }

    /// Send a tiny request to check the settings work.
    pub async fn ai_test(&self, settings: AiSettings, api_key: Option<String>) -> Result<String> {
        let c = self.ai_client_with(settings, api_key)?;
        let reply = c.complete("You are a connection test.", "Reply with the single word: OK", None).await?;
        Ok(reply.trim().chars().take(80).collect())
    }

    fn owner(&self, account_id: Option<&str>) -> EmailAddress {
        self.default_account(account_id).map(|a| EmailAddress::new(a.display_name, a.email)).unwrap_or_default()
    }

    fn start_run(&self, request_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.inner.ai.runs.lock().unwrap().insert(request_id.to_string(), token.clone());
        token
    }

    fn end_run(&self, request_id: &str) {
        self.inner.ai.runs.lock().unwrap().remove(request_id);
    }

    /// Stop a running AI request (`ai_run` / `ai_chat`).
    pub fn ai_cancel(&self, request_id: &str) {
        if let Some(t) = self.inner.ai.runs.lock().unwrap().remove(request_id) {
            t.cancel();
        }
    }

    /// One-shot writing task, streaming text through `on_text`.
    pub async fn ai_run(&self, request_id: &str, task: AiTask, on_text: &mut (dyn FnMut(&str) + Send)) -> Result<AiOutput> {
        let token = self.start_run(request_id);
        let r = tokio::select! {
            r = self.ai_run_inner(task, on_text) => r,
            _ = token.cancelled() => Err(anyhow!("Stopped")),
        };
        self.end_run(request_id);
        r
    }

    async fn ai_run_inner(&self, task: AiTask, on_text: &mut (dyn FnMut(&str) + Send)) -> Result<AiOutput> {
        let client = self.ai_client()?;
        let instructions = client.settings().instructions.clone();
        match task {
            AiTask::Summarize { account_id, thread_id } => {
                let t = self.get_thread(&account_id, &thread_id).await?;
                let latest = t.messages.last().map(|m| m.meta.id.clone()).unwrap_or_default();
                let key = format!("{account_id}|{thread_id}|{latest}");
                if let Some(cached) = self.inner.ai.summaries.lock().unwrap().get(&key).cloned() {
                    on_text(&cached);
                    return Ok(AiOutput { text: cached, ..Default::default() });
                }
                let system = format!("{}\n\n{}", ai::base_system(&self.owner(Some(&account_id)), &instructions), ai::summarize_prompt());
                let turn = client.stream(&system, &[ai::Message::user(ai::thread_text(&t))], &[], None, on_text).await?;
                let mut text = usable_text(&turn, true)?;
                if turn.stop_reason == "max_tokens" {
                    text.push('…');
                } else {
                    self.inner.ai.summaries.lock().unwrap().insert(key, text.clone());
                }
                Ok(AiOutput { text, ..Default::default() })
            }
            AiTask::Reply { message_id, kind, instructions: wish } => {
                let meta = self.inner.store.message_meta(&message_id)?.context("message not found")?;
                let t = self.get_thread(&meta.account_id, &meta.thread_id).await?;
                let mut draft = self.reply_draft(&message_id, kind).await?;
                let what = match kind {
                    ReplyKind::Reply => "a reply to the latest message",
                    ReplyKind::ReplyAll => "a reply to everyone on the latest message",
                    ReplyKind::Forward => "a short note to go with forwarding this conversation",
                };
                let to = draft.to.iter().map(|a| a.display_full()).collect::<Vec<_>>().join(", ");
                let user = format!(
                    "Conversation:\n\n{}\n\n---\nWrite {what}{}.\nWhat the user wants to say: {}",
                    ai::thread_text(&t),
                    if to.is_empty() { String::new() } else { format!(" (to {to})") },
                    if wish.trim().is_empty() { "Write the most suitable reply." } else { wish.trim() }
                );
                let system = format!("{}\n\n{}", ai::base_system(&self.owner(Some(&meta.account_id)), &instructions), ai::reply_prompt());
                let turn = client.stream(&system, &[ai::Message::user(user)], &[], None, on_text).await?;
                let text = ai::strip_fences(&usable_text(&turn, false)?);
                let html = ai::text_to_email_html(&text);
                draft.html_body = assistant::with_body(&draft.html_body, &html);
                Ok(AiOutput { text, html: Some(html), subject: None, draft: Some(draft) })
            }
            AiTask::Compose { account_id, instructions: wish, to, subject, current } => {
                let mut user = String::new();
                if !to.is_empty() {
                    user.push_str(&format!("Recipients: {}\n", to.iter().map(|a| a.display_full()).collect::<Vec<_>>().join(", ")));
                }
                if !subject.trim().is_empty() {
                    user.push_str(&format!("Current subject: {}\n", subject.trim()));
                }
                if !current.trim().is_empty() {
                    user.push_str(&format!("Current draft (revise or continue it as asked):\n{}\n", current.trim()));
                }
                user.push_str(&format!("\nWhat to write: {}", wish.trim()));
                let system = format!("{}\n\n{}", ai::base_system(&self.owner(account_id.as_deref()), &instructions), ai::compose_prompt());
                let turn = client.stream(&system, &[ai::Message::user(user)], &[], None, on_text).await?;
                let (subj, body) = ai::split_subject(&usable_text(&turn, false)?);
                Ok(AiOutput { html: Some(ai::text_to_email_html(&body)), text: body, subject: subj, draft: None })
            }
            AiTask::Rewrite { account_id, text, mode, instructions: custom } => {
                if text.trim().is_empty() {
                    bail!("There's no text to rewrite yet");
                }
                let system = format!(
                    "{}\n\n{}",
                    ai::base_system(&self.owner(account_id.as_deref()), &instructions),
                    ai::rewrite_instruction(&mode, &custom)
                );
                let turn = client.stream(&system, &[ai::Message::user(text)], &[], None, on_text).await?;
                let out = ai::strip_fences(&usable_text(&turn, false)?);
                Ok(AiOutput { html: Some(ai::text_to_email_html(&out)), text: out, subject: None, draft: None })
            }
        }
    }

    /// Answer a tool approval request from the chat panel.
    pub fn ai_approve(&self, call_id: &str, allow: bool) {
        if let Some(tx) = self.inner.ai.approvals.lock().unwrap().remove(call_id) {
            let _ = tx.send(allow);
        }
    }

    pub fn ai_chat_reset(&self, conversation_id: &str) {
        self.inner.ai.chats.lock().unwrap().remove(conversation_id);
        self.inner.ai.untrusted.lock().unwrap().remove(conversation_id);
    }

    /// One user message to the assistant: runs the model/tool loop and
    /// streams progress through `emit`.
    pub async fn ai_chat(
        &self,
        conversation_id: &str,
        request_id: &str,
        text: String,
        context: Option<ThreadRef>,
        emit: Arc<dyn Fn(ChatEvent) + Send + Sync>,
    ) -> Result<()> {
        let token = self.start_run(request_id);
        let r = tokio::select! {
            r = self.ai_chat_inner(conversation_id, text, context, emit.clone()) => r,
            _ = token.cancelled() => Err(anyhow!("Stopped")),
        };
        self.end_run(request_id);
        match &r {
            Ok(()) => emit(ChatEvent::Done),
            Err(e) => emit(ChatEvent::Error { message: format!("{e:#}") }),
        }
        r
    }

    async fn ai_chat_inner(
        &self,
        conversation_id: &str,
        text: String,
        context: Option<ThreadRef>,
        emit: Arc<dyn Fn(ChatEvent) + Send + Sync>,
    ) -> Result<()> {
        let client = self.ai_client()?;
        let accounts = self.accounts()?;
        let current = match &context {
            Some(c) => self.get_thread(&c.account_id, &c.thread_id).await.ok(),
            None => None,
        };
        let owner = self.owner(context.as_ref().map(|c| c.account_id.as_str()));
        let system = assistant::chat_system(&owner, &accounts, &client.settings().instructions, current.as_ref());
        let mut system = system;
        let mut tools = assistant::tool_defs(assistant::Permissions::ALL);
        let opts = assistant::CallOptions { permissions: assistant::Permissions::ALL, open_drafts: true };
        let external = self.external_tools(&mut tools, &mut system).await;
        // Email anyone can write is in the context (the open thread, or tool results earlier in
        // this chat): it may tell the model to send mail out through an outside tool.
        if current.is_some() {
            self.inner.ai.untrusted.lock().unwrap().insert(conversation_id.to_string());
        }
        let mut untrusted = self.inner.ai.untrusted.lock().unwrap().contains(conversation_id);

        let mut msgs = self.inner.ai.chats.lock().unwrap().get(conversation_id).cloned().unwrap_or_default();
        msgs.push(ai::Message::user(text));
        for _ in 0..12 {
            let em = emit.clone();
            let mut on_text = move |t: &str| em(ChatEvent::Text { delta: t.to_string() });
            let turn = client.stream(&system, &msgs, &tools, None, &mut on_text).await?;
            let uses = turn.tool_uses();
            if turn.content.is_empty() {
                // Providers reject empty assistant turns in later requests.
                let note = "(The model returned no answer.)";
                emit(ChatEvent::Text { delta: note.into() });
                msgs.push(ai::Message { role: ai::Role::Assistant, content: vec![ai::Block::Text { text: note.into() }] });
                break;
            }
            msgs.push(ai::Message { role: ai::Role::Assistant, content: turn.content.clone() });
            if uses.is_empty() {
                break;
            }
            let mut results = Vec::new();
            for (id, name, input) in uses {
                let title = match external.get(&name) {
                    Some((_, tool)) => tool.replace('_', " "),
                    None => assistant::tool_title(&name, &input),
                };
                let ext = external.get(&name);
                let server = ext.map(|(cfg, _)| cfg.name.clone());
                emit(ChatEvent::ToolStart { id: id.clone(), name: name.clone(), title: title.clone(), input: input.clone(), server: server.clone() });
                let bad_args = turn.has_bad_args(&id);
                // Auto-approve doesn't cover outside tools once email content could be steering the model.
                let (ask, reason) = match ext {
                    Some((cfg, _)) if cfg.auto_approve && untrusted => (true, Some(UNTRUSTED_APPROVAL.to_string())),
                    Some((cfg, _)) => (!cfg.auto_approve, None),
                    None => (assistant::needs_approval(&name), None),
                };
                let allowed = if bad_args {
                    false
                } else if ask {
                    let (tx, rx) = oneshot::channel();
                    self.inner.ai.approvals.lock().unwrap().insert(id.clone(), tx);
                    // Forget the request if the chat is stopped while waiting.
                    let _pending = ApprovalGuard { map: &self.inner.ai.approvals, id: id.clone() };
                    emit(ChatEvent::Approval { id: id.clone(), name: name.clone(), title, input: input.clone(), server, reason });
                    rx.await.unwrap_or(false)
                } else {
                    true
                };
                if allowed && ext.is_none() && assistant::exposes_mail(&name) && !untrusted {
                    untrusted = true;
                    self.inner.ai.untrusted.lock().unwrap().insert(conversation_id.to_string());
                }
                let out = if bad_args {
                    assistant::ToolResult {
                        text: "The arguments of this call were cut off or weren't valid JSON, so it wasn't run.".into(),
                        is_error: true,
                    }
                } else if !allowed {
                    assistant::ToolResult { text: "The user declined this action.".into(), is_error: true }
                } else if let Some((cfg, tool)) = ext {
                    match self.inner.mcp_hub.call(cfg, tool, input.clone()).await {
                        Ok(o) => assistant::ToolResult { text: o.text, is_error: o.is_error },
                        Err(e) => assistant::ToolResult { text: format!("{e:#}"), is_error: true },
                    }
                } else {
                    assistant::call(self, &name, &input, opts).await
                };
                emit(ChatEvent::ToolEnd { id: id.clone(), ok: !out.is_error, summary: assistant::result_summary(&out) });
                results.push(ai::Block::ToolResult { tool_use_id: id, content: assistant::clip_tool_output(&out.text), is_error: out.is_error });
            }
            msgs.push(ai::Message { role: ai::Role::User, content: results });
            // Keep completed actions in the history even if a later step fails.
            self.inner.ai.chats.lock().unwrap().insert(conversation_id.to_string(), msgs.clone());
        }
        assistant::trim_history(&mut msgs, 120_000);
        self.inner.ai.chats.lock().unwrap().insert(conversation_id.to_string(), msgs);
        Ok(())
    }

    /// Tools of the enabled outside MCP servers, added to `tools`; returns
    /// model-facing name → (server, original tool name).
    async fn external_tools(&self, tools: &mut Vec<ai::ToolDef>, system: &mut String) -> HashMap<String, (McpServerConfig, String)> {
        let enabled: Vec<McpServerConfig> = self.mcp_settings().servers.into_iter().filter(|s| s.enabled).collect();
        let enabled = self.resolve_mcp_secrets(enabled);
        let mut map = HashMap::new();
        if enabled.is_empty() {
            return map;
        }
        let mut problems = Vec::new();
        for (cfg, res) in self.inner.mcp_hub.list_all(&enabled).await {
            match res {
                Ok(list) => {
                    for t in list {
                        let mut name = mcpmod::sanitize_tool_name(&cfg.name, &t.name);
                        // Two servers can sanitize to the same name; providers reject duplicates.
                        let mut n = 2;
                        while map.contains_key(&name) || tools.iter().any(|d| d.name == name) {
                            let base: String = mcpmod::sanitize_tool_name(&cfg.name, &t.name).chars().take(60).collect();
                            name = format!("{base}_{n}");
                            n += 1;
                        }
                        let mut schema = t.input_schema.clone();
                        if !schema.is_object() {
                            schema = serde_json::json!({ "type": "object", "properties": {} });
                        } else if schema.get("type").is_none() {
                            schema["type"] = serde_json::json!("object");
                        }
                        tools.push(ai::ToolDef {
                            name: name.clone(),
                            description: format!("[{} via MCP] {}", cfg.name, t.description).chars().take(1000).collect(),
                            input_schema: schema,
                        });
                        map.insert(name, (cfg.clone(), t.name));
                    }
                }
                Err(e) => {
                    // Errors can carry URLs with secret paths and server output; the model only gets the name.
                    let detail = mcpmod::redact_urls(&crate::mime::truncate_chars(&format!("{e:#}"), 500));
                    tracing::warn!(server = %cfg.name, "MCP server unavailable: {detail}");
                    problems.push(cfg.name.clone());
                }
            }
        }
        if !problems.is_empty() {
            system.push_str(&format!(
                "\n\nThese connected MCP servers could not be reached or failed to start, so their tools are unavailable \
                 right now; tell the user if they ask for them (details are in Settings → Connections): {}",
                problems.join("; ")
            ));
        }
        map
    }

    // ------------------------------------------------------------------
    // MCP
    // ------------------------------------------------------------------

    /// The settings, with outside servers' env var and header values replaced by
    /// [`SECRET_PLACEHOLDER`](mcpmod::client::SECRET_PLACEHOLDER) (the values live in the secret store).
    pub fn mcp_settings(&self) -> McpSettings {
        let mut s: McpSettings = self.inner.store.setting("mcp").unwrap_or_default();
        // Up to v0.1.4 the values were saved in the database itself.
        if self.stash_mcp_secrets(&mut s, false).unwrap_or(false) {
            if let Err(e) = self.inner.store.set_setting("mcp", &s) {
                tracing::warn!("couldn't save MCP settings: {e:#}");
            }
        }
        s
    }

    pub async fn set_mcp_settings(&self, s: McpSettings) -> Result<McpStatus> {
        let mut s = s;
        let before = self.mcp_settings();
        for srv in &mut s.servers {
            if srv.id.trim().is_empty() {
                srv.id = new_id("mcp");
            }
            srv.name = srv.name.trim().to_string();
            if srv.name.is_empty() {
                bail!("Give every MCP server a name");
            }
            let (kind, values) = srv.transport.secret_values();
            for (key, value) in values {
                let id = mcpmod::client::secret_id(&srv.id, kind, key);
                if value == mcpmod::client::SECRET_PLACEHOLDER && self.inner.secrets.get(&id)?.is_none() {
                    bail!("Enter the value of {key} for {} again", srv.name);
                }
            }
        }
        self.stash_mcp_secrets(&mut s, true)?;
        self.inner.store.set_setting("mcp", &s)?;
        let kept = mcp_secret_ids(&s);
        for id in mcp_secret_ids(&before).difference(&kept) {
            if let Err(e) = self.inner.secrets.delete(id) {
                tracing::warn!("couldn't delete an MCP server credential: {e:#}");
            }
        }
        self.inner.mcp_hub.retain(&self.resolve_mcp_secrets(s.servers)).await;
        self.apply_mcp_server().await;
        Ok(self.mcp_status())
    }

    /// Moves plaintext env var / header values into the secret store, leaving placeholders.
    /// Returns whether anything moved; with `strict`, the first failure is an error.
    fn stash_mcp_secrets(&self, s: &mut McpSettings, strict: bool) -> Result<bool> {
        let mut moved = false;
        for srv in s.servers.iter_mut().filter(|srv| !srv.id.is_empty()) {
            let (kind, values) = srv.transport.secret_values_mut();
            for (key, value) in values.iter_mut() {
                if value.is_empty() || value == mcpmod::client::SECRET_PLACEHOLDER {
                    continue;
                }
                match self.inner.secrets.set(&mcpmod::client::secret_id(&srv.id, kind, key), value) {
                    Ok(()) => {
                        *value = mcpmod::client::SECRET_PLACEHOLDER.to_string();
                        moved = true;
                    }
                    Err(e) if strict => return Err(e.context("couldn't store the MCP server's credentials")),
                    Err(e) => tracing::warn!("couldn't move an MCP server credential to the secret store: {e:#}"),
                }
            }
        }
        Ok(moved)
    }

    /// The servers with placeholders replaced by the stored values, ready to connect.
    fn resolve_mcp_secrets(&self, servers: Vec<McpServerConfig>) -> Vec<McpServerConfig> {
        servers
            .into_iter()
            .map(|mut srv| {
                let (kind, values) = srv.transport.secret_values_mut();
                for (key, value) in values.iter_mut().filter(|(_, v)| *v == mcpmod::client::SECRET_PLACEHOLDER) {
                    let id = mcpmod::client::secret_id(&srv.id, kind, key);
                    *value = match self.inner.secrets.get(&id) {
                        Ok(v) => v.unwrap_or_default(),
                        // The server then fails to sign in; say why here (never the value).
                        Err(e) => {
                            tracing::warn!("reading the saved value of {key} for MCP server {} failed: {e:#}", srv.name);
                            String::new()
                        }
                    };
                }
                srv
            })
            .collect()
    }

    fn mcp_token(&self) -> Result<String> {
        if let Some(t) = self.inner.secrets.get(MCP_TOKEN_ID)?.filter(|t| t.len() >= 16) {
            return Ok(t);
        }
        let t = format!("rm_{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        self.inner.secrets.set(MCP_TOKEN_ID, &t)?;
        Ok(t)
    }

    /// New access token; agents must be reconfigured.
    pub async fn regenerate_mcp_token(&self) -> Result<McpStatus> {
        self.inner.secrets.delete(MCP_TOKEN_ID)?;
        self.mcp_token()?;
        self.stop_mcp_server().await;
        self.apply_mcp_server().await;
        Ok(self.mcp_status())
    }

    pub fn mcp_status(&self) -> McpStatus {
        let port = self.inner.mcp_server.lock().unwrap().as_ref().map(|h| h.port());
        McpStatus {
            running: port.is_some(),
            url: port.map(|p| format!("http://127.0.0.1:{p}/mcp")),
            token: self.mcp_token().unwrap_or_default(),
            error: self.inner.mcp_error.lock().unwrap().clone(),
        }
    }

    async fn stop_mcp_server(&self) {
        let handle = self.inner.mcp_server.lock().unwrap().take();
        if let Some(h) = handle {
            h.shutdown().await;
        }
        if let Some(dir) = &self.inner.data_dir {
            mcpmod::bridge::remove_endpoint_file(dir);
        }
    }

    /// Start, restart or stop Remail's MCP server to match the settings.
    pub async fn apply_mcp_server(&self) {
        let s = self.mcp_settings();
        let running_port = self.inner.mcp_server.lock().unwrap().as_ref().map(|h| h.port());
        if !s.server_enabled {
            self.stop_mcp_server().await;
            *self.inner.mcp_error.lock().unwrap() = None;
            return;
        }
        if running_port == Some(s.port) {
            return;
        }
        self.stop_mcp_server().await;
        let result = async {
            let token = self.mcp_token()?;
            let handler: Arc<dyn ToolHandler> = Arc::new(RemailTools { inner: Arc::downgrade(&self.inner) });
            let handle = crate::mcp::server::serve(s.port, token.clone(), "remail", env!("CARGO_PKG_VERSION"), handler)
                .await
                .with_context(|| format!("couldn't listen on port {}", s.port))?;
            if let Some(dir) = &self.inner.data_dir {
                let ep = mcpmod::bridge::EndpointFile {
                    url: format!("http://127.0.0.1:{}/mcp", handle.port()),
                    token,
                    pid: std::process::id(),
                };
                if let Err(e) = mcpmod::bridge::write_endpoint_file(dir, &ep) {
                    tracing::warn!("couldn't write the MCP endpoint file: {e:#}");
                }
            }
            Ok::<_, anyhow::Error>(handle)
        }
        .await;
        match result {
            Ok(h) => {
                *self.inner.mcp_server.lock().unwrap() = Some(h);
                *self.inner.mcp_error.lock().unwrap() = None;
            }
            Err(e) => {
                tracing::warn!("MCP server failed to start: {e:#}");
                *self.inner.mcp_error.lock().unwrap() = Some(format!("{e:#}"));
            }
        }
    }

    /// Stop the MCP server and outside servers (on quit). Waits up to ~3 s for the outside
    /// servers' processes to end: the app exits without running destructors.
    pub fn shutdown_mcp(&self) {
        if let Some(h) = self.inner.mcp_server.lock().unwrap().take() {
            h.stop();
        }
        if let Some(dir) = &self.inner.data_dir {
            mcpmod::bridge::remove_endpoint_file(dir);
        }
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(3), inner.mcp_hub.shutdown()).await;
            let _ = done_tx.send(());
        });
        // Blocking a single-threaded runtime's only thread would stall the shutdown itself.
        let single_thread = Handle::try_current().is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread);
        if !single_thread {
            let _ = done_rx.recv_timeout(Duration::from_millis(3500));
        }
    }

    /// Connect to an outside server once and list its tools (settings "Test"). Hidden values
    /// ([`SECRET_PLACEHOLDER`](mcpmod::client::SECRET_PLACEHOLDER)) are filled in from the saved
    /// server with the same command or URL.
    pub async fn mcp_test(&self, transport: &McpTransport) -> Result<(ServerInfo, Vec<ToolInfo>)> {
        let mut transport = transport.clone();
        let hidden = transport.secret_values().1.values().any(|v| v == mcpmod::client::SECRET_PLACEHOLDER);
        if hidden {
            let saved = self.mcp_settings().servers.into_iter().find(|s| s.transport.same_endpoint(&transport));
            let (kind, values) = transport.secret_values_mut();
            for (key, value) in values.iter_mut().filter(|(_, v)| *v == mcpmod::client::SECRET_PLACEHOLDER) {
                let stored = match &saved {
                    Some(srv) => self.inner.secrets.get(&mcpmod::client::secret_id(&srv.id, kind, key))?,
                    None => None,
                };
                *value = stored.with_context(|| format!("Enter the value of {key} again to test with it"))?;
            }
        }
        McpHub::test(&transport).await
    }

    /// MCP servers configured in other apps (Claude, Cursor, VS Code, …).
    pub async fn mcp_discover(&self) -> Vec<mcpmod::import::ImportedServer> {
        tokio::task::spawn_blocking(mcpmod::import::discover).await.unwrap_or_default()
    }

    /// Servers from pasted JSON config.
    pub fn mcp_parse(&self, text: &str) -> Result<Vec<mcpmod::import::ImportedServer>> {
        mcpmod::import::parse_config_json(text, "Pasted")
    }

    // ------------------------------------------------------------------
    // Signing out of Remail
    // ------------------------------------------------------------------

    /// Stop mail and remove everything the Remail account brought to this
    /// device: mailboxes (with their mail), credentials, calendars,
    /// signatures, templates, sender rules, snoozes and the section cache.
    /// Device settings (theme, notifications, MCP) stay.
    pub fn wipe_setup(&self) -> Result<()> {
        self.close_gate();
        self.cancel_sign_in();
        let accounts = self.inner.store.accounts()?;
        let cal_accounts: Vec<String> = {
            let db = self.inner.store.db();
            let mut stmt = db.prepare("SELECT id FROM cal_accounts")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for a in &accounts {
            if let Err(e) = self.inner.secrets.delete(&a.id) {
                tracing::warn!("couldn't delete an account's credential: {e:#}");
            }
        }
        for c in &cal_accounts {
            let _ = self.inner.secrets.delete(&format!("cal:{c}"));
        }
        {
            let mut db = self.inner.store.db();
            let tx = db.transaction()?;
            tx.execute_batch(
                "DELETE FROM cal_accounts; DELETE FROM signatures; DELETE FROM templates; DELETE FROM sender_rules;
                 DELETE FROM snoozes; DELETE FROM accounts; DELETE FROM cloud_sections;",
            )?;
            for key in [crate::calendar::OUTLOOK_OFF, UI_PREFS, crate::cloud::CAL_OVERRIDES] {
                tx.execute("DELETE FROM settings WHERE key = ?", [key])?;
            }
            tx.commit()?;
        }
        self.inner.oauth_tokens.lock().unwrap().clear();
        self.inner.ai.summaries.lock().unwrap().clear();
        self.inner.ai.chats.lock().unwrap().clear();
        self.inner.cal.forget_all();
        for a in accounts {
            self.emit(EngineEvent::MailboxChanged { account_id: a.id });
        }
        self.emit(EngineEvent::CalendarChanged);
        Ok(())
    }

    /// Stop Remail's MCP server (signed out: agents mustn't reach mail).
    pub async fn stop_mcp(&self) {
        self.stop_mcp_server().await;
    }

    /// Re-apply what settings changed on another device and tell open windows.
    pub(crate) fn settings_applied(&self) {
        self.inner.clean_wake.notify_one();
        self.inner.ai.sort_wake.notify_one();
        self.inner.snooze_wake.notify_one();
        self.emit(EngineEvent::SettingsChanged);
    }

    /// A snooze arrived from another device.
    pub(crate) fn snoozes_applied(&self, account_ids: &[String]) {
        self.inner.snooze_wake.notify_one();
        for a in account_ids {
            self.emit(EngineEvent::MailboxChanged { account_id: a.clone() });
        }
    }

    /// Restart an account's worker (its servers or sign-in changed elsewhere).
    pub(crate) fn restart_worker(&self, account_id: &str) {
        self.inner.workers.lock().unwrap().remove(account_id);
        let mut cache = self.inner.oauth_tokens.lock().unwrap();
        cache.remove(account_id);
        cache.remove(&token_key(account_id, Service::Calendar));
        drop(cache);
        self.spawn_worker(account_id);
    }

    /// Start the worker of an account that arrived from another device.
    pub(crate) fn start_worker(&self, account_id: &str) {
        if !self.inner.workers.lock().unwrap().contains_key(account_id) {
            self.spawn_worker(account_id);
        }
    }

    // ------------------------------------------------------------------
    // Unsubscribe
    // ------------------------------------------------------------------

    pub fn newsletter_senders(&self, account_id: Option<&str>) -> Result<Vec<NewsletterSender>> {
        self.inner.store.newsletter_senders(account_id, 50)
    }

    /// RFC 8058 one-click POST when offered; otherwise hand back the web
    /// page to open; otherwise send the `mailto:` request from the account.
    pub async fn unsubscribe(&self, message_id: &str) -> Result<UnsubscribeResult> {
        let meta = self.inner.store.message_meta(message_id)?.context("message not found")?;
        let Some(u) = meta.unsubscribe.clone() else {
            return Ok(UnsubscribeResult { method: "none".into(), url: None, error: Some("No unsubscribe method available".into()) });
        };
        if let (true, Some(url)) = (u.one_click, &u.http_url) {
            if url.to_ascii_lowercase().starts_with("https://") {
                let res = self
                    .inner
                    .http
                    .post(url)
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body("List-Unsubscribe=One-Click")
                    .send()
                    .await;
                match res {
                    Ok(r) if r.status().is_success() => {
                        return Ok(UnsubscribeResult { method: "one-click".into(), url: None, error: None })
                    }
                    Ok(r) => tracing::warn!("one-click unsubscribe returned {}", r.status()),
                    Err(e) => tracing::warn!("one-click unsubscribe failed: {e}"),
                }
            }
        }
        if let Some(url) = u.http_url {
            return Ok(UnsubscribeResult { method: "url".into(), url: Some(url), error: None });
        }
        if let Some(mailto) = u.mailto {
            let m = compose::parse_mailto(&mailto).context("bad unsubscribe address")?;
            let account = self.default_account(Some(&meta.account_id))?;
            let mut d = self.blank_draft(&account)?;
            d.to = m.to.iter().filter_map(|a| compose::parse_address(a)).collect();
            d.bcc.clear();
            d.subject = if m.subject.is_empty() { "Unsubscribe".into() } else { m.subject };
            d.html_body = compose::text_to_html(if m.body.is_empty() {
                "Please unsubscribe me from this mailing list."
            } else {
                &m.body
            });
            let mut item = self.send(d, None)?;
            // No undo window for unsubscribe requests.
            item.send_at = now_ms();
            self.inner.store.reschedule_outbox(&item.id, item.send_at)?;
            self.inner.outbox_wake.notify_one();
            return Ok(UnsubscribeResult { method: "email".into(), url: None, error: None });
        }
        Ok(UnsubscribeResult { method: "none".into(), url: None, error: Some("No unsubscribe method available".into()) })
    }
}

/// A message's display parts: sanitised HTML with inline (`cid:`) images
/// embedded, the text part, and the attachments to list. None when the
/// message can't be parsed.
fn render_body(raw: &[u8]) -> Option<(Option<String>, Option<String>, Vec<AttachmentMeta>)> {
    let p = crate::mime::parse(raw)?;
    let wanted: Vec<usize> = p.attachments.iter().filter(|a| a.content_id.is_some()).map(|a| a.index).collect();
    let mut inline = HashMap::new();
    if p.html.is_some() && !wanted.is_empty() {
        for (index, bytes) in crate::mime::attachments_bytes(raw, &wanted) {
            if let Some(a) = p.attachments.iter().find(|a| a.index == index) {
                inline.insert(a.content_id.clone().unwrap_or_default(), (a.mime_type.clone(), bytes));
            }
        }
    }
    let html = p.html.as_deref().map(|h| render::inline_cid_images(&render::sanitize_html(h), &inline));
    let attachments = p.attachments.into_iter().filter(|a| !a.inline).collect();
    Some((html, p.text, attachments))
}

/// What "delete forever" removes on the server: known UIDs, and Message-IDs
/// for rows whose UID a move left unknown.
#[allow(clippy::type_complexity)]
fn expunge_targets(locs: &[&crate::store::Location]) -> (Vec<(String, u32, u32)>, Vec<(String, String)>) {
    let mut by_uid = Vec::new();
    let mut by_message_id = Vec::new();
    for l in locs {
        if l.uid > 0 {
            by_uid.push((l.path.clone(), l.uid_validity, l.uid));
        } else if let Some(mid) = &l.message_id {
            by_message_id.push((l.path.clone(), mid.clone()));
        }
    }
    (by_uid, by_message_id)
}

fn strip_header(raw: &[u8], name: &str) -> Vec<u8> {
    let text = String::from_utf8_lossy(raw);
    let Some(split) = text.find("\r\n\r\n") else { return raw.to_vec() };
    let (head, body) = text.split_at(split);
    let prefix = format!("{}:", name.to_lowercase());
    let mut out = String::new();
    let mut skipping = false;
    for line in head.split("\r\n") {
        if line.starts_with(' ') || line.starts_with('\t') {
            if !skipping {
                out.push_str(line);
                out.push_str("\r\n");
            }
            continue;
        }
        skipping = line.to_lowercase().starts_with(&prefix);
        if !skipping {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    let mut s = out.trim_end_matches("\r\n").to_string();
    s.push_str(body);
    s.into_bytes()
}

// ----------------------------------------------------------------------------
// Account worker
// ----------------------------------------------------------------------------

async fn connect(inner: &Inner, account: &Account) -> Result<ImapConn> {
    let login = login_for(inner, account).await?;
    match ImapConn::connect(account, &login).await {
        Err(e) if imap::is_auth_error(&e) => match &login {
            Login::OAuth(failed) => {
                // A token the server no longer takes: get a new one and try once more.
                forget_token(inner, &account.id, failed);
                ImapConn::connect(account, &login_for(inner, account).await?).await.map_err(|e| {
                    if imap::is_auth_error(&e) {
                        let name = provider_name(&account.auth);
                        oauth::SignInRequired(format!("{name} no longer accepts this account's sign-in. Sign in again.")).into()
                    } else {
                        e
                    }
                })
            }
            Login::Password(_) => Err(e),
        },
        r => r,
    }
}

/// Drop a cached access token, unless another task already replaced it.
fn forget_token(inner: &Inner, account_id: &str, failed: &str) {
    forget_service_token(inner, account_id, Service::Mail, failed);
}

fn forget_service_token(inner: &Inner, account_id: &str, service: Service, failed: &str) {
    let key = token_key(account_id, service);
    let mut tokens = inner.oauth_tokens.lock().unwrap();
    if tokens.get(&key).is_some_and(|(t, _)| t == failed) {
        tokens.remove(&key);
    }
}

/// Log in to IMAP and list the folders. Returns whether the server is Gmail.
async fn verify_imap(server: &ServerConfig, username: &str, login: &Login, allow_invalid: bool) -> Result<bool> {
    let mut conn = ImapConn::connect_with(server, username, login, allow_invalid).await.context("IMAP")?;
    conn.list_folders().await.context("IMAP folder list")?;
    let gmail = conn.caps.gmail;
    conn.logout().await;
    Ok(gmail)
}

/// A debug-only override of a provider endpoint (tests point it at a mock;
/// release builds never read it: refresh tokens would go there).
fn debug_endpoint(var: &str) -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var(var).ok().map(|v| v.trim().trim_end_matches('/').to_string()).filter(|v| !v.is_empty())
}

/// The Microsoft identity platform for the organisation's tenant (`common`
/// when it has none). `REMAIL_MS_AUTHORITY` overrides it in debug builds.
fn microsoft_provider(tenant: Option<&str>) -> oauth::Provider {
    if let Some(a) = debug_endpoint("REMAIL_MS_AUTHORITY") {
        return oauth::Provider::microsoft_at(&a);
    }
    match tenant.map(str::trim).filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')) {
        Some(t) => oauth::Provider::microsoft_at(&format!("https://login.microsoftonline.com/{t}")),
        None => oauth::Provider::microsoft(),
    }
}

/// Google's sign-in. `REMAIL_GOOGLE_AUTHORITY` overrides it in debug builds.
fn google_provider() -> oauth::Provider {
    match debug_endpoint("REMAIL_GOOGLE_AUTHORITY") {
        Some(a) => oauth::Provider::google_at(&a),
        None => oauth::Provider::google(),
    }
}

/// Google Calendar's CalDAV endpoint. `REMAIL_GOOGLE_CALDAV` overrides it in debug builds.
pub(crate) fn google_caldav_base() -> String {
    debug_endpoint("REMAIL_GOOGLE_CALDAV").unwrap_or_else(|| "https://apidata.googleusercontent.com/caldav/v2".into())
}

/// `Google` / `Microsoft` for an account's `auth` (or a grant's provider).
fn provider_name(auth: &str) -> &'static str {
    if auth == oauth::GOOGLE {
        "Google"
    } else {
        "Microsoft"
    }
}

fn provider_for(inner: &Inner, provider: &str) -> oauth::Provider {
    if provider == oauth::GOOGLE {
        google_provider()
    } else {
        microsoft_provider(inner.org.lock().unwrap().as_ref().map(|o| o.providers.microsoft_app.tenant.as_str()))
    }
}

/// The server refused to refresh a Google sign-in: revoked or expired at
/// Google is "sign in again"; anything else is passing.
fn google_server_error(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<CloudError>() {
        Some(c) if c.code == "invalid_token" => {
            oauth::OAuthError { code: "invalid_grant".into(), description: c.message.clone(), detail: c.message.clone() }.into()
        }
        _ => e,
    }
}

/// Save the credential of a mail account (`acc_…`, under its id) or a
/// calendar account (`cal_…`, under `cal:<id>`), and sync it (`cred:<id>`).
/// Every credential written here goes through this, whatever wrote it.
fn set_credential(inner: &Inner, owner_id: &str, secret: &str) -> Result<()> {
    inner.secrets.set(&credential_key(owner_id), secret)?;
    mark_dirty(inner, &sections::cred_section(owner_id));
    Ok(())
}

/// Where an account's credential is kept in the secret store.
pub(crate) fn credential_key(owner_id: &str) -> String {
    if owner_id.starts_with("cal_") {
        format!("cal:{owner_id}")
    } else {
        owner_id.to_string()
    }
}

fn mark_dirty(inner: &Inner, section: &str) {
    let link = inner.link.lock().unwrap().as_ref().and_then(Weak::upgrade);
    if let Some(link) = link {
        link.mark_dirty(section);
    }
}

/// How the account logs in next: its password, or an OAuth access token
/// (refreshed shortly before it expires; a revoked sign-in becomes
/// `SignInRequired`, which asks the user to sign in again).
async fn login_for(inner: &Inner, account: &Account) -> Result<Login> {
    if account.auth == "password" {
        let password = inner.secrets.get(&account.id)?.context("no saved password for this account")?;
        return Ok(Login::Password(password));
    }
    Ok(Login::OAuth(access_token(inner, &account.id, Service::Mail).await?))
}

/// What an access token is for: each Microsoft token works for one service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Service {
    Mail,
    Calendar,
}

/// Where a service's access token is cached.
fn token_key(account_id: &str, service: Service) -> String {
    match service {
        Service::Mail => account_id.to_string(),
        Service::Calendar => format!("{account_id}#calendar"),
    }
}

/// An OAuth access token for one of the account's services, from the cache
/// or the saved sign-in (whose refresh token may rotate). A rotated refresh
/// token is saved and synced as the account's credential alone (`cred:<id>`),
/// so it never races an edit of the account itself.
async fn access_token(inner: &Inner, account_id: &str, service: Service) -> Result<String> {
    let key = token_key(account_id, service);
    let cached = || inner.oauth_tokens.lock().unwrap().get(&key).filter(|(_, exp)| *exp - now_ms() > 120_000).map(|(t, _)| t.clone());
    if let Some(t) = cached() {
        return Ok(t);
    }
    let _one_at_a_time = inner.oauth_refresh.lock().await;
    if let Some(t) = cached() {
        return Ok(t);
    }
    let sign_in_again = |why: &str| anyhow::Error::from(oauth::SignInRequired(why.to_string()));
    let grant: oauth::Grant = inner
        .secrets
        .get(account_id)?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .ok_or_else(|| sign_in_again("Sign in again"))?;
    let provider = provider_for(inner, &grant.provider);
    let name = provider.display_name();
    let scope = match service {
        Service::Calendar if provider.id == oauth::MICROSOFT => crate::graph::SCOPE.to_string(),
        _ => provider.scope.clone(),
    };
    // A Google sign-in the server exchanged (a "Web" client) refreshes there too: only it has the secret.
    let via_server = grant.provider == oauth::GOOGLE && inner.org.lock().unwrap().as_ref().is_some_and(|o| o.google_server_exchange);
    let refreshed = if via_server {
        let link = inner.link.lock().unwrap().as_ref().and_then(Weak::upgrade);
        match link {
            Some(link) => link.google_refresh(grant.refresh_token.clone()).await.map(oauth::tokens_from_server).map_err(google_server_error),
            None => Err(anyhow!("Google sign-in needs the Remail server, which isn't connected")),
        }
    } else {
        oauth::refresh(&inner.http, &provider, &grant.client_id, &grant.refresh_token, &scope).await
    };
    let tokens = match refreshed {
        Ok(t) => t,
        Err(e) => {
            return Err(match (e.downcast_ref::<oauth::OAuthError>(), service) {
                (Some(o), Service::Mail) if o.needs_sign_in() => {
                    sign_in_again(&format!("Your {name} sign-in has expired ({}). Sign in again.", o.to_string().trim_end_matches('.')))
                }
                // Signed in before Remail asked for the calendar, or the permission was taken back.
                (Some(o), Service::Calendar) if o.needs_sign_in() => {
                    sign_in_again(&format!("Sign in to {name} again to allow Remail to show this calendar ({}).", o.to_string().trim_end_matches('.')))
                }
                _ => e,
            });
        }
    };
    // Removed meanwhile: don't bring its keyring entry back.
    if inner.store.account(account_id)?.is_none() {
        bail!("account was removed");
    }
    if let Some(rotated) = tokens.refresh_token.as_ref().filter(|rt| **rt != grant.refresh_token) {
        let grant = oauth::Grant { refresh_token: rotated.clone(), ..grant };
        set_credential(inner, account_id, &serde_json::to_string(&grant)?)?;
    }
    inner.oauth_tokens.lock().unwrap().insert(key, (tokens.access_token.clone(), tokens.expires_at));
    Ok(tokens.access_token)
}

/// Microsoft Graph. Debug builds can point it at a test server with
/// `REMAIL_MS_GRAPH` (never release builds: calendar tokens would go there).
pub(crate) fn graph_base() -> String {
    if cfg!(debug_assertions) {
        if let Ok(g) = std::env::var("REMAIL_MS_GRAPH") {
            if !g.trim().is_empty() {
                return g.trim().trim_end_matches('/').to_string();
            }
        }
    }
    crate::graph::BASE.to_string()
}

/// Run `$body` with a live connection, reconnecting and running it once more
/// when the connection broke. Other errors (a server `NO`, a database error)
/// are returned as they are: running the body again could repeat work that
/// already happened, such as an APPEND.
macro_rules! with_conn {
    ($conn:expr, $inner:expr, $account:expr, |$c:ident| $body:expr) => {{
        let mut attempt = 0;
        loop {
            attempt += 1;
            if $conn.is_none() {
                match connect($inner, $account).await {
                    Ok(c) => $conn = Some(c),
                    Err(e) => break Err(e),
                }
            }
            let $c = $conn.as_mut().unwrap();
            let r = $body.await;
            match r {
                Err(e) if imap::is_connection_error(&e) && attempt < 2 => {
                    tracing::debug!("IMAP connection lost, reconnecting: {e:#}");
                    $conn = None;
                    continue;
                }
                other => break other,
            }
        }
    }};
}

async fn flush_ops(inner: &Inner, conn: &mut Option<ImapConn>, account: &Account) -> Result<usize> {
    let mut done = 0;
    let now = now_ms();
    for op in inner.store.due_ops(&account.id, now)? {
        let parsed: ServerOp = match serde_json::from_str(&op.op_json) {
            Ok(p) => p,
            Err(_) => {
                inner.store.finish_op(op.id)?;
                continue;
            }
        };
        let r: Result<()> = with_conn!(*conn, inner, account, |c| sync::execute_op(c, &inner.store, account, &parsed));
        match r {
            Ok(()) => {
                inner.store.finish_op(op.id)?;
                done += 1;
            }
            Err(e) if imap::is_auth_error(&e) => return Err(e),
            Err(e) => {
                let attempts = op.attempts + 1;
                if attempts >= OP_MAX_ATTEMPTS {
                    tracing::warn!("giving up on server op after {attempts} attempts: {e:#}");
                    sync::revert_op(&inner.store, account, &parsed)?;
                    inner.store.finish_op(op.id)?;
                    done += 1;
                } else {
                    let delay = (1000i64 << attempts).min(30_000);
                    inner.store.retry_op(op.id, now_ms() + delay, &format!("{e:#}"))?;
                }
                // Later ops may depend on this one; keep order.
                break;
            }
        }
    }
    Ok(done)
}

async fn worker_loop(weak: Weak<Inner>, account_id: String, mut rx: mpsc::UnboundedReceiver<Cmd>) {
    let mut conn: Option<ImapConn> = None;
    let mut cache = sync::SyncCache::default();
    let mut next_tick = Instant::now();
    let mut empty_polls: u32 = 0;
    loop {
        let cmd = tokio::select! {
            c = rx.recv() => match c { Some(c) => c, None => break },
            _ = tokio::time::sleep_until(next_tick) => Cmd::Sync { periodic: true, reply: None },
        };
        let Some(inner) = weak.upgrade() else { break };
        let Ok(Some(account)) = inner.store.account(&account_id) else { break };
        let interval = Duration::from_secs(inner.store.setting::<AppSettings>("app").unwrap_or_default().sync_interval_seconds.max(5));

        match cmd {
            Cmd::Sync { periodic, reply } => {
                inner.sync_started.lock().unwrap().insert(account.id.clone());
                if periodic && account.status == AccountStatus::AuthError {
                    next_tick = Instant::now() + POLL_BACKOFF_MAX;
                    continue;
                }
                inner.sink.emit(EngineEvent::SyncStatus { account_id: account.id.clone(), syncing: true, error: None });
                let ops = flush_ops(&inner, &mut conn, &account).await;
                let result: Result<sync::SyncOutcome> = match ops {
                    Err(e) => Err(e),
                    Ok(ops_done) => {
                        let r: Result<sync::SyncOutcome> =
                            with_conn!(conn, &inner, &account, |c| sync::sync_account(c, &inner.store, &account, &mut cache));
                        r.map(|mut o| {
                            o.changed += ops_done;
                            o
                        })
                    }
                };
                match &result {
                    Ok(outcome) => {
                        let _ = inner.store.set_account_status(&account.id, AccountStatus::Active, None);
                        inner.sink.emit(EngineEvent::SyncStatus { account_id: account.id.clone(), syncing: false, error: None });
                        if outcome.changed > 0 {
                            inner.sink.emit(EngineEvent::MailboxChanged { account_id: account.id.clone() });
                        }
                        if outcome.snoozes_cancelled {
                            mark_dirty(&inner, sections::SNOOZES);
                        }
                        let own: Vec<String> = inner.store.accounts().unwrap_or_default().into_iter().map(|a| a.email).collect();
                        let fresh: Vec<MessageMeta> = inner
                            .store
                            .messages_by_ids(&outcome.new_inbox)
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|m| !own.iter().any(|o| o.eq_ignore_ascii_case(&m.from.email)))
                            .filter(|m| m.date > now_ms() - 86_400_000)
                            .collect();
                        let app = inner.store.setting::<AppSettings>("app").unwrap_or_default();
                        let fresh: Vec<MessageMeta> = if app.smart_inbox && app.notify_important_only {
                            fresh
                                .into_iter()
                                .filter(|m| inner.store.thread_category(&m.account_id, &m.thread_id, now_ms()).is_ok_and(|c| c == "primary"))
                                .collect()
                        } else {
                            fresh
                        };
                        if !outcome.new_inbox.is_empty() {
                            inner.ai.sort_wake.notify_one();
                        }
                        if !fresh.is_empty() {
                            inner.sink.emit(EngineEvent::NewMail { account_id: account.id.clone(), messages: fresh });
                        }
                        // Adaptive polling: back off while nothing changes (IDLE covers new mail).
                        if outcome.changed > 0 || !periodic {
                            empty_polls = 0;
                        } else {
                            empty_polls = (empty_polls + 1).min(8);
                        }
                        let wait = (interval * 2u32.pow(empty_polls)).min(POLL_BACKOFF_MAX);
                        next_tick = Instant::now() + wait;
                    }
                    Err(e) => {
                        let msg = friendly_error(e);
                        let status = if imap::is_auth_error(e) { AccountStatus::AuthError } else { AccountStatus::Offline };
                        tracing::warn!(account = %account.email, "sync failed: {e:#}");
                        let _ = inner.store.set_account_status(&account.id, status, Some(&msg));
                        inner.sink.emit(EngineEvent::SyncStatus { account_id: account.id.clone(), syncing: false, error: Some(msg) });
                        conn = None;
                        empty_polls = (empty_polls + 1).min(8);
                        next_tick = Instant::now() + (interval * 2u32.pow(empty_polls)).min(POLL_BACKOFF_MAX);
                    }
                }
                if let Some(reply) = reply {
                    let _ = reply.send(result.map(|_| ()));
                }
            }
            Cmd::FlushOps => match flush_ops(&inner, &mut conn, &account).await {
                Ok(n) if n > 0 => inner.sink.emit(EngineEvent::MailboxChanged { account_id: account.id.clone() }),
                Ok(_) => {
                    // Ops waiting on backoff: try again soon.
                    if inner.store.pending_op_count(&account.id).unwrap_or(0) > 0 {
                        next_tick = next_tick.min(Instant::now() + Duration::from_secs(5));
                    }
                }
                Err(e) => tracing::warn!("flushing ops failed: {e:#}"),
            },
            Cmd::LoadMore { label, reply } => {
                let r = with_conn!(conn, &inner, &account, |c| sync::load_more(c, &inner.store, &account, &label));
                if matches!(r, Ok((n, _)) if n > 0) {
                    inner.sink.emit(EngineEvent::MailboxChanged { account_id: account.id.clone() });
                }
                let _ = reply.send(r);
            }
            Cmd::ServerSearch { query, reply } => {
                let r = with_conn!(conn, &inner, &account, |c| sync::server_search(c, &inner.store, &account, &query));
                let _ = reply.send(r);
            }
            Cmd::FetchBody { message_id, reply } => {
                let r = with_conn!(conn, &inner, &account, |c| sync::fetch_body(c, &inner.store, &message_id));
                let _ = reply.send(r);
            }
            Cmd::Append { label, flags, raw, message_id, reply } => {
                let r = with_conn!(conn, &inner, &account, |c| sync::append_and_record(
                    c,
                    &inner.store,
                    &account,
                    &label,
                    &flags,
                    &raw,
                    &message_id
                ));
                let _ = reply.send(r);
            }
            Cmd::CreateFolder { name, reply } => {
                let encoded = imap::encode_utf7(&name);
                let r = with_conn!(conn, &inner, &account, |c| async {
                    c.create_folder(&encoded).await?;
                    sync::refresh_folders(c, &inner.store, &account).await.map(|_| ())
                });
                let _ = reply.send(r);
            }
        }
    }
    if let Some(c) = conn {
        c.logout().await;
    }
}

/// Second connection parked in IDLE on INBOX; any change triggers a sync.
async fn idle_loop(weak: Weak<Inner>, account_id: String, tx: mpsc::UnboundedSender<Cmd>) {
    // Let the initial sync go first.
    tokio::time::sleep(Duration::from_secs(10)).await;
    let mut failures = 0u32;
    loop {
        let Some(inner) = weak.upgrade() else { return };
        let Ok(Some(account)) = inner.store.account(&account_id) else { return };
        if account.status == AccountStatus::AuthError {
            drop(inner);
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }
        let conn = connect(&inner, &account).await;
        drop(inner);
        let mut conn = match conn {
            Ok(c) if c.caps.idle => c,
            Ok(c) => {
                // No IDLE support: polling only.
                c.logout().await;
                return;
            }
            Err(_) => {
                failures += 1;
                tokio::time::sleep(Duration::from_secs(30 * failures.min(10) as u64)).await;
                continue;
            }
        };
        failures = 0;
        loop {
            let (r, back) = conn.idle("INBOX", IDLE_REFRESH).await;
            match r {
                Ok(true) => {
                    // Let the server finish delivering before we look.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    if tx.send(Cmd::Sync { periodic: false, reply: None }).is_err() {
                        return;
                    }
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::debug!("IDLE ended: {e:#}");
                    break;
                }
            }
            match back {
                Some(c) => conn = c,
                None => break,
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

// ----------------------------------------------------------------------------
// Outbox delivery
// ----------------------------------------------------------------------------

async fn outbox_loop(weak: Weak<Inner>) {
    loop {
        let Some(inner) = weak.upgrade() else { return };
        let now = now_ms();
        let mut next: Option<i64> = None;
        for (id, send_at) in inner.store.outbox_schedule().unwrap_or_default() {
            if send_at > now {
                next = Some(next.map_or(send_at, |n| n.min(send_at)));
                continue;
            }
            // Each delivery runs on its own so a slow server can't hold up the
            // others; claiming here means it is started only once.
            let Ok(Some(item)) = inner.store.outbox_item(&id) else { continue };
            if !inner.store.claim_outbox(&id).unwrap_or(false) {
                continue;
            }
            let inner = inner.clone();
            inner.runtime.clone().spawn(async move { deliver_claimed(&inner, item).await });
        }
        let wait = next.map(|n| (n - now_ms()).clamp(50, 60_000)).unwrap_or(60_000);
        tokio::select! {
            _ = inner.outbox_wake.notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(wait as u64)) => {}
        }
    }
}

async fn deliver(inner: &Arc<Inner>, item: OutboxItem) {
    if inner.store.claim_outbox(&item.id).unwrap_or(false) {
        deliver_claimed(inner, item).await;
    }
}

/// Send an item already claimed (`sending`); `item.attempts` counts the
/// attempts before this one.
async fn deliver_claimed(inner: &Arc<Inner>, item: OutboxItem) {
    inner.sink.emit(EngineEvent::OutboxChanged);
    let subject = item.draft.subject.clone();
    match deliver_inner(inner, &item).await {
        Ok(sent) => {
            // The server took it: forget the item first, so that a crash or
            // quit from here on can't send it a second time.
            if let Err(e) = inner.store.delete_outbox(&item.id) {
                tracing::error!("couldn't remove a sent message from the outbox: {e:#}");
            }
            after_send(inner, &item, sent);
            inner.sink.emit(EngineEvent::OutboxChanged);
            inner.sink.emit(EngineEvent::MessageSent { account_id: item.account_id.clone(), subject });
            inner.sink.emit(EngineEvent::MailboxChanged { account_id: item.account_id.clone() });
        }
        Err(e) => {
            let signed_out = e.downcast_ref::<oauth::SignInRequired>().map(|s| s.0.clone());
            let (permanent, msg) = match e.downcast_ref::<SendError>() {
                _ if signed_out.is_some() => {
                    let m = signed_out.unwrap_or_default();
                    let _ = inner.store.set_account_status(&item.account_id, AccountStatus::AuthError, Some(&m));
                    (true, m)
                }
                Some(SendError::Auth(m)) => {
                    let _ = inner.store.set_account_status(&item.account_id, AccountStatus::AuthError, Some(m));
                    (true, friendly_error(&e))
                }
                Some(SendError::Permanent(m)) => (true, m.clone()),
                Some(SendError::Transient(m)) => (false, m.clone()),
                None => (true, friendly_error(&e)),
            };
            let attempt = item.attempts as usize; // attempts before this claim
            let retry_at = if permanent { None } else { SEND_RETRY_DELAYS_MS.get(attempt).map(|d| now_ms() + d) };
            let _ = inner.store.fail_outbox(&item.id, &msg, retry_at);
            if retry_at.is_none() {
                let _ = inner.store.set_draft_pending(&item.draft.id, false);
                inner.sink.emit(EngineEvent::SendFailed { account_id: item.account_id.clone(), subject, error: msg });
            }
            inner.sink.emit(EngineEvent::OutboxChanged);
        }
    }
}

/// A message the SMTP server accepted.
struct Delivered {
    account: Account,
    message_id: String,
    raw: Vec<u8>,
}

/// Build and submit the message over SMTP; nothing else (see `after_send`).
async fn deliver_inner(inner: &Arc<Inner>, item: &OutboxItem) -> Result<Delivered> {
    let account = inner.store.account(&item.account_id)?.context("account was removed")?;
    let login = match login_for(inner, &account).await {
        Ok(l) => l,
        Err(e) if e.downcast_ref::<oauth::SignInRequired>().is_some() => return Err(e),
        // Offline, DNS, a Microsoft hiccup: retry later like any other network error.
        Err(e) => return Err(SendError::Transient(format!("couldn't renew the Microsoft sign-in: {}", friendly_error(&e))).into()),
    };
    let domain = account.email.rsplit('@').next().unwrap_or("remail.local").to_string();
    let message_id = format!("{}@{}", uuid::Uuid::new_v4().simple(), domain);

    let (in_reply_to, references) = match (&item.draft.reply_to_message_id, item.draft.reply_kind) {
        (Some(orig), Some(k)) if k != ReplyKind::Forward => match inner.store.threading_headers(orig)? {
            Some((mid, irt, refs)) => {
                let r = compose::reply_references(mid.as_deref(), irt.as_deref(), &refs);
                (mid, r)
            }
            None => (None, vec![]),
        },
        _ => (None, vec![]),
    };
    let from = EmailAddress::new(account.display_name.clone(), account.email.clone());
    let (raw, recipients) = compose::build_message(
        &item.draft,
        &compose::OutgoingMeta { from, message_id: message_id.clone(), in_reply_to: in_reply_to.as_deref(), references: &references },
    )?;
    let send = |login: Login| {
        let (account, recipients, raw) = (&account, &recipients, &raw);
        async move { smtp::send(&account.smtp, &account.username, &login, account.allow_invalid_certs, &account.email, recipients, raw).await }
    };
    let sent = match (send(login.clone()).await, &login) {
        (Err(SendError::Auth(_)), Login::OAuth(failed)) => {
            // Once more with a fresh token; if Microsoft still refuses, it's about
            // sending (e.g. SMTP AUTH turned off), not the sign-in, so reading goes on.
            forget_token(inner, &account.id, failed);
            let fresh = match login_for(inner, &account).await {
                Ok(l) => l,
                Err(e) if e.downcast_ref::<oauth::SignInRequired>().is_some() => return Err(e),
                Err(e) => return Err(SendError::Transient(format!("couldn't renew the Microsoft sign-in: {}", friendly_error(&e))).into()),
            };
            send(fresh).await.map_err(|e| match e {
                SendError::Auth(m) => SendError::Permanent(format!(
                    "Microsoft didn't accept sending from this account ({m}). Microsoft 365 organizations can turn off sending from mail apps (SMTP AUTH); ask your admin."
                )),
                other => other,
            })
        }
        (r, _) => r,
    };
    sent.map_err(anyhow::Error::from)?;
    Ok(Delivered { account, message_id, raw })
}

/// Bookkeeping once the server has taken a message: a copy in Sent, \Answered
/// on the original, the draft removed. Only local changes and queued server
/// ops (which survive a restart), so none of it can send the message again.
fn after_send(inner: &Arc<Inner>, item: &OutboxItem, sent: Delivered) {
    let engine = Engine { inner: inner.clone() };
    let account = &sent.account;
    // Gmail files sent mail itself; other servers need an APPEND to Sent.
    if !account.is_gmail {
        let op = ServerOp::Append {
            label: labels::SENT.into(),
            flags: "(\\Seen)".into(),
            message_id: sent.message_id,
            raw_base64: base64::engine::general_purpose::STANDARD.encode(&sent.raw),
        };
        if let Err(e) = engine.queue(&account.id, &op) {
            tracing::warn!("couldn't save a copy to Sent: {e:#}");
        }
    }
    if let (Some(orig), Some(k)) = (&item.draft.reply_to_message_id, item.draft.reply_kind) {
        if k != ReplyKind::Forward {
            let _ = engine.queue(&account.id, &ServerOp::Flags { message_ids: vec![orig.clone()], seen: None, flagged: None, answered: Some(true) });
        }
    }
    let _ = engine.delete_draft(&item.draft.id);
    engine.sync_now(Some(&account.id));
}

/// Model output for writing tasks: refuse empty or (unless `partial_ok`) cut-off answers
/// rather than replacing the user's text with them.
fn usable_text(turn: &ai::Turn, partial_ok: bool) -> Result<String> {
    let text = turn.text();
    if text.trim().is_empty() {
        bail!("The AI returned an empty answer. Try again, or pick another model (or raise its token limit) in Settings.");
    }
    if turn.stop_reason == "max_tokens" && !partial_ok {
        bail!("The AI's answer was cut off at the length limit. Try a shorter text, or raise the limit in Settings.");
    }
    Ok(text)
}

/// Why an outside tool set to run without asking asks anyway.
const UNTRUSTED_APPROVAL: &str = "This chat contains email content, which anyone can write and which may try to \
     instruct the assistant. Check what this tool would send before allowing it.";

/// Secret-store ids of every hidden env var / header value in `s`.
fn mcp_secret_ids(s: &McpSettings) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    for srv in &s.servers {
        let (kind, values) = srv.transport.secret_values();
        for (key, value) in values {
            if value == mcpmod::client::SECRET_PLACEHOLDER {
                ids.insert(mcpmod::client::secret_id(&srv.id, kind, key));
            }
        }
    }
    ids
}

/// Removes a pending tool approval when the chat that asked for it ends.
struct ApprovalGuard<'a> {
    map: &'a Mutex<HashMap<String, oneshot::Sender<bool>>>,
    id: String,
}

impl Drop for ApprovalGuard<'_> {
    fn drop(&mut self) {
        self.map.lock().unwrap().remove(&self.id);
    }
}

// ----------------------------------------------------------------------------
// MCP server tools
// ----------------------------------------------------------------------------

/// Remail's mailbox tools, served to outside agents within the user's permissions.
struct RemailTools {
    inner: Weak<Inner>,
}

impl ToolHandler for RemailTools {
    fn instructions(&self) -> Option<String> {
        Some(
            "Remail is the user's desktop email client (IMAP accounts, all mail cached locally). Find conversations with \
             list_emails or search_emails (Gmail-style operators), then read them with read_thread using the returned \
             account_id and thread_id. Write mail with create_draft so the user can review it; send_email and the \
             organizing tools are available only if the user enabled them in Remail's settings. Sent mail goes through \
             Remail's undo-send delay."
                .into(),
        )
    }

    fn tools(&self) -> Vec<ToolInfo> {
        let Some(inner) = self.inner.upgrade() else { return vec![] };
        let engine = Engine { inner };
        assistant::tool_defs(engine.mcp_settings().permissions)
            .into_iter()
            .map(|t| ToolInfo { name: t.name, description: t.description, input_schema: t.input_schema })
            .collect()
    }

    fn call(&self, name: String, arguments: serde_json::Value) -> futures::future::BoxFuture<'static, ToolOutput> {
        let weak = self.inner.clone();
        Box::pin(async move {
            let Some(inner) = weak.upgrade() else { return ToolOutput::error("Remail is shutting down") };
            let engine = Engine { inner };
            let permissions = engine.mcp_settings().permissions;
            let r = assistant::call(&engine, &name, &arguments, assistant::CallOptions { permissions, open_drafts: false }).await;
            ToolOutput { text: r.text, is_error: r.is_error }
        })
    }
}

// ----------------------------------------------------------------------------
// Auto-clean
// ----------------------------------------------------------------------------

/// Read Inbox conversations whose rule's delay has passed.
fn autoclean_due(inner: &Inner, cfg: &AutoClean, now: i64, viewing: Option<&ThreadRef>) -> Result<Vec<ThreadRef>> {
    if !cfg.enabled || cfg.rules.is_empty() {
        return Ok(vec![]);
    }
    let since = cfg.rules.iter().map(|r| r.since).min().unwrap_or(now);
    let shortest = cfg.rules.iter().map(|r| r.after_minutes.max(0)).min().unwrap_or(0);
    let mut out = Vec::new();
    for f in inner.store.autoclean_facts(since, now - shortest * 60_000, now)? {
        // Never: unread, starred, conversations you wrote in, or mail read before we tracked it.
        if !f.in_inbox || f.unread > 0 || f.starred || f.mine || f.read_unknown {
            continue;
        }
        let Some(read_at) = f.read_at else { continue };
        if viewing.is_some_and(|v| v.account_id == f.account_id && v.thread_id == f.thread_id) {
            continue;
        }
        let matches = cfg.rules.iter().any(|r| {
            // Codes only outside Important: a colleague's "your code is ready" stays.
            let kind_ok = if r.kind == "otp" { f.otp && f.category != "primary" } else { f.category == r.kind };
            kind_ok && read_at >= r.since && read_at + r.after_minutes.max(0) * 60_000 <= now
        });
        if matches {
            out.push(ThreadRef { account_id: f.account_id, thread_id: f.thread_id });
        }
    }
    Ok(out)
}

/// Database upkeep: re-index mail stored before the current full-text index
/// (`Store::reindex_fts_batch`), then refresh the query planner's statistics
/// every few hours, as SQLite suggests for long-lived connections.
async fn db_maintenance(weak: Weak<Inner>) {
    tokio::time::sleep(Duration::from_secs(10)).await;
    loop {
        let Some(inner) = weak.upgrade() else { return };
        match tokio::task::spawn_blocking(move || inner.store.reindex_fts_batch(200)).await {
            Ok(Ok(0)) | Err(_) => break,
            // Short pauses leave the database to sync and the UI.
            Ok(Ok(_)) => tokio::time::sleep(Duration::from_millis(200)).await,
            Ok(Err(e)) => {
                tracing::warn!("re-indexing old mail for search failed: {e:#}");
                break;
            }
        }
    }
    loop {
        let Some(inner) = weak.upgrade() else { return };
        let _ = tokio::task::spawn_blocking(move || inner.store.optimize()).await;
        tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
    }
}

/// Fill in the one-time-code flag for mail stored before it existed.
fn otp_backlog(inner: &Inner) {
    loop {
        let rows = match inner.store.otp_unknown(500) {
            Ok(r) if !r.is_empty() => r,
            _ => return,
        };
        let flags: Vec<(String, bool)> =
            rows.into_iter().map(|(id, subject, snippet)| (id, crate::classify::is_one_time_code(&subject, &snippet))).collect();
        if inner.store.set_otp(&flags).is_err() || flags.len() < 500 {
            return;
        }
    }
}

async fn autoclean_loop(weak: Weak<Inner>) {
    tokio::time::sleep(Duration::from_secs(5)).await;
    if let Some(inner) = weak.upgrade() {
        let _ = tokio::task::spawn_blocking(move || otp_backlog(&inner)).await;
    }
    loop {
        let Some(inner) = weak.upgrade() else { return };
        let cfg: AppSettings = inner.store.setting("app").unwrap_or_default();
        let viewing = inner.viewing.lock().unwrap().clone();
        let now = now_ms();
        match autoclean_due(&inner, &cfg.auto_clean, now, viewing.as_ref()) {
            Ok(due) if !due.is_empty() => {
                let engine = Engine { inner: inner.clone() };
                let dest = if cfg.auto_clean.action == "archive" { labels::ARCHIVE } else { labels::TRASH };
                // Only the Inbox copies: copies filed in other folders stay put.
                let (mut threads, mut messages) = (Vec::new(), Vec::new());
                for t in due {
                    let ids = inner.store.inbox_message_ids(&t.account_id, &t.thread_id).unwrap_or_default();
                    match engine.move_messages(&ids, dest) {
                        Ok(()) => {
                            messages.extend(ids);
                            threads.push(t);
                        }
                        Err(e) => tracing::warn!("auto-clean failed: {e:#}"),
                    }
                }
                if !threads.is_empty() {
                    tracing::info!(count = threads.len(), "auto-clean");
                    inner.sink.emit(EngineEvent::AutoCleaned { threads, messages, action: cfg.auto_clean.action.clone() });
                }
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("auto-clean check failed: {e:#}"),
        }
        tokio::select! {
            _ = inner.clean_wake.notified() => {
                // Let a burst of changes (open → mark read → move on) settle.
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
        }
    }
}

// ----------------------------------------------------------------------------
// Smart Inbox background work
// ----------------------------------------------------------------------------

/// Classify messages stored before Smart Inbox existed.
async fn categorize_backlog(weak: Weak<Inner>) {
    loop {
        let Some(inner) = weak.upgrade() else { return };
        let batch = match inner.store.uncategorized(200) {
            Ok(b) if !b.is_empty() => b,
            _ => return,
        };
        // Header parsing is CPU work: keep it off the async threads.
        let rows: Vec<(String, &str)> = match tokio::task::spawn_blocking(move || {
            batch
                .into_iter()
                .map(|(id, raw, from, subject, unsub)| {
                    let cat = raw
                        .as_deref()
                        .and_then(crate::mime::parse_headers_category)
                        .unwrap_or_else(|| crate::classify::classify_basic(&from, &subject, unsub));
                    (id, cat.as_str())
                })
                .collect()
        })
        .await
        {
            Ok(rows) => rows,
            Err(_) => return,
        };
        if let Err(e) = inner.store.set_categories(&rows, false) {
            tracing::warn!("categorizing old mail failed: {e:#}");
            return;
        }
        let done = rows.len() < 200;
        if done {
            for a in inner.store.accounts().unwrap_or_default() {
                inner.sink.emit(EngineEvent::MailboxChanged { account_id: a.id });
            }
            return;
        }
        drop(inner);
        // Leave the database to the UI between batches.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Let the AI refine Smart Inbox sorting of recent Inbox mail (opt-in).
async fn ai_sort_loop(weak: Weak<Inner>) {
    tokio::time::sleep(Duration::from_secs(20)).await;
    loop {
        let Some(inner) = weak.upgrade() else { return };
        let engine = Engine { inner: inner.clone() };
        let s = engine.ai_settings();
        let mut wait = Duration::from_secs(600);
        if s.enabled && s.smart_sort {
            match engine.ai_client() {
                Ok(client) => {
                    let since = now_ms() - 7 * 86_400_000;
                    for _ in 0..4 {
                        let batch = inner.store.ai_unsorted(since, 25).unwrap_or_default();
                        if batch.is_empty() {
                            break;
                        }
                        let items: Vec<ai::SortItem> = batch
                            .iter()
                            .map(|c| ai::SortItem { id: c.id.clone(), from: c.from.clone(), subject: c.subject.clone(), snippet: c.snippet.clone(), bulk: c.bulk })
                            .collect();
                        match ai::classify_batch(&client, &items).await {
                            Ok(sorted) => {
                                // Unanswered ids keep the heuristic so they aren't retried forever.
                                let mut rows: Vec<(String, &str)> = batch.iter().map(|c| (c.id.clone(), heuristic_str(&c.heuristic))).collect();
                                for (id, cat) in &sorted {
                                    if let Some(r) = rows.iter_mut().find(|r| &r.0 == id) {
                                        r.1 = cat.as_str();
                                    }
                                }
                                let _ = inner.store.set_categories(&rows, true);
                                for a in inner.store.accounts().unwrap_or_default() {
                                    inner.sink.emit(EngineEvent::MailboxChanged { account_id: a.id });
                                }
                            }
                            Err(e) => {
                                tracing::warn!("AI sorting failed: {e:#}");
                                break;
                            }
                        }
                    }
                    wait = Duration::from_secs(120);
                }
                Err(e) => tracing::debug!("AI sorting unavailable: {e:#}"),
            }
        }
        drop(engine);
        tokio::select! {
            _ = inner.ai.sort_wake.notified() => {}
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

fn heuristic_str(s: &str) -> &'static str {
    crate::classify::Category::parse(s).unwrap_or_default().as_str()
}

// ----------------------------------------------------------------------------
// Snooze wake-ups
// ----------------------------------------------------------------------------

async fn snooze_loop(weak: Weak<Inner>) {
    loop {
        let Some(inner) = weak.upgrade() else { return };
        let now = now_ms();
        for (account_id, thread_id, subject) in inner.store.due_snoozes(now).unwrap_or_default() {
            let _ = inner.store.mark_snooze_notified(&account_id, &thread_id);
            inner.sink.emit(EngineEvent::SnoozeWoke { account_id: account_id.clone(), thread_id, subject });
            inner.sink.emit(EngineEvent::MailboxChanged { account_id });
        }
        let next = inner.store.next_snooze_wake(now).ok().flatten();
        let wait = next.map(|n| (n - now_ms()).clamp(100, 60_000)).unwrap_or(60_000);
        tokio::select! {
            _ = inner.snooze_wake.notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(wait as u64)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{account, put, raw_message};

    struct Quiet;
    impl EventSink for Quiet {
        fn emit(&self, _: EngineEvent) {}
    }

    #[test]
    fn google_refused_by_the_server_means_sign_in_again() {
        let revoked = google_server_error(CloudError::new("invalid_token", "Google refused the sign-in (invalid_grant).").into());
        assert!(revoked.downcast_ref::<oauth::OAuthError>().is_some_and(|o| o.needs_sign_in()), "{revoked:#}");
        let passing = google_server_error(CloudError::new("unavailable", "Google didn't answer.").into());
        assert!(passing.downcast_ref::<oauth::OAuthError>().is_none());
        assert_eq!(passing.downcast_ref::<CloudError>().map(|c| c.code.as_str()), Some("unavailable"));
    }

    #[tokio::test]
    async fn credentials_are_marked_for_sync_wherever_they_are_saved() {
        struct Spy(Mutex<Vec<String>>);
        impl CloudLink for Spy {
            fn mark_dirty(&self, section: &str) {
                self.0.lock().unwrap().push(section.to_string());
            }
            fn google_exchange(&self, _: remail_proto::api::GoogleExchange) -> futures::future::BoxFuture<'static, Result<remail_proto::api::GoogleTokens>> {
                Box::pin(async { bail!("not here") })
            }
            fn google_refresh(&self, _: String) -> futures::future::BoxFuture<'static, Result<remail_proto::api::GoogleTokens>> {
                Box::pin(async { bail!("not here") })
            }
        }
        let secrets = Arc::new(crate::secrets::MemoryStore::default());
        let engine = Engine::with_store(Store::open_in_memory().unwrap(), secrets.clone(), Arc::new(Quiet)).unwrap();
        let spy = Arc::new(Spy(Mutex::new(Vec::new())));
        let link: Arc<dyn CloudLink> = spy.clone();
        engine.set_cloud_link(Arc::downgrade(&link));
        engine.set_credential("acc_1", "pw").unwrap();
        engine.set_credential("cal_2", "feed-pw").unwrap();
        assert_eq!(secrets.get("acc_1").unwrap().as_deref(), Some("pw"));
        assert_eq!(secrets.get("cal:cal_2").unwrap().as_deref(), Some("feed-pw"), "calendar credentials keep their key");
        assert_eq!(*spy.0.lock().unwrap(), ["cred:acc_1", "cred:cal_2"]);
    }

    #[tokio::test]
    async fn autoclean_rules() {
        let engine = Engine::with_store(Store::open_in_memory().unwrap(), Arc::new(crate::secrets::MemoryStore::default()), Arc::new(Quiet)).unwrap();
        let store = engine.store();
        let me = account(store, "me@example.com");
        let promo = "List-Unsubscribe: <https://shop.test/u>\r\nX-Mailchimp-Campaign: 1\r\n";
        // Read before the feature was turned on: never touched.
        let old = put(store, &me, "INBOX", 1, raw_message("Shop <news@shop.test>", "me@example.com", "Old sale", promo, "old@x"), 1000);
        store.set_flags_local(&[old], Some(false), None).unwrap();

        let cfg = AppSettings {
            auto_clean: AutoClean {
                enabled: true,
                action: "trash".into(),
                rules: vec![
                    CleanRule { kind: "promotions".into(), after_minutes: 0, since: 0 },
                    CleanRule { kind: "otp".into(), after_minutes: 60, since: 0 },
                ],
            },
            ..Default::default()
        };
        std::thread::sleep(Duration::from_millis(5));
        let saved = engine.set_settings(&cfg).unwrap();
        assert!(saved.auto_clean.rules.iter().all(|r| r.since > 1000));
        std::thread::sleep(Duration::from_millis(5));

        let read = |subject: &str, from: &str, extra: &str, uid: u32| {
            let id = put(store, &me, "INBOX", uid, raw_message(from, "me@example.com", subject, extra, &format!("{uid}@x")), now_ms());
            store.set_flags_local(std::slice::from_ref(&id), Some(false), None).unwrap();
            id
        };
        read("50% off", "Shop <news@shop.test>", promo, 2);
        read("Your verification code 482913", "Bank <no-reply@bank.test>", "", 3);
        read("Lunch?", "Sara <sara@acme.test>", "", 4);
        put(store, &me, "INBOX", 5, raw_message("Shop <news@shop.test>", "me@example.com", "Unread sale", promo, "5@x"), now_ms());
        let starred = read("Starred sale", "Shop <news@shop.test>", promo, 6);
        store.set_flags_local(&[starred], None, Some(true)).unwrap();

        let ids = |due: Vec<ThreadRef>| {
            let mut v: Vec<String> = due.into_iter().map(|t| t.thread_id).collect();
            v.sort();
            v
        };
        let cfg = engine.settings().auto_clean;
        let now = now_ms();
        assert_eq!(ids(autoclean_due(&engine.inner, &cfg, now, None).unwrap()), vec!["t:2@x"]);
        // The open conversation waits until you move on.
        let open = ThreadRef { account_id: me.id.clone(), thread_id: "t:2@x".into() };
        assert!(autoclean_due(&engine.inner, &cfg, now, Some(&open)).unwrap().is_empty());
        // An hour later the one-time code goes too.
        assert_eq!(ids(autoclean_due(&engine.inner, &cfg, now + 61 * 60_000, None).unwrap()), vec!["t:2@x", "t:3@x"]);
        // Rescued conversations stay.
        store.autoclean_exempt(&[(me.id.clone(), "t:2@x".into())]).unwrap();
        assert!(autoclean_due(&engine.inner, &cfg, now, None).unwrap().is_empty());

        // A code-looking email from a person (Important) is never treated as a code.
        read("Your code is 739204", "Sara <sara@acme.test>", "", 7);
        assert_eq!(ids(autoclean_due(&engine.inner, &cfg, now + 61 * 60_000, None).unwrap()), vec!["t:3@x"]);
        // Snoozed conversations are left alone, also after they come back.
        read("Flash sale", "Shop <news@shop.test>", promo, 8);
        assert_eq!(ids(autoclean_due(&engine.inner, &cfg, now_ms(), None).unwrap()), vec!["t:8@x"]);
        let flash = ThreadRef { account_id: me.id.clone(), thread_id: "t:8@x".into() };
        store.snooze(&me.id, "t:8@x", now_ms() - 1).unwrap();
        assert!(autoclean_due(&engine.inner, &cfg, now_ms(), None).unwrap().is_empty());
        store.unsnooze(&me.id, "t:8@x").unwrap();
        // Putting it back in the Inbox by hand keeps it there.
        engine.modify_threads(std::slice::from_ref(&flash), &ThreadAction::MoveToInbox { from: None }).unwrap();
        assert!(autoclean_due(&engine.inner, &cfg, now_ms(), None).unwrap().is_empty());
    }

    #[test]
    fn strips_to_header() {
        let raw = b"From: a@x\r\nTo: b@x,\r\n c@x\r\nSubject: s\r\n\r\nbody";
        assert_eq!(String::from_utf8(strip_header(raw, "To")).unwrap(), "From: a@x\r\nSubject: s\r\n\r\nbody");
        let strip = |raw: &[u8]| String::from_utf8(strip_header(raw, "To")).unwrap();
        // Any case; a header just before the blank line; similar names stay.
        assert_eq!(strip(b"to: b@x\r\nTo-Do: x\r\nFrom: a@x\r\nTO: c@x\r\n\r\nbody"), "To-Do: x\r\nFrom: a@x\r\n\r\nbody");
        // No header/body separator: left alone.
        assert_eq!(strip(b"From: a@x\r\nTo: b@x"), "From: a@x\r\nTo: b@x");
    }

    #[test]
    fn errors_and_settings() {
        let e: anyhow::Error = imap::AuthError("NO bad".into()).into();
        assert!(friendly_error(&e).contains("app password"), "{}", friendly_error(&e));
        let e: anyhow::Error = SendError::Auth("535".into()).into();
        assert_eq!(friendly_error(&e), "The SMTP server rejected the username or password.");
        // Times that aren't HH:MM fall back to the defaults.
        let t = AppSettings { snooze_morning: "8:30".into(), snooze_evening: "19:15".into(), ..Default::default() }.snooze_times();
        assert_eq!(t.morning, presets::SnoozeTimes::default().morning);
        assert_eq!(t.evening, presets::parse_hhmm("19:15").unwrap());
    }

    #[test]
    fn usable_ai_text() {
        let turn = |text: &str, stop: &str| ai::Turn { content: vec![ai::Block::Text { text: text.into() }], stop_reason: stop.into(), bad_args: vec![] };
        assert_eq!(usable_text(&turn("Hello", "end_turn"), false).unwrap(), "Hello");
        assert!(usable_text(&turn("  ", "end_turn"), true).is_err());
        assert!(usable_text(&turn("Hel", "max_tokens"), false).unwrap_err().to_string().contains("cut off"));
        assert_eq!(usable_text(&turn("Hel", "max_tokens"), true).unwrap(), "Hel");
    }

    #[test]
    fn delete_forever_targets() {
        let loc = |uid: u32, mid: Option<&str>| crate::store::Location {
            id: "m".into(),
            account_id: "a".into(),
            thread_id: "t".into(),
            folder: labels::TRASH.into(),
            path: "Trash".into(),
            uid_validity: 4,
            uid,
            message_id: mid.map(String::from),
            is_unread: false,
            is_starred: false,
        };
        let (a, b, c) = (loc(9, Some("x@y")), loc(0, Some("moved@y")), loc(0, None));
        let (by_uid, by_mid) = expunge_targets(&[&a, &b, &c]);
        assert_eq!(by_uid, vec![("Trash".to_string(), 4, 9)]);
        assert_eq!(by_mid, vec![("Trash".to_string(), "moved@y".to_string())], "a moved row is found by Message-ID");
    }

    #[test]
    fn renders_inline_images_and_lists_attachments() {
        let raw = "From: a@x\r\nSubject: s\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=\"b\"\r\n\r\n\
            --b\r\nContent-Type: text/html\r\n\r\n<p onclick=\"x()\">Hi <img src=\"cid:logo@x\"></p>\r\n\
            --b\r\nContent-Type: image/png\r\nContent-ID: <logo@x>\r\nContent-Transfer-Encoding: base64\r\n\r\nAQID\r\n\
            --b\r\nContent-Type: application/pdf; name=\"a.pdf\"\r\nContent-Disposition: attachment; filename=\"a.pdf\"\r\n\r\n%PDF\r\n\
            --b--\r\n";
        let (html, _, attachments) = render_body(raw.as_bytes()).unwrap();
        let html = html.unwrap();
        assert!(html.contains("data:image/png;base64,AQID"), "{html}");
        assert!(!html.contains("onclick"));
        assert_eq!(attachments.iter().map(|a| a.filename.as_str()).collect::<Vec<_>>(), vec!["a.pdf"]);
    }

    #[test]
    fn gmail_flag_follows_server_changes() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        assert!(!me.is_gmail);
        store.set_account_gmail(&me.id, true).unwrap();
        assert!(store.account(&me.id).unwrap().unwrap().is_gmail);
    }
}
