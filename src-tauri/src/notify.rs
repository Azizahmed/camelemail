//! OS notifications, and what a click (or a tap on a phone) on one does: it
//! brings the main window forward and opens what the notification was about.
//!
//! Desktop: each notification carries its target in the `extra` payload
//! ("open") and the notification plugin reports clicks to [`init`]'s handler:
//! Linux (D-Bus "default" action), Windows and macOS (while the toast is on
//! screen). The target is parked in [`Pending`].
//!
//! Android: the plugin hands a tap to the app as the notification's id only
//! (not its payload), so targets are kept by id in a small file in the app's
//! data folder, which also covers a tap that starts the app again after
//! Android ended it. `RemailPlugin` keeps the tapped id until it's taken.
//!
//! Either way the main window hears `notification-open` and takes the target
//! with `take_notification_open` — also right after it starts, so a tap that
//! launched the app isn't lost.

use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

/// Where a click on a notification goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum OpenTarget {
    /// A conversation (new mail, a snoozed email that came back).
    Thread { account_id: String, thread_id: String },
    /// The inbox ("5 new emails").
    Inbox { account_id: String },
    /// The calendar on the day of this time (an event reminder).
    Calendar { start_ms: i64 },
}

/// Tells the main window a target is waiting.
const EVENT: &str = "notification-open";

/// Show a notification; a click on it opens `open` (or just the app).
pub fn show(app: &AppHandle, title: &str, body: &str, open: Option<&OpenTarget>) {
    let builder = app.notification().builder().title(title).body(body);
    #[cfg(desktop)]
    let builder = match open {
        Some(open) => builder.extra(desktop::EXTRA_KEY, open),
        None => builder,
    };
    // Android: the notification icon (res/drawable-*dpi/ic_stat_remail, the
    // colour stamp logo: phones that draw it as it is show the logo, stock
    // Android only its outline in `icon_color`; res/raw/keep.xml keeps it in
    // release builds), the expanded text shown when a notification is opened
    // up, an id the target is filed under, and gone once tapped (also when the
    // tap restarts the app).
    #[cfg(mobile)]
    let builder = {
        let id = phone::new_id();
        if let Some(open) = open {
            phone::remember(app, id, open);
        }
        builder.id(id).icon("ic_stat_remail").icon_color("#D39033").large_body(body).auto_cancel()
    };
    let _ = builder.show();
}

/// Start listening for clicks on notifications. Call once from setup, before
/// any notification is shown (earlier ones report nothing on desktop).
pub fn init(app: &AppHandle) {
    #[cfg(desktop)]
    desktop::init(app);
    #[cfg(mobile)]
    {
        use tauri::Emitter;
        let handle = app.clone();
        if let Err(e) = crate::mobile::on_notification_tap(app, move || {
            let _ = handle.emit_to("main", EVENT, ());
        }) {
            tracing::warn!("taps on notifications won't open anything: {e}");
        }
    }
}

/// The target of the last click, once.
#[cfg(desktop)]
pub fn take_pending(app: &AppHandle) -> Option<OpenTarget> {
    desktop::take(app)
}

/// The target of the last tap, once.
#[cfg(mobile)]
pub fn take_pending(app: &AppHandle) -> Option<OpenTarget> {
    phone::take(app, crate::mobile::take_notification_tap(app)?)
}

#[cfg(desktop)]
mod desktop {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use tauri::{AppHandle, Emitter, Manager};
    use tauri_plugin_notification::{ActionPerformed, NotificationExt};

    use super::{OpenTarget, EVENT};

    /// The key of the target in a notification's `extra` payload.
    pub const EXTRA_KEY: &str = "open";
    /// A target nobody took by then (e.g. clicked while signed out) is dropped.
    const KEEP_FOR: Duration = Duration::from_secs(120);

    /// The target of the last click, until the page takes it.
    #[derive(Default)]
    struct Pending(Mutex<Option<(OpenTarget, Instant)>>);

    pub fn init(app: &AppHandle) {
        app.manage(Pending::default());
        #[cfg(target_os = "linux")]
        super::linux::listen();
        let handle = app.clone();
        if let Err(e) = app.notification().on_action(move |action| on_action(&handle, action)) {
            tracing::warn!("clicks on notifications won't open anything: {e}");
        }
    }

