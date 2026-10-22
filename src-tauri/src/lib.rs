//! App shell (desktop and mobile): hosts the Remail engine, exposes it to
//! the UI as Tauri commands, forwards engine events, and owns windows, tray
//! and OS notifications. Phones have one window: compose and settings are
//! screens inside it (`mobile-open-compose` / `mobile-open-settings` events),
//! and there is no tray, single-instance lock or MCP.

mod commands;
#[cfg(mobile)]
mod mobile;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use remail_core::cloud::{Cloud, CloudEvent, CloudOptions, CloudSink, Phase};
use remail_core::engine::AppSettings;
#[cfg(desktop)]
use remail_core::secrets::KeyringStore;
use remail_core::{Engine, EngineEvent, EventSink};
#[cfg(desktop)]
use tauri::menu::{Menu, MenuItem};
#[cfg(desktop)]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
#[cfg(desktop)]
use tauri::WebviewUrl;
use tauri::{AppHandle, Emitter, Manager, RunEvent, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_notification::NotificationExt;

pub struct AppState {
    pub engine: Engine,
    /// The Remail server connection; mail runs only once it's `ready`.
    pub cloud: Cloud,
    /// Open compose windows by draft id → window label.
    pub compose_windows: Mutex<HashMap<String, String>>,
    pub next_window: Mutex<u64>,
}

/// `mailto:` links that arrive before setup has started the engine; `None`
/// once setup has handled them.
struct EarlyArgs(Mutex<Option<Vec<String>>>);

/// Wakes the tray updater: mailbox changes come in bursts and counting
/// unread mail on every one (often on the main thread) would stall the UI.
#[derive(Default)]
struct TrayRefresh {
    dirty: AtomicBool,
    wake: tokio::sync::Notify,
}

impl TrayRefresh {
    fn request(&self) {
        self.dirty.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }
}

struct UiSink {
    app: AppHandle,
    tray: Arc<TrayRefresh>,
}

impl UiSink {
    fn main_focused(&self) -> bool {
        self.app
            .get_webview_window("main")
            .and_then(|w| w.is_focused().ok())
            .unwrap_or(false)
    }

    fn settings(&self) -> AppSettings {
        self.app.try_state::<AppState>().map(|s| s.engine.settings()).unwrap_or_default()
    }

    fn notify(&self, title: &str, body: &str) {
        let builder = self.app.notification().builder().title(title).body(body);
        // Android: Remail's status-bar icon (res/drawable/ic_stat_remail), and
        // the expanded text shown when a notification is opened up.
        #[cfg(mobile)]
        let builder = builder.icon("ic_stat_remail").icon_color("#2C3D66").large_body(body);
        let _ = builder.show();
    }
}

impl EventSink for UiSink {
    fn emit(&self, event: EngineEvent) {
        match &event {
            EngineEvent::NewMail { messages, .. } if self.settings().notifications && !self.main_focused() => {
                if messages.len() > 3 {
                    self.notify("Camel Mail", &format!("{} new emails", messages.len()));
                } else {
                    for m in messages {
                        let subject = if m.subject.is_empty() { "(No subject)" } else { m.subject.as_str() };
                        self.notify(&m.from.display(), subject);
                    }
                }
            }
            EngineEvent::SnoozeWoke { subject, .. } if self.settings().notify_on_snooze_wake && !self.main_focused() => {
                self.notify("Snoozed email is back", subject);
            }
            EngineEvent::SendFailed { subject, error, .. } if !self.main_focused() => {
                self.notify(&format!("Couldn't send \"{subject}\""), error);
            }
            EngineEvent::MailboxChanged { .. } => self.tray.request(),
            EngineEvent::OpenCompose { draft_id } => {
                let _ = open_compose_window(&self.app, draft_id);
            }
            EngineEvent::EventReminder { summary, start_ms, location, all_day, day } => {
                let mins = (start_ms - chrono_now_ms()) / 60_000;
                let when = if *all_day || mins >= 24 * 60 {
                    day.clone()
                } else if mins <= 0 {
                    "Now".to_string()
                } else if mins < 60 {
                    format!("In {mins} min")
                } else {
                    format!("In {} h {} min", mins / 60, mins % 60)
                };
                let body = if location.is_empty() { when } else { format!("{when} · {location}") };
                self.notify(if summary.is_empty() { "Event" } else { summary }, &body);
            }
            _ => {}
        }
        let _ = self.app.emit("engine-event", &event);
    }
}

/// Forwards Remail server events to every window (`cloud-event`), and starts
/// or stops what may only run while signed in.
struct CloudUi {
    app: AppHandle,
    tray: Arc<TrayRefresh>,
    /// The last phase seen, to act on changes only.
    phase: Mutex<Option<Phase>>,
}

impl CloudSink for CloudUi {
    fn cloud_event(&self, event: CloudEvent) {
        if let CloudEvent::Status { status } = &event {
            let changed = self.phase.lock().unwrap().replace(status.phase) != Some(status.phase);
            if changed {
                self.tray.request();
                if cfg!(desktop) && status.phase == Phase::Ready {
                    // Agents may reach mail now (if the user turned the MCP server on).
                    if let Some(state) = self.app.try_state::<AppState>() {
                        let engine = state.engine.clone();
                        tauri::async_runtime::spawn(async move { engine.apply_mcp_server().await });
                    }
                }
            }
        }
        let _ = self.app.emit("cloud-event", &event);
    }
}

/// Mail is running (signed in to Remail and the setup is here).
pub fn mail_ready(app: &AppHandle) -> bool {
    app.try_state::<AppState>().is_some_and(|s| s.cloud.status().phase == Phase::Ready)
}

fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(desktop)]
/// Keeps the tray tooltip's unread count current: at most one count per
/// ~500 ms, on the blocking pool, and only touches the tray when the text changes.
async fn tray_updater(app: AppHandle, engine: Engine, refresh: Arc<TrayRefresh>) {
    let mut shown = "Camel Mail".to_string();
    loop {
        refresh.wake.notified().await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        if !refresh.dirty.swap(false, Ordering::SeqCst) {
            continue;
        }
        let engine = engine.clone();
        // Signed out: no mail counts in the tray.
        let ready = mail_ready(&app);
        let unread: i64 = if ready {
            tauri::async_runtime::spawn_blocking(move || engine.unread_counts().map(|m| m.values().sum()).unwrap_or(0)).await.unwrap_or(0)
        } else {
            0
        };
        let tip = if unread > 0 { format!("Camel Mail - {unread} unread") } else { "Camel Mail".to_string() };
        if tip != shown {
            if let Some(tray) = app.tray_by_id("main") {
                let _ = tray.set_tooltip(Some(&tip));
            }
            shown = tip;
        }
    }
}

