//! Tauri commands: a thin layer over [`remail_core::Engine`]. Errors are
//! returned to the UI as user-readable strings.

use std::collections::HashMap;
use std::path::PathBuf;

use base64::Engine as _;
use std::sync::Arc;

use remail_core::ai::AiSettings;
use remail_core::assistant::ChatEvent;
use remail_core::engine::{
    friendly_error, AiOutput, AiTask, AppSettings, AttachmentData, McpSettings, McpStatus, ThreadAction, ThreadRef, UnsubscribeResult,
};
use remail_core::mcp::client::McpTransport;
use remail_core::mcp::import::ImportedServer;
use remail_core::mcp::{ServerInfo, ToolInfo};
use remail_core::store::{SenderRule, SmartCounts};
use remail_core::presets::{ProviderPreset, TimePreset};
use remail_core::*;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::AppState;

type R<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    friendly_error(&e)
}

/// Run store, keyring or file work on the blocking pool. Sync commands run on
/// the main thread, and the store's connection is shared with the sync
/// workers, so a command waiting on it there would freeze every window.
async fn blocking<T: Send + 'static>(state: &State<'_, AppState>, f: impl FnOnce(&Engine) -> anyhow::Result<T> + Send + 'static) -> R<T> {
    let engine = state.engine.clone();
    tauri::async_runtime::spawn_blocking(move || f(&engine)).await.map_err(|e| e.to_string())?.map_err(err)
}

macro_rules! handlers {
    ($($name:ident),* $(,)?) => {
        pub fn handlers() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
            tauri::generate_handler![$($name),*]
        }
    };
}

handlers![
    app_info,
    get_settings,
    set_settings,
    provider_presets,
    guess_servers,
    add_account,
    add_microsoft_account,
    add_google_account,
    reauthorize_account,
    cancel_sign_in,
    microsoft_sign_in_ready,
    update_account,
    remove_account,
    accounts,
    update_account_prefs,
    all_labels,
    unread_counts,
    list_threads,
    load_more,
    server_search,
    get_thread,
    message_with_images,
    save_attachment,
    open_attachment,
    modify_threads,
    set_message_flags,
    empty_folder,
    create_folder,
    snooze_presets,
    send_later_presets,
    snooze,
    unsnooze,
    new_draft,
    reply_draft,
    edit_draft_message,
    get_draft,
    save_draft,
    delete_draft,
    send,
    undo_send,
    outbox,
    reschedule,
    send_now,
    signatures,
    save_signature,
    delete_signature,
    templates,
    save_template,
    delete_template,
    newsletter_senders,
    unsubscribe,
    suggest_contacts,
    sync_now,
    open_compose,
    open_settings,
    cal_accounts,
    cal_calendars,
    cal_connect,
    cal_subscribe,
    cal_remove_account,
    cal_set_calendar,
    cal_sync,
    cal_events,
    cal_search,
    cal_create,
    cal_update,
    cal_delete,
    weather_forecast,
    weather_places,
    open_url,
    pick_attachments,
    read_attachments,
    smart_counts,
    set_sender_category,
    set_viewing,
    autoclean_preview,
    autoclean_undo,
    sender_rules,
    ai_settings,
    set_ai_settings,
    ai_models,
    ai_test,
    ai_run,
    ai_cancel,
    ai_chat,
    ai_chat_reset,
    ai_approve,
    mcp_settings,
    set_mcp_settings,
    mcp_status,
    mcp_regenerate_token,
    mcp_test,
    mcp_discover,
    mcp_parse,
    cloud_status,
    cloud_set_server,
    cloud_signup,
    cloud_verify_email,
    cloud_login,
    cloud_login_2fa,
    cloud_logout,
    cloud_request_reset,
    cloud_reset_password,
    cloud_me,
    cloud_update_me,
    cloud_change_password,
    cloud_totp_setup,
    cloud_totp_enable,
    cloud_totp_disable,
    cloud_devices,
    cloud_revoke_device,
    cloud_providers,
    cloud_migration,
    cloud_migrate,
    cloud_sync_now,
    ui_prefs,
    set_ui_prefs,
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    version: String,
    data_dir: String,
}

#[tauri::command]
fn app_info(app: AppHandle) -> AppInfo {
    use tauri::Manager;
    AppInfo {
        version: app.package_info().version.to_string(),
        data_dir: app.path().app_data_dir().map(|p| p.display().to_string()).unwrap_or_default(),
    }
}