    /// The target a notification carries, if any.
    pub fn target_of(action: &ActionPerformed) -> Option<OpenTarget> {
        let value = action.notification()?.extra().get(EXTRA_KEY)?;
        serde_json::from_value(value.clone()).ok()
    }

    fn on_action(app: &AppHandle, action: &ActionPerformed) {
        // `tap` is a click on the notification itself; Remail adds no buttons.
        if action.action_id() != "tap" {
            return;
        }
        let target = target_of(action);
        if let (Some(target), Some(pending)) = (&target, app.try_state::<Pending>()) {
            *pending.0.lock().unwrap() = Some((target.clone(), Instant::now()));
        }
        // A notification with a click action isn't opened by the desktop
        // itself (GNOME leaves that to the app): every click brings it up.
        super::bring_forward(app);
        if target.is_some() {
            let _ = app.emit_to("main", EVENT, ());
        }
    }

    pub fn take(app: &AppHandle) -> Option<OpenTarget> {
        let pending = app.try_state::<Pending>()?;
        let (target, at) = pending.0.lock().unwrap().take()?;
        (at.elapsed() < KEEP_FOR).then_some(target)
    }
}

/// Android: targets filed by notification id.
#[cfg(mobile)]
mod phone {
    use std::hash::{BuildHasher, Hasher};
    use std::path::PathBuf;
    use std::sync::Mutex;

    use tauri::{AppHandle, Manager};

    use super::OpenTarget;

    /// The newest targets kept; older notifications open just the app.
    const KEEP: usize = 64;
    /// One reader/writer of the file at a time.
    static FILE: Mutex<()> = Mutex::new(());

    pub fn new_id() -> i32 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        // Positive and never the plugin's "no notification" (Int.MIN_VALUE).
        (h.finish() & 0x7fff_ffff) as i32
    }

    fn path(app: &AppHandle) -> Option<PathBuf> {
        Some(app.path().app_data_dir().ok()?.join("notification-targets.json"))
    }

    fn load(path: &PathBuf) -> Vec<(i32, OpenTarget)> {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn remember(app: &AppHandle, id: i32, target: &OpenTarget) {
        let Some(path) = path(app) else { return };
        let _lock = FILE.lock().unwrap();
        let mut all = load(&path);
        all.push((id, target.clone()));
        let drop = all.len().saturating_sub(KEEP);
        all.drain(..drop);
        if let Ok(json) = serde_json::to_vec(&all) {
            let _ = std::fs::write(&path, json);
        }
    }

    pub fn take(app: &AppHandle, id: i32) -> Option<OpenTarget> {
        let path = path(app)?;
        let _lock = FILE.lock().unwrap();
        let mut all = load(&path);
        let i = all.iter().position(|(n, _)| *n == id)?;
        let (_, target) = all.remove(i);
        if let Ok(json) = serde_json::to_vec(&all) {
            let _ = std::fs::write(&path, json);
        }
        Some(target)
    }
}

/// Show, unminimize and focus the main window (it may be hidden in the tray).
#[cfg(desktop)]
fn bring_forward(app: &AppHandle) {
    #[cfg(target_os = "linux")]
    {
        // Off the thread that reports the plugin's signals: the activation
        // token may still be on its way.
        let app = app.clone();
        let _ = std::thread::Builder::new().name("notification-focus".into()).spawn(move || {
            let token = linux::recent_token();
            let main = app.clone();
            let _ = app.run_on_main_thread(move || linux::raise(&main, token));
        });
    }
    #[cfg(not(target_os = "linux"))]
    crate::show_main(app);
}

/// Linux: desktops only let a window take the focus for a reason. Wayland
/// wants the XDG activation token the notification server sends with the
/// click (`ActivationToken`, Desktop Notifications Specification 1.2; GNOME
/// Shell sends it right before `ActionInvoked`) — without it GNOME shows
/// "Camel Mail is ready" instead. X11 wants a current timestamp.
#[cfg(target_os = "linux")]
mod linux {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use gtk::prelude::*;
    use tauri::{AppHandle, Manager};