/// Quit, but first deliver anything still inside its undo-send window
/// (nothing else would send them before the next launch).
pub fn quit(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        app.exit(0);
        return;
    };
    state.engine.shutdown_mcp();
    let pending = state
        .engine
        .outbox()
        .map(|o| o.iter().any(|i| i.kind == remail_core::OutboxKind::Undo))
        .unwrap_or(false);
    if !pending {
        app.exit(0);
        return;
    }
    for w in app.webview_windows().values() {
        let _ = w.hide();
    }
    let engine = state.engine.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        engine.flush_outbox(Duration::from_secs(15)).await;
        app.exit(0);
    });
}

/// Email bodies render in sandboxed frames where no script runs, so their
/// links (`<base target="_blank">`) arrive as new-window requests: open web
/// links in the browser (and `mailto:` in compose), never inside Remail.
fn links_open_outside(app: AppHandle) -> impl Fn(tauri::Url, tauri::webview::NewWindowFeatures) -> tauri::webview::NewWindowResponse<tauri::Wry> + Send + Sync + 'static {
    move |url, _| {
        if matches!(url.scheme(), "http" | "https" | "mailto") {
            let app = app.clone();
            let url = url.to_string();
            tauri::async_runtime::spawn(async move {
                let Some(state) = app.try_state::<AppState>() else { return };
                if let Err(e) = commands::open_url(app.clone(), state, url).await {
                    tracing::warn!("couldn't open a link: {e}");
                }
            });
        }
        tauri::webview::NewWindowResponse::Deny
    }
}