#[tauri::command]
async fn get_settings(state: State<'_, AppState>) -> R<AppSettings> {
    blocking(&state, |e| Ok(e.settings())).await
}

#[tauri::command]
async fn set_settings(app: AppHandle, state: State<'_, AppState>, settings: AppSettings) -> R<AppSettings> {
    let saved = blocking(&state, move |e| e.set_settings(&settings)).await?;
    let _ = app.emit("settings-changed", &saved);
    Ok(saved)
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

#[tauri::command]
fn provider_presets(state: State<'_, AppState>) -> Vec<ProviderPreset> {
    state.engine.provider_presets()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerGuess {
    imap: ServerConfig,
    smtp: ServerConfig,
}

#[tauri::command]
fn guess_servers(state: State<'_, AppState>, email: String) -> ServerGuess {
    let (imap, smtp) = state.engine.guess_servers(&email);
    ServerGuess { imap, smtp }
}

#[tauri::command]
async fn add_account(state: State<'_, AppState>, account: NewAccount) -> R<Account> {
    state.engine.add_account(account).await.map_err(err)
}

/// Opens the sign-in page in the user's browser.
fn browser(app: AppHandle) -> impl Fn(&str) -> anyhow::Result<()> {
    move |url: &str| app.opener().open_url(url, None::<&str>).map_err(|e| anyhow::anyhow!("couldn't open the browser: {e}"))
}

#[tauri::command]
async fn add_microsoft_account(app: AppHandle, state: State<'_, AppState>, req: OAuthAccountRequest) -> R<OAuthAccountAdded> {
    state.engine.add_microsoft_account(browser(app), req).await.map_err(err)
}

/// Sign in at Google in the browser and add the Gmail / Workspace account.
#[tauri::command]
async fn add_google_account(app: AppHandle, state: State<'_, AppState>, display_name: Option<String>) -> R<OAuthAccountAdded> {
    let req = OAuthAccountRequest { display_name, ..Default::default() };
    state.engine.add_google_account(browser(app), req).await.map_err(err)
}

#[tauri::command]
async fn reauthorize_account(app: AppHandle, state: State<'_, AppState>, id: String) -> R<()> {
    state.engine.reauthorize_account(&id, browser(app)).await.map_err(err)
}

#[tauri::command]
fn cancel_sign_in(state: State<'_, AppState>) {
    state.engine.cancel_sign_in();
}

/// Whether "Sign in with Microsoft" has an app (client) ID to use (from the organisation).
#[tauri::command]
async fn microsoft_sign_in_ready(state: State<'_, AppState>) -> R<bool> {
    blocking(&state, |e| Ok(e.microsoft_client_id().is_some())).await
}

#[tauri::command]
async fn update_account(state: State<'_, AppState>, id: String, account: NewAccount) -> R<()> {
    state.engine.update_account(&id, account).await.map_err(err)
}

#[tauri::command]
async fn remove_account(state: State<'_, AppState>, id: String) -> R<()> {
    blocking(&state, move |e| e.remove_account(&id)).await
}

#[tauri::command]
async fn accounts(state: State<'_, AppState>) -> R<Vec<Account>> {
    blocking(&state, move |e| e.accounts()).await
}

#[tauri::command]
async fn update_account_prefs(
    state: State<'_, AppState>,
    id: String,
    display_name: Option<String>,
    color: Option<String>,
    hidden_in_unified: Option<bool>,
    auto_bcc: Option<Vec<String>>,
) -> R<()> {
    blocking(&state, move |e| e.update_account_prefs(&id, display_name, color, hidden_in_unified, auto_bcc)).await
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

#[tauri::command]
async fn all_labels(state: State<'_, AppState>) -> R<HashMap<String, Vec<Label>>> {
    blocking(&state, |e| {
        let mut out = HashMap::new();
        for a in e.accounts()? {
            out.insert(a.id.clone(), e.labels(&a.id)?);
        }
        Ok(out)
    })
    .await
}

#[tauri::command]
async fn unread_counts(state: State<'_, AppState>) -> R<HashMap<String, i64>> {
    blocking(&state, move |e| e.unread_counts()).await
}

#[tauri::command]
async fn list_threads(state: State<'_, AppState>, query: ThreadQuery) -> R<Vec<ThreadSummary>> {
    blocking(&state, move |e| e.list_threads(&query)).await
}

#[tauri::command]
async fn load_more(state: State<'_, AppState>, account_id: Option<String>, label: String) -> R<bool> {
    state.engine.load_more(account_id.as_deref(), &label).await.map_err(err)
}

#[tauri::command]
async fn server_search(state: State<'_, AppState>, account_id: Option<String>, query: String) -> R<usize> {
    state.engine.server_search(account_id.as_deref(), &query).await.map_err(err)
}

#[tauri::command]
async fn get_thread(state: State<'_, AppState>, account_id: String, thread_id: String) -> R<ThreadDetail> {
    state.engine.get_thread(&account_id, &thread_id).await.map_err(err)
}

#[tauri::command]
async fn message_with_images(state: State<'_, AppState>, message_id: String) -> R<MessageFull> {
    state.engine.message_with_images(&message_id).await.map_err(err)
}

fn safe_filename(name: &str) -> String {
    let cleaned: String =
        name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '\0') { '_' } else { c }).collect();
    let trimmed = cleaned.trim().trim_start_matches('.');
    if trimmed.is_empty() {
        "attachment".into()
    } else {
        trimmed.to_string()
    }
}

/// Ask where to save an attachment and write it. Returns the saved path.
#[tauri::command]
async fn save_attachment(app: AppHandle, state: State<'_, AppState>, message_id: String, index: usize) -> R<Option<String>> {
    let att: AttachmentData = state.engine.attachment(&message_id, index).await.map_err(err)?;
    let name = safe_filename(&att.filename);
    let picked = tauri::async_runtime::spawn_blocking(move || app.dialog().file().set_file_name(&name).blocking_save_file())
        .await
        .map_err(|e| e.to_string())?;
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else { return Ok(None) };
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = base64::engine::general_purpose::STANDARD.decode(att.data_base64).map_err(|e| e.to_string())?;
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        Ok(Some(path.display().to_string()))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Write an attachment to a private temp folder and open it with the default app.
#[tauri::command]
async fn open_attachment(app: AppHandle, state: State<'_, AppState>, message_id: String, index: usize) -> R<()> {
    let att = state.engine.attachment(&message_id, index).await.map_err(err)?;
    let path = tauri::async_runtime::spawn_blocking(move || -> R<PathBuf> {
        let bytes = base64::engine::general_purpose::STANDARD.decode(att.data_base64).map_err(|e| e.to_string())?;
        let dir: PathBuf = std::env::temp_dir().join(format!("remail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(safe_filename(&att.filename));
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        Ok(path)
    })
    .await
    .map_err(|e| e.to_string())??;
    app.opener().open_path(path.display().to_string(), None::<&str>).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

#[tauri::command]
async fn modify_threads(state: State<'_, AppState>, threads: Vec<ThreadRef>, action: ThreadAction) -> R<()> {
    blocking(&state, move |e| e.modify_threads(&threads, &action)).await
}

#[tauri::command]
async fn set_message_flags(state: State<'_, AppState>, message_id: String, unread: Option<bool>, starred: Option<bool>) -> R<()> {
    blocking(&state, move |e| e.set_message_flags(&message_id, unread, starred)).await
}

#[tauri::command]
async fn empty_folder(state: State<'_, AppState>, account_id: String, label: String) -> R<()> {
    blocking(&state, move |e| e.empty_folder(&account_id, &label)).await
}

#[tauri::command]
async fn create_folder(state: State<'_, AppState>, account_id: String, name: String) -> R<()> {
    state.engine.create_folder(&account_id, &name).await.map_err(err)
}

#[tauri::command]
async fn snooze_presets(state: State<'_, AppState>) -> R<Vec<TimePreset>> {
    blocking(&state, |e| Ok(e.snooze_presets())).await
}

#[tauri::command]
async fn send_later_presets(state: State<'_, AppState>) -> R<Vec<TimePreset>> {
    blocking(&state, |e| Ok(e.send_later_presets())).await
}

#[tauri::command]
async fn snooze(state: State<'_, AppState>, threads: Vec<ThreadRef>, until: i64) -> R<()> {
    blocking(&state, move |e| e.snooze(&threads, until)).await
}

#[tauri::command]
async fn unsnooze(state: State<'_, AppState>, threads: Vec<ThreadRef>) -> R<()> {
    blocking(&state, move |e| e.unsnooze(&threads)).await
}

// ---------------------------------------------------------------------------
// Compose
// ---------------------------------------------------------------------------

#[tauri::command]
async fn new_draft(state: State<'_, AppState>, account_id: Option<String>, mailto: Option<String>) -> R<Draft> {
    let d = blocking(&state, move |e| match mailto {
        Some(url) => e.mailto_draft(&url, account_id.as_deref()),
        None => e.new_draft(account_id.as_deref()),
    })
    .await?;
    state.engine.save_draft(d, false).await.map_err(err)
}

#[tauri::command]
async fn reply_draft(state: State<'_, AppState>, message_id: String, kind: ReplyKind) -> R<Draft> {
    let d = state.engine.reply_draft(&message_id, kind).await.map_err(err)?;
    state.engine.save_draft(d, false).await.map_err(err)
}

#[tauri::command]
async fn edit_draft_message(state: State<'_, AppState>, message_id: String) -> R<Draft> {
    state.engine.edit_draft_message(&message_id).await.map_err(err)
}

#[tauri::command]
async fn get_draft(state: State<'_, AppState>, id: String) -> R<Option<Draft>> {
    blocking(&state, move |e| Ok(e.store().draft(&id)?.map(|(d, _)| d))).await
}

#[tauri::command]
async fn save_draft(state: State<'_, AppState>, draft: Draft, sync_server: bool) -> R<Draft> {
    state.engine.save_draft(draft, sync_server).await.map_err(err)
}

#[tauri::command]
async fn delete_draft(state: State<'_, AppState>, id: String) -> R<()> {
    blocking(&state, move |e| e.delete_draft(&id)).await
}

#[tauri::command]
async fn send(state: State<'_, AppState>, draft: Draft, send_at: Option<i64>) -> R<OutboxItem> {
    blocking(&state, move |e| e.send(draft, send_at)).await
}

#[tauri::command]
async fn undo_send(state: State<'_, AppState>, outbox_id: String) -> R<Option<Draft>> {
    blocking(&state, move |e| e.undo_send(&outbox_id)).await
}

#[tauri::command]
async fn outbox(state: State<'_, AppState>) -> R<Vec<OutboxItem>> {
    blocking(&state, move |e| e.outbox()).await
}

#[tauri::command]
async fn reschedule(state: State<'_, AppState>, outbox_id: String, send_at: i64) -> R<()> {
    blocking(&state, move |e| e.reschedule(&outbox_id, send_at)).await
}

#[tauri::command]
async fn send_now(state: State<'_, AppState>, outbox_id: String) -> R<()> {
    blocking(&state, move |e| e.send_now(&outbox_id)).await
}

#[tauri::command]
async fn signatures(state: State<'_, AppState>) -> R<Vec<Signature>> {
    blocking(&state, move |e| e.signatures()).await
}

#[tauri::command]
async fn save_signature(state: State<'_, AppState>, signature: Signature) -> R<Signature> {
    blocking(&state, move |e| e.save_signature(signature)).await
}

#[tauri::command]
async fn delete_signature(state: State<'_, AppState>, id: String) -> R<()> {
    blocking(&state, move |e| e.delete_signature(&id)).await
}

#[tauri::command]
async fn templates(state: State<'_, AppState>) -> R<Vec<Template>> {
    blocking(&state, move |e| e.templates()).await
}

#[tauri::command]
async fn save_template(state: State<'_, AppState>, template: Template) -> R<Template> {
    blocking(&state, move |e| e.save_template(template)).await
}

#[tauri::command]
async fn delete_template(state: State<'_, AppState>, id: String) -> R<()> {
    blocking(&state, move |e| e.delete_template(&id)).await
}

#[tauri::command]
async fn newsletter_senders(state: State<'_, AppState>, account_id: Option<String>) -> R<Vec<NewsletterSender>> {
    blocking(&state, move |e| e.newsletter_senders(account_id.as_deref())).await
}

#[tauri::command]
async fn unsubscribe(app: AppHandle, state: State<'_, AppState>, message_id: String) -> R<UnsubscribeResult> {
    let res = state.engine.unsubscribe(&message_id).await.map_err(err)?;
    if let Some(url) = &res.url {
        app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())?;
    }
    Ok(res)
}

#[tauri::command]
async fn suggest_contacts(state: State<'_, AppState>, prefix: String) -> R<Vec<Contact>> {
    blocking(&state, move |e| e.suggest_contacts(&prefix)).await
}

#[tauri::command]
fn sync_now(state: State<'_, AppState>, account_id: Option<String>) {
    state.engine.sync_now(account_id.as_deref());
}

// ---------------------------------------------------------------------------
// Windows & files
// ---------------------------------------------------------------------------

// Window-building commands are async: building a window from a sync command
// (on the main thread) deadlocks on Windows.
#[tauri::command]
async fn open_compose(app: AppHandle, draft_id: String) -> R<()> {
    crate::open_compose_window(&app, &draft_id).map_err(|e| e.to_string())
}


#[tauri::command]
async fn open_settings(app: AppHandle, section: Option<String>) -> R<()> {
    crate::open_settings_window(&app, section.as_deref()).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn open_url(app: AppHandle, state: State<'_, AppState>, url: String) -> R<()> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")) {
        return Err("Only web and mail links can be opened".into());
    }
    if lower.starts_with("mailto:") {
        let draft = blocking(&state, move |e| e.mailto_draft(&url, None)).await?;
        let engine = state.engine.clone();
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Ok(d) = engine.save_draft(draft, false).await {
                let _ = crate::open_compose_window(&app2, &d.id);
            }
        });
        return Ok(());
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

fn guess_mime(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" | "log" | "md" => "text/plain",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "ics" => "text/calendar",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "json" => "application/json",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

/// Native file picker → attachments ready for a draft (`images`: pictures only).
#[tauri::command]
async fn pick_attachments(app: AppHandle, images: Option<bool>) -> R<Vec<OutgoingAttachment>> {
    // The picker, the reads and the encoding all block: keep them off the async workers.
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app.dialog().file();
        if images == Some(true) {
            dialog = dialog.add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp"]);
        }
        let paths = dialog.blocking_pick_files().unwrap_or_default().into_iter().filter_map(|f| f.into_path().ok());
        read_files(paths)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Files dropped on a composer → attachments ready for a draft.
#[tauri::command]
async fn read_attachments(paths: Vec<String>) -> R<Vec<OutgoingAttachment>> {
    tauri::async_runtime::spawn_blocking(move || read_files(paths.into_iter().map(std::path::PathBuf::from)))
        .await
        .map_err(|e| e.to_string())?
}

fn read_files(paths: impl Iterator<Item = std::path::PathBuf>) -> R<Vec<OutgoingAttachment>> {
    let mut out = Vec::new();
    for path in paths {
        // Dropped folders aren't attachments.
        if path.is_dir() {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if bytes.len() > remail_core::compose::MAX_ATTACHMENT_TOTAL_BYTES {
            return Err(format!("{} is larger than 25 MB", path.display()));
        }
        let filename = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
        out.push(OutgoingAttachment {
            mime_type: guess_mime(&filename).to_string(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            filename,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Smart Inbox
// ---------------------------------------------------------------------------

#[tauri::command]
async fn smart_counts(state: State<'_, AppState>, account_id: Option<String>) -> R<SmartCounts> {
    blocking(&state, move |e| e.smart_counts(account_id.as_deref())).await
}

#[tauri::command]
async fn set_sender_category(state: State<'_, AppState>, email: String, category: Option<String>) -> R<()> {
    blocking(&state, move |e| e.set_sender_category(&email, category.as_deref())).await
}

#[tauri::command]
fn set_viewing(state: State<'_, AppState>, thread: Option<ThreadRef>) {
    state.engine.set_viewing(thread);
}

#[tauri::command]
async fn autoclean_preview(state: State<'_, AppState>) -> R<usize> {
    blocking(&state, move |e| e.autoclean_preview()).await
}

#[tauri::command]
async fn autoclean_undo(state: State<'_, AppState>, threads: Vec<ThreadRef>, messages: Vec<String>) -> R<()> {
    blocking(&state, move |e| e.autoclean_undo(&threads, &messages)).await
}

#[tauri::command]
async fn sender_rules(state: State<'_, AppState>) -> R<Vec<SenderRule>> {
    blocking(&state, move |e| e.sender_rules()).await
}

// ---------------------------------------------------------------------------
// AI assistant
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AiSettingsView {
    settings: AiSettings,
    has_key: bool,
}

fn ai_view(e: &Engine) -> AiSettingsView {
    AiSettingsView { settings: e.ai_settings(), has_key: e.ai_has_key() }
}

#[tauri::command]
async fn ai_settings(state: State<'_, AppState>) -> R<AiSettingsView> {
    blocking(&state, |e| Ok(ai_view(e))).await
}

/// `api_key`: None keeps the saved key, "" removes it.
#[tauri::command]
async fn set_ai_settings(app: AppHandle, state: State<'_, AppState>, settings: AiSettings, api_key: Option<String>) -> R<AiSettingsView> {
    let view = blocking(&state, move |e| {
        e.set_ai_settings(&settings, api_key.as_deref())?;
        Ok(ai_view(e))
    })
    .await?;
    let _ = app.emit("ai-settings-changed", &view);
    Ok(view)
}

#[tauri::command]
async fn ai_models(state: State<'_, AppState>, settings: AiSettings, api_key: Option<String>) -> R<Vec<String>> {
    state.engine.ai_models(settings, api_key).await.map_err(err)
}

#[tauri::command]
async fn ai_test(state: State<'_, AppState>, settings: AiSettings, api_key: Option<String>) -> R<String> {
    state.engine.ai_test(settings, api_key).await.map_err(err)
}

/// Run a writing task; text streams through `on_text`.
#[tauri::command]
async fn ai_run(state: State<'_, AppState>, request_id: String, task: AiTask, on_text: Channel<String>) -> R<AiOutput> {
    let mut cb = move |t: &str| {
        let _ = on_text.send(t.to_string());
    };
    state.engine.ai_run(&request_id, task, &mut cb).await.map_err(err)
}

#[tauri::command]
fn ai_cancel(state: State<'_, AppState>, request_id: String) {
    state.engine.ai_cancel(&request_id);
}

#[tauri::command]
async fn ai_chat(
    state: State<'_, AppState>,
    conversation_id: String,
    request_id: String,
    text: String,
    context: Option<ThreadRef>,
    on_event: Channel<ChatEvent>,
) -> R<()> {
    let emit = Arc::new(move |e: ChatEvent| {
        let _ = on_event.send(e);
    });
    state.engine.ai_chat(&conversation_id, &request_id, text, context, emit).await.map_err(err)
}

#[tauri::command]
fn ai_chat_reset(state: State<'_, AppState>, conversation_id: String) {
    state.engine.ai_chat_reset(&conversation_id);
}

#[tauri::command]
fn ai_approve(state: State<'_, AppState>, call_id: String, allow: bool) {
    state.engine.ai_approve(&call_id, allow);
}

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatusView {
    #[serde(flatten)]
    status: McpStatus,
    /// This executable, for stdio configs (`remail --mcp`).
    command: String,
}

fn status_view(status: McpStatus) -> McpStatusView {
    McpStatusView { status, command: crate::launcher_path().display().to_string() }
}

#[tauri::command]
async fn mcp_settings(state: State<'_, AppState>) -> R<McpSettings> {
    blocking(&state, |e| Ok(e.mcp_settings())).await
}

#[tauri::command]
async fn set_mcp_settings(state: State<'_, AppState>, settings: McpSettings) -> R<McpStatusView> {
    state.engine.set_mcp_settings(settings).await.map(status_view).map_err(err)
}

#[tauri::command]
async fn mcp_status(state: State<'_, AppState>) -> R<McpStatusView> {
    blocking(&state, |e| Ok(status_view(e.mcp_status()))).await
}

#[tauri::command]
async fn mcp_regenerate_token(state: State<'_, AppState>) -> R<McpStatusView> {
    state.engine.regenerate_mcp_token().await.map(status_view).map_err(err)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTestResult {
    server: ServerInfo,
    tools: Vec<ToolInfo>,
}

#[tauri::command]
async fn mcp_test(state: State<'_, AppState>, transport: McpTransport) -> R<McpTestResult> {
    let (server, tools) = state.engine.mcp_test(&transport).await.map_err(err)?;
    Ok(McpTestResult { server, tools })
}

#[tauri::command]
async fn mcp_discover(state: State<'_, AppState>) -> R<Vec<ImportedServer>> {
    Ok(state.engine.mcp_discover().await)
}

#[tauri::command]
fn mcp_parse(state: State<'_, AppState>, text: String) -> R<Vec<ImportedServer>> {
    state.engine.mcp_parse(&text).map_err(err)
}

// ---------------------------------------------------------------------------
// Calendar
// ---------------------------------------------------------------------------

use remail_core::caldav::event::{EditScope, EventInput};
use remail_core::calendar::{CalAccountView, CalConnect, CalEvent, CalSubscribe, CalendarView, DayWeather, Place};

#[tauri::command]
async fn cal_accounts(state: State<'_, AppState>) -> R<Vec<CalAccountView>> {
    blocking(&state, move |e| e.cal_accounts()).await
}

#[tauri::command]
async fn cal_calendars(state: State<'_, AppState>) -> R<Vec<CalendarView>> {
    blocking(&state, move |e| e.cal_calendars()).await
}

#[tauri::command]
async fn cal_connect(state: State<'_, AppState>, req: CalConnect) -> R<CalAccountView> {
    state.engine.cal_connect(req).await.map_err(err)
}

#[tauri::command]
async fn cal_subscribe(state: State<'_, AppState>, req: CalSubscribe) -> R<CalAccountView> {
    state.engine.cal_subscribe(req).await.map_err(err)
}

#[tauri::command]
async fn cal_remove_account(state: State<'_, AppState>, id: String) -> R<()> {
    blocking(&state, move |e| e.cal_remove_account(&id)).await
}

#[tauri::command]
async fn cal_set_calendar(state: State<'_, AppState>, id: String, enabled: Option<bool>, color: Option<String>) -> R<()> {
    blocking(&state, move |e| e.cal_set_calendar(&id, enabled, color)).await
}

#[tauri::command]
async fn cal_sync(state: State<'_, AppState>, account_id: Option<String>) -> R<()> {
    state.engine.cal_sync(account_id.as_deref()).await.map_err(err)
}

#[tauri::command]
async fn cal_events(state: State<'_, AppState>, from_ms: i64, to_ms: i64) -> R<Vec<CalEvent>> {
    blocking(&state, move |e| e.cal_events(from_ms, to_ms)).await
}

#[tauri::command]
async fn cal_search(state: State<'_, AppState>, query: String) -> R<Vec<CalEvent>> {
    blocking(&state, move |e| e.cal_search(&query)).await
}

#[tauri::command]
async fn cal_create(state: State<'_, AppState>, calendar_id: String, input: EventInput) -> R<()> {
    state.engine.cal_create(&calendar_id, input).await.map_err(err)
}

#[tauri::command]
async fn cal_update(
    state: State<'_, AppState>,
    calendar_id: String,
    href: String,
    input: EventInput,
    scope: EditScope,
    new_calendar_id: Option<String>,
    occurrence_start: Option<i64>,
) -> R<()> {
    state.engine.cal_update(&calendar_id, &href, input, scope, new_calendar_id, occurrence_start).await.map_err(err)
}

#[tauri::command]
async fn cal_delete(state: State<'_, AppState>, calendar_id: String, href: String, scope: EditScope) -> R<()> {
    state.engine.cal_delete(&calendar_id, &href, scope).await.map_err(err)
}

#[tauri::command]
async fn weather_forecast(state: State<'_, AppState>) -> R<Vec<DayWeather>> {
    state.engine.weather_forecast().await.map_err(err)
}

#[tauri::command]
async fn weather_places(state: State<'_, AppState>, query: String) -> R<Vec<Place>> {
    state.engine.weather_places(&query).await.map_err(err)
}

// ---------------------------------------------------------------------------
// Remail account (the server). Tokens never leave Rust; errors are
// `cloud:<code>:<message>`.
// ---------------------------------------------------------------------------

use remail_core::cloud::{CloudError, CloudStatus, LoginResult, MigrationInfo, ProviderView};
use remail_core::engine::{UiPrefs, UiPrefsPatch};
use remail_core::proto::api::{BackupCodes, Device, Me, Meta, SignupRequest, SignupResponse, TotpSetup};

fn cerr(e: CloudError) -> String {
    e.to_string()
}

#[tauri::command]
fn cloud_status(state: State<'_, AppState>) -> CloudStatus {
    state.cloud.status()
}

#[tauri::command]
async fn cloud_set_server(state: State<'_, AppState>, address: String) -> R<Meta> {
    state.cloud.set_server(&address).await.map_err(cerr)
}

#[tauri::command]
async fn cloud_signup(
    state: State<'_, AppState>,
    name: String,
    email: String,
    password: String,
    invite: Option<String>,
    locale: Option<String>,
) -> R<SignupResponse> {
    let req = SignupRequest { name, email, password, invite: invite.filter(|i| !i.trim().is_empty()), locale };
    state.cloud.signup(req).await.map_err(cerr)
}

#[tauri::command]
async fn cloud_verify_email(state: State<'_, AppState>, token: String) -> R<()> {
    state.cloud.verify_email(&token).await.map_err(cerr)
}

#[tauri::command]
async fn cloud_login(state: State<'_, AppState>, email: String, password: String) -> R<LoginResult> {
    state.cloud.login(&email, &password).await.map_err(cerr)
}

#[derive(Serialize)]
pub struct MeResult {
    me: Me,
}

#[tauri::command]
async fn cloud_login_2fa(state: State<'_, AppState>, challenge: String, code: String) -> R<MeResult> {
    state.cloud.login_2fa(&challenge, &code).await.map(|me| MeResult { me }).map_err(cerr)
}

/// Sign out: this computer's synced setup and credentials are removed and mail stops.
#[tauri::command]
async fn cloud_logout(state: State<'_, AppState>) -> R<()> {
    state.cloud.logout().await.map_err(cerr)
}

#[tauri::command]
async fn cloud_request_reset(state: State<'_, AppState>, email: String) -> R<()> {
    state.cloud.request_reset(&email).await.map_err(cerr)
}

#[tauri::command]
async fn cloud_reset_password(state: State<'_, AppState>, token: String, password: String) -> R<()> {
    state.cloud.reset_password(&token, &password).await.map_err(cerr)
}

#[tauri::command]
async fn cloud_me(state: State<'_, AppState>) -> R<Me> {
    state.cloud.me().await.map_err(cerr)
}

#[tauri::command]
async fn cloud_update_me(state: State<'_, AppState>, name: String) -> R<Me> {
    state.cloud.update_me(&name).await.map_err(cerr)
}

#[tauri::command]
async fn cloud_change_password(state: State<'_, AppState>, current: String, new: String) -> R<()> {
    state.cloud.change_password(&current, &new).await.map_err(cerr)
}

#[tauri::command]
async fn cloud_totp_setup(state: State<'_, AppState>, challenge: Option<String>) -> R<TotpSetup> {
    state.cloud.totp_setup(challenge).await.map_err(cerr)
}

#[tauri::command]
async fn cloud_totp_enable(state: State<'_, AppState>, code: String, challenge: Option<String>) -> R<BackupCodes> {
    state.cloud.totp_enable(&code, challenge).await.map_err(cerr)
}

#[tauri::command]
async fn cloud_totp_disable(state: State<'_, AppState>, password: String, code: String) -> R<()> {
    state.cloud.totp_disable(&password, &code).await.map_err(cerr)
}

#[tauri::command]
async fn cloud_devices(state: State<'_, AppState>) -> R<Vec<Device>> {
    state.cloud.devices().await.map_err(cerr)
}

#[tauri::command]
async fn cloud_revoke_device(state: State<'_, AppState>, id: String) -> R<()> {
    state.cloud.revoke_device(&id).await.map_err(cerr)
}

#[tauri::command]
async fn cloud_providers(state: State<'_, AppState>) -> R<Vec<ProviderView>> {
    state.cloud.providers().await.map_err(cerr)
}

#[tauri::command]
async fn cloud_migration(state: State<'_, AppState>) -> R<MigrationInfo> {
    state.cloud.migration().await.map_err(cerr)
}

#[tauri::command]
async fn cloud_migrate(state: State<'_, AppState>, upload: bool) -> R<()> {
    state.cloud.migrate(upload).await.map_err(cerr)
}

#[tauri::command]
async fn cloud_sync_now(state: State<'_, AppState>) -> R<()> {
    state.cloud.sync_now().await.map_err(cerr)
}

/// Shortcuts, hidden sidebar entries and the last compose account (synced).
#[tauri::command]
async fn ui_prefs(state: State<'_, AppState>) -> R<UiPrefs> {
    blocking(&state, |e| Ok(e.ui_prefs())).await
}

#[tauri::command]
async fn set_ui_prefs(state: State<'_, AppState>, prefs: UiPrefsPatch) -> R<()> {
    blocking(&state, move |e| e.set_ui_prefs(prefs).map(|_| ())).await
}