    /// The last token and when it came.
    static LAST: Mutex<Option<(String, Instant)>> = Mutex::new(None);

    /// Collect tokens on a thread of its own for as long as the app runs.
    pub fn listen() {
        let started = std::thread::Builder::new().name("notification-tokens".into()).spawn(|| {
            let run = || -> zbus::Result<()> {
                let connection = zbus::blocking::Connection::session()?;
                let rule = zbus::MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .path("/org/freedesktop/Notifications")?
                    .interface("org.freedesktop.Notifications")?
                    .member("ActivationToken")?
                    .build();
                for signal in zbus::blocking::MessageIterator::for_match_rule(rule, &connection, None)?.flatten() {
                    if let Ok((_, token)) = signal.body().deserialize::<(u32, String)>() {
                        *LAST.lock().unwrap() = Some((token, Instant::now()));
                    }
                }
                Ok(())
            };
            if let Err(e) = run() {
                tracing::debug!("no notification activation tokens: {e}");
            }
        });
        if let Err(e) = started {
            tracing::debug!("no notification activation tokens: {e}");
        }
    }

    /// The token that came with the click being handled: one from the last
    /// two seconds, waiting up to a quarter second for it to arrive.
    pub fn recent_token() -> Option<String> {
        for _ in 0..10 {
            if let Some((token, at)) = LAST.lock().unwrap().take() {
                if at.elapsed() < Duration::from_secs(2) {
                    return Some(token);
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        None
    }

    /// Show and focus the main window. Main thread.
    pub fn raise(app: &AppHandle, token: Option<String>) {
        let Some(window) = app.get_webview_window("main") else { return };
        let Ok(gtk_window) = window.gtk_window() else { return crate::show_main(app) };
        let display = gtk_window.upcast_ref::<gtk::Widget>().display();
        match display.type_().name() {
            "GdkWaylandDisplay" => {
                if let Some(token) = token.and_then(|t| std::ffi::CString::new(t).ok()) {
                    // GTK activates the window with it when it's shown or
                    // focused next. SAFETY: a live GdkWaylandDisplay (checked
                    // above); GDK copies the string.
                    unsafe { gdk_wayland_sys::gdk_wayland_display_set_startup_notification_id(display.as_ptr() as *mut _, token.as_ptr()) };
                }
                crate::show_main(app);
            }
            "GdkX11Display" => {
                // GTK stamps a focus request with the app's last input, which
                // X ignores once another window took the focus after it.
                let _ = window.unminimize();
                let now = gtk_window
                    .window()
                    // SAFETY: the realized GdkWindow of a window on an X11 display.
                    .map(|w| unsafe { gdk_x11_sys::gdk_x11_get_server_time(w.as_ptr() as *mut _) })
                    .unwrap_or(0);
                // Also shows it when it's hidden.
                gtk_window.present_with_time(now);
            }
            _ => crate::show_main(app),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_travel_as_camel_case_json() {
        let t = OpenTarget::Thread { account_id: "a1".into(), thread_id: "t9".into() };
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v, serde_json::json!({"kind": "thread", "accountId": "a1", "threadId": "t9"}));
        assert_eq!(serde_json::from_value::<OpenTarget>(v).unwrap(), t);
        let c = serde_json::to_value(OpenTarget::Calendar { start_ms: 5 }).unwrap();
        assert_eq!(c, serde_json::json!({"kind": "calendar", "startMs": 5}));
    }

    #[cfg(desktop)]
    #[test]
    fn a_click_carries_its_target() {
        use tauri_plugin_notification::ActionPerformed;
        // As the plugin reports a click on Linux: the notification's data.
        let performed: ActionPerformed = serde_json::from_value(serde_json::json!({
            "actionId": "tap",
            "inputValue": null,
            "notification": {
                "id": 7, "title": "Ann", "body": "Hi",
                "extra": {"open": {"kind": "inbox", "accountId": "a1"}},
            },
        }))
        .unwrap();
        assert_eq!(desktop::target_of(&performed), Some(OpenTarget::Inbox { account_id: "a1".into() }));
    }
}