#[cfg(desktop)]
pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[cfg(mobile)]
pub fn show_main(_app: &AppHandle) {}

/// The phone's compose screen for a saved draft.
#[cfg(mobile)]
pub fn open_compose_window(app: &AppHandle, draft_id: &str) -> tauri::Result<()> {
    app.emit_to("main", "mobile-open-compose", draft_id)
}

/// The phone's settings screen.
#[cfg(mobile)]
pub fn open_settings_window(app: &AppHandle, section: Option<&str>) -> tauri::Result<()> {
    app.emit_to("main", "mobile-open-settings", section)
}

/// Open (or focus) the compose window for a saved draft.
#[cfg(desktop)]
pub fn open_compose_window(app: &AppHandle, draft_id: &str) -> tauri::Result<()> {
    let state = app.state::<AppState>();
    // Check and reserve under one lock so two requests can't both build a
    // window for the draft; the window calls happen after it's released.
    let reserved = {
        let mut windows = state.compose_windows.lock().unwrap();
        match windows.get(draft_id) {
            Some(label) => Err(label.clone()),
            None => {
                let mut n = state.next_window.lock().unwrap();
                *n += 1;
                let label = format!("compose-{}", *n);
                windows.insert(draft_id.to_string(), label.clone());
                Ok(label)
            }
        }
    };
    let label = match reserved {
        Ok(label) => label,
        Err(label) => {
            // No window yet: another request is still building it.
            if let Some(w) = app.get_webview_window(&label) {
                let _ = w.unminimize();
                return w.set_focus();
            }
            return Ok(());
        }
    };
    let url = WebviewUrl::App(format!("index.html#compose={draft_id}").into());
    let built = WebviewWindowBuilder::new(app, &label, url)
        .on_new_window(links_open_outside(app.clone()))
        .title("New Message - Camel Mail")
        .inner_size(800.0, 720.0)
        .min_inner_size(500.0, 450.0)
        .build();
    if let Err(e) = built {
        state.compose_windows.lock().unwrap().retain(|_, l| *l != label);
        return Err(e);
    }
    Ok(())
}

/// Bring the main window forward showing the calendar.
pub fn show_calendar(app: &AppHandle) {
    show_main(app);
    let _ = app.emit_to("main", "show-calendar", ());
}

#[cfg(desktop)]
pub fn open_settings_window(app: &AppHandle, section: Option<&str>) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window("settings") {
        if let Some(s) = section {
            let _ = w.emit("settings-section", s);
        }
        let _ = w.unminimize();
        return w.set_focus();
    }
    let hash = match section {
        Some(s) => format!("index.html#settings={s}"),
        None => "index.html#settings".to_string(),
    };
    WebviewWindowBuilder::new(app, "settings", WebviewUrl::App(hash.into()))
        .title("Settings - Camel Mail")
        .inner_size(860.0, 640.0)
        .min_inner_size(640.0, 500.0)
        .build()?;
    Ok(())
}

/// `mailto:` from the command line (first launch or a second instance).
fn handle_args(app: &AppHandle, args: &[String]) {
    let Some(url) = args.iter().find(|a| a.to_ascii_lowercase().starts_with("mailto:")) else { return };
    // A second instance can call in while setup is still starting the engine.
    if let Some(early) = app.try_state::<EarlyArgs>() {
        if let Some(queue) = early.0.lock().unwrap().as_mut() {
            queue.push(url.clone());
            return;
        }
    }
    let Some(state) = app.try_state::<AppState>() else { return };
    if !mail_ready(app) {
        tracing::info!("ignoring a mailto link: not signed in to Remail yet");
        return;
    }
    match state.engine.mailto_draft(url, None) {
        Ok(draft) => {
            let engine = state.engine.clone();
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Ok(d) = engine.save_draft(draft, false).await {
                    let _ = open_compose_window(&app, &d.id);
                }
            });
        }
        Err(e) => tracing::warn!("ignoring mailto link: {e:#}"),
    }
}

