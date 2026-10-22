//! Phone-only native pieces, served by `RemailPlugin` (Kotlin, in
//! `gen/android/app/src/main/java/app/remail/mobile`): the credentials key
//! kept by the Android Keystore, and attachments saved to Downloads or
//! opened in another app.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{AppHandle, Manager, Runtime, Wry};

struct Native(PluginHandle<Wry>);

pub fn init() -> TauriPlugin<Wry> {
    Builder::new("remail-native")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            let handle = api.register_android_plugin("app.remail.mobile", "RemailPlugin")?;
            #[cfg(target_os = "ios")]
            let handle = {
                let _ = api;
                return Err("iOS is not built yet".into());
            };
            app.manage(Native(handle));
            Ok(())
        })
        .build()
}

fn native<R: Runtime>(app: &AppHandle<R>) -> anyhow::Result<PluginHandle<Wry>> {
    app.try_state::<Native>().map(|n| n.0.clone()).ok_or_else(|| anyhow::anyhow!("the native plugin isn't loaded"))
}

#[derive(Deserialize)]
struct KeyReply {
    key: String,
}

/// The 32-byte key that seals this device's credentials file. The Kotlin
/// side keeps it wrapped by a non-exportable Android Keystore key.
pub fn credentials_key<R: Runtime>(app: &AppHandle<R>) -> anyhow::Result<Vec<u8>> {
    let reply: KeyReply = native(app)?.run_mobile_plugin("credentialsKey", ()).map_err(|e| anyhow::anyhow!("the Keystore key is unavailable: {e}"))?;
    let key = base64::engine::general_purpose::STANDARD.decode(reply.key)?;
    anyhow::ensure!(key.len() == 32, "the Keystore returned a key of the wrong size");
    Ok(key)
}

#[derive(Serialize)]
struct FileArgs<'a> {
    path: &'a str,
    name: &'a str,
    mime: &'a str,
}

#[derive(Deserialize)]
struct SavedReply {
    location: String,
}

/// Copy a file to the phone's Downloads. Returns where it went, for the toast.
pub fn save_to_downloads<R: Runtime>(app: &AppHandle<R>, path: &str, name: &str, mime: &str) -> Result<String, String> {
    let reply: SavedReply = native(app).map_err(|e| e.to_string())?.run_mobile_plugin("saveToDownloads", FileArgs { path, name, mime }).map_err(|e| e.to_string())?;
    Ok(reply.location)
}

/// Open a file (in the app's cache) with whichever app handles its type.
pub fn open_file<R: Runtime>(app: &AppHandle<R>, path: &str, name: &str, mime: &str) -> Result<(), String> {
    native(app).map_err(|e| e.to_string())?.run_mobile_plugin::<serde_json::Value>("openFile", FileArgs { path, name, mime }).map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Serialize)]
struct BarsArgs<'a> {
    dark: bool,
    background: &'a str,
}

pub fn set_system_bars<R: Runtime>(app: &AppHandle<R>, dark: bool, background: &str) {
    if let Ok(n) = native(app) {
        let _ = n.run_mobile_plugin::<serde_json::Value>("setSystemBars", BarsArgs { dark, background });
    }
}

/// Close a text menu (Copy, Share…) left up with nothing selected.
pub fn dismiss_text_menu<R: Runtime>(app: &AppHandle<R>) {
    if let Ok(n) = native(app) {
        let _ = n.run_mobile_plugin::<serde_json::Value>("dismissTextMenu", ());
    }
}

#[derive(Serialize)]
struct RegisterListener {
    event: &'static str,
    handler: tauri::ipc::Channel<serde_json::Value>,
}

/// Run `handler` whenever one of Remail's notifications is tapped while the
/// app runs (`take_notification_tap` says which).
pub fn on_notification_tap<R: Runtime>(app: &AppHandle<R>, handler: impl Fn() + Send + Sync + 'static) -> anyhow::Result<()> {
    let channel = tauri::ipc::Channel::new(move |_| {
        handler();
        Ok(())
    });
    native(app)?
        .run_mobile_plugin::<()>("registerListener", RegisterListener { event: "notificationTap", handler: channel })
        .map_err(|e| anyhow::anyhow!("{e}"))
}

#[derive(Deserialize)]
struct TapReply {
    id: Option<i32>,
}

/// The id of the notification tapped last (also one that started the app), once.
pub fn take_notification_tap<R: Runtime>(app: &AppHandle<R>) -> Option<i32> {
    let reply: TapReply = native(app).ok()?.run_mobile_plugin("takeNotificationTap", ()).ok()?;
    reply.id
}