#[cfg(desktop)]
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Camel Mail", true, None::<&str>)?;
    let compose = MenuItem::with_id(app, "compose", "Compose New Email", true, None::<&str>)?;
    let calendar = MenuItem::with_id(app, "calendar", "Open Calendar", true, None::<&str>)?;
    let sync = MenuItem::with_id(app, "sync", "Check for New Mail", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit Camel Mail", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &compose, &calendar, &sync, &quit_item])?;
    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().cloned().expect("bundled icon"))
        .tooltip("Camel Mail")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "compose" if mail_ready(app) => {
                let state = app.state::<AppState>();
                if let Ok(d) = state.engine.new_draft(None) {
                    let engine = state.engine.clone();
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Ok(d) = engine.save_draft(d, false).await {
                            let _ = open_compose_window(&app, &d.id);
                        }
                    });
                }
            }
            "calendar" => show_calendar(app),
            "sync" if mail_ready(app) => app.state::<AppState>().engine.sync_now(None),
            "quit" => quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "remail=info,remail_core=info".into()),
        )
        .init();

    let quitting = Arc::new(AtomicBool::new(false));
    let builder = tauri::Builder::default().manage(EarlyArgs(Mutex::new(Some(Vec::new()))));
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
        if !args.iter().any(|a| a == "--background") {
            show_main(app);
        }
        handle_args(app, &args);
    }));
    #[cfg(mobile)]
    let builder = builder.plugin(mobile::init());
    let app = builder
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            // The main window is built here (not from the config) to attach the link handler.
            let main = app.config().app.windows.iter().find(|w| w.label == "main").cloned().expect("main window in tauri.conf.json");
            WebviewWindowBuilder::from_config(&handle, &main)?.on_new_window(links_open_outside(handle.clone())).build()?;
            // REMAIL_DATA_DIR selects a separate profile (testing, portable
            // installs); such profiles keep credentials in a private file
            // instead of the system keyring.
            let custom_dir = std::env::var_os("REMAIL_DATA_DIR").map(std::path::PathBuf::from);
            let data_dir = match &custom_dir {
                Some(d) => d.clone(),
                None => app.path().app_data_dir()?,
            };
            std::fs::create_dir_all(&data_dir)?;
            #[cfg(desktop)]
            let secrets: Arc<dyn remail_core::secrets::SecretStore> = match custom_dir {
                Some(_) => Arc::new(remail_core::secrets::FileStore::new(data_dir.join("credentials.json"))),
                None => Arc::new(KeyringStore::new(data_dir.join("credentials.json"))),
            };
            // Phones: a private file sealed with a key from the Keystore/Keychain.
            #[cfg(mobile)]
            let secrets: Arc<dyn remail_core::secrets::SecretStore> = {
                let _ = custom_dir;
                let key = mobile::credentials_key(&handle)?;
                Arc::new(remail_core::secrets::FileStore::sealed(data_dir.join("credentials.sealed"), &key)?)
            };
            let tray_refresh = Arc::new(TrayRefresh::default());
            let sink = Arc::new(UiSink { app: handle.clone(), tray: tray_refresh.clone() });
            let cloud_sink = Arc::new(CloudUi { app: handle.clone(), tray: tray_refresh.clone(), phase: Mutex::new(None) });
            let device = remail_core::cloud::this_device(&handle.package_info().version.to_string());
            // Mail waits for the Remail sign-in (the gate): the engine starts locked
            // and the cloud connection opens it once the session and setup are ready.
            let (engine, cloud) = tauri::async_runtime::block_on(async move {
                let engine = Engine::start_locked(data_dir, secrets, sink)?;
                let cloud = Cloud::new(engine.clone(), cloud_sink, device, CloudOptions::default())?;
                cloud.start().await?;
                anyhow::Ok((engine, cloud))
            })?;
            app.manage(AppState { engine, cloud, compose_windows: Mutex::new(HashMap::new()), next_window: Mutex::new(0) });
            #[cfg(desktop)]
            if let Err(e) = build_tray(&handle) {
                tracing::warn!("system tray unavailable: {e}");
            }
            let engine = app.state::<AppState>().engine.clone();
            #[cfg(desktop)]
            tauri::async_runtime::spawn(tray_updater(handle.clone(), engine.clone(), tray_refresh.clone()));
            tray_refresh.request();
            let args: Vec<String> = std::env::args().collect();
            // `--background` (started by `remail --mcp` for an agent): stay in the tray.
            #[cfg(desktop)]
            if !args.iter().any(|a| a == "--background") || handle.tray_by_id("main").is_none() {
                show_main(&handle);
            }
            let early = app.state::<EarlyArgs>().0.lock().unwrap().take().unwrap_or_default();
            handle_args(&handle, &args);
            for url in early {
                handle_args(&handle, &[url]);
            }
            #[cfg(desktop)]
            if mail_ready(&handle) {
                tauri::async_runtime::spawn(async move { engine.apply_mcp_server().await });
            }
            #[cfg(mobile)]
            let _ = engine;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::Destroyed = event {
                let label = window.label().to_string();
                if label.starts_with("compose-") {
                    if let Some(state) = window.app_handle().try_state::<AppState>() {
                        state.compose_windows.lock().unwrap().retain(|_, l| *l != label);
                    }
                }
            }
            #[cfg(desktop)]
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    let close_to_tray = window
                        .app_handle()
                        .try_state::<AppState>()
                        .map(|s| s.engine.settings().close_to_tray)
                        .unwrap_or(false);
                    api.prevent_close();
                    if close_to_tray && window.app_handle().tray_by_id("main").is_some() {
                        let _ = window.hide();
                    } else {
                        quit(window.app_handle());
                    }
                }
            }
        })
        .invoke_handler(commands::handlers())
        .build(tauri::generate_context!())
        .expect("error while building Remail");

    app.run(move |app, event| {
        // Exits that didn't come through `quit` (e.g. session logout).
        if let RunEvent::ExitRequested { api, code, .. } = event {
            if code.is_some() || quitting.swap(true, Ordering::SeqCst) {
                return;
            }
            api.prevent_exit();
            quit(app);
        }
    });
}

#[cfg(desktop)]
/// `remail --mcp`: a stdio MCP server for agents that can't use HTTP. It
/// forwards to the running app's MCP endpoint, starting Remail in the
/// background first if needed.
pub fn run_mcp_bridge() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .init();
    let data_dir = remail_core::mcp::bridge::default_data_dir();
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("remail --mcp: {e}");
            std::process::exit(1);
        }
    };
    rt.block_on(async {
        if !endpoint_alive(&data_dir).await {
            launch_background();
            for _ in 0..40 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if endpoint_alive(&data_dir).await {
                    break;
                }
            }
        }
        if let Err(e) = remail_core::mcp::bridge::run_stdio_bridge_discovering(&data_dir).await {
            eprintln!("remail --mcp: {e:#}");
        }
    });
}

#[cfg(desktop)]
/// A running Remail answers at the endpoint it wrote (a crash leaves the file behind).
async fn endpoint_alive(data_dir: &std::path::Path) -> bool {
    let Ok(client) = reqwest_lite_client() else { return false };
    remail_core::mcp::bridge::discover_endpoint(&client, data_dir).await.is_some()
}

#[cfg(desktop)]
fn reqwest_lite_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder().timeout(Duration::from_secs(2)).build()
}

/// The path that starts Remail. An AppImage runs from a temporary mount
/// that changes on every launch, so use the AppImage file itself — but only
/// when we are that AppImage (the variables leak into child processes of
/// other AppImage apps).
pub fn launcher_path() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| "remail".into());
    if let (Some(appimage), Some(appdir)) = (std::env::var_os("APPIMAGE"), std::env::var_os("APPDIR")) {
        if !appimage.is_empty() && !appdir.is_empty() && exe.starts_with(&appdir) {
            return appimage.into();
        }
    }
    exe
}

#[cfg(desktop)]
fn launch_background() {
    let _ = std::process::Command::new(launcher_path())
        .arg("--background")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
