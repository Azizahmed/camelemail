//! Sections ⇄ the local store.
//!
//! `collect` reads a section's records from the tables and settings the
//! engine already uses; `stamp` folds them into the section document, giving
//! a new stamp only to what changed; `apply` writes a (merged) document back.
//! The engine keeps reading SQLite as before, so it never sees documents.
//!
//! Applying is idempotent and only touches what differs. Things that can't
//! be applied yet (a snooze whose mail hasn't synced, a signature whose
//! account hasn't arrived) are left for a later pass; they aren't deleted
//! from the document because this device never had them.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use remail_proto::sections::{self, account, cred};
use remail_proto::sync::{Record, SectionDoc, Stamp, SINGLETON};

use super::vault::{self, VaultKey};
use super::{CalOverride, CAL_OVERRIDES};
use crate::calendar::{SyncedCalAccount, OUTLOOK_OFF};
use crate::engine::{account_provider, AppSettings, CleanRule, Engine, UI_PREFS};
use crate::model::*;
use crate::store::{now_ms, AccountLook};

/// One record as this device has it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocalRecord {
    pub fields: BTreeMap<String, Value>,
    pub sets: BTreeMap<String, Vec<String>>,
    /// Fields the document has and this record doesn't are cleared (`null`):
    /// a shortcut set back to its default, a password replaced by a sign-in.
    pub exhaustive: bool,
}

/// A section's records on this device, by record id.
pub type Local = BTreeMap<String, LocalRecord>;

/// Calendar records besides accounts (`cal_…`) and overrides (`ovr:…`).
const CAL_PREFS: &str = "prefs";
const CAL_OUTLOOK: &str = "outlook";
const CAL_OUTLOOK_OFF: &str = "off";
const OVERRIDE_PREFIX: &str = "ovr:";

/// `AppSettings` keys that follow the user (settings registry §1); the rest
/// stay on the device.
const USER_PREFS: &[&str] = &[
    "undoSendSeconds",
    "theme",
    "loadRemoteImages",
    "defaultReply",
    "markReadOnOpen",
    "notifyOnSnoozeWake",
    "snoozeLaterMinutes",
    "snoozeMorning",
    "snoozeAfternoon",
    "snoozeEvening",
    "snoozeTomorrow",
    "snoozeDayAfter",
    "snoozeSaturday",
    "snoozeMonday",
    "confirmBeforeSend",
    "afterRemove",
    "smartInbox",
    "notifyImportantOnly",
    "language",
];

/// The order sections are applied in: credentials before the accounts that
/// use them, accounts before what refers to them.
pub fn apply_order(section: &str) -> u8 {
    match section {
        s if s.starts_with(sections::CRED_PREFIX) => 0,
        sections::ACCOUNTS => 1,
        sections::CALENDARS => 2,
        sections::SIGNATURES => 3,
        sections::TEMPLATES => 4,
        sections::SENDER_RULES => 5,
        sections::SNOOZES => 6,
        sections::PREFS => 7,
        sections::SHORTCUTS => 8,
        _ => 9,
    }
}

// ----------------------------------------------------------------------------
// Documents
// ----------------------------------------------------------------------------

/// Fold this device's records into `doc`: changed values get `at`, unchanged
/// ones keep their stamps (so re-saving a form doesn't win over another
/// device's edit), and records in `known` that are gone here are deleted.
pub fn stamp(doc: &mut SectionDoc, local: &Local, known: &BTreeSet<String>, at: &Stamp) {
    for (id, l) in local {
        if !doc.records.contains_key(id) && l.fields.is_empty() && l.sets.is_empty() {
            continue;
        }
        let rec = doc.record_mut(id);
        for (f, v) in &l.fields {
            rec.set(f, v.clone(), at);
        }
        for (f, items) in &l.sets {
            // (An empty set is the same as none: tombstone GC drops it.)
            if rec.set_items(f) != sorted(items) {
                rec.assign_set(f, items, at);
            }
        }
        if l.exhaustive {
            let stale: Vec<String> = rec.fields.iter().filter(|(k, v)| !l.fields.contains_key(*k) && !v.value.is_null()).map(|(k, _)| k.clone()).collect();
            for f in stale {
                rec.set(&f, Value::Null, at);
            }
        }
    }
    for id in known {
        if !local.contains_key(id) && doc.record(id).is_some() {
            doc.delete(id, at);
        }
    }
}

/// Records that differ between two versions of a document.
pub fn changed_ids(before: &SectionDoc, after: &SectionDoc) -> BTreeSet<String> {
    before.records.keys().chain(after.records.keys()).filter(|id| before.records.get(*id) != after.records.get(*id)).cloned().collect()
}

/// The newest stamp in a document (to keep this device's clock after it).
pub fn latest_ms(doc: &SectionDoc) -> i64 {
    doc.records.values().filter_map(|r| r.latest().map(|s| s.ms).max(r.deleted.as_ref().map(|d| d.ms))).max().unwrap_or(0)
}

fn sorted(items: &[String]) -> Vec<String> {
    let mut v = items.to_vec();
    v.sort();
    v.dedup();
    v
}

fn wanted(only: Option<&BTreeSet<String>>, id: &str) -> bool {
    only.is_none_or(|o| o.contains(id))
}

fn text(r: &Record, f: &str) -> Option<String> {
    r.get(f).and_then(Value::as_str).map(String::from)
}

// ----------------------------------------------------------------------------
// Collect
// ----------------------------------------------------------------------------

/// The section's records on this device. None: don't sync it this time (a
/// credential this device can't read, say, mustn't look deleted).
pub fn collect(engine: &Engine, section: &str) -> Result<Option<Local>> {
    Ok(Some(match section {
        sections::ACCOUNTS => collect_accounts(engine)?,
        sections::CALENDARS => collect_calendars(engine)?,
        sections::SIGNATURES => collect_signatures(engine)?,
        sections::TEMPLATES => collect_templates(engine)?,
        sections::SENDER_RULES => collect_sender_rules(engine)?,
        sections::SNOOZES => collect_snoozes(engine)?,
        sections::PREFS => collect_prefs(engine)?,
        sections::SHORTCUTS => collect_shortcuts(engine),
        s => match s.strip_prefix(sections::CRED_PREFIX) {
            Some(owner) => return collect_cred(engine, owner),
            None => return Ok(None),
        },
    }))
}

fn server_json(s: &ServerConfig) -> Value {
    json!({ "host": s.host, "port": s.port, "security": s.security.as_str() })
}

fn server_from(v: &Value) -> Option<ServerConfig> {
    Some(ServerConfig {
        host: v.get("host")?.as_str()?.to_string(),
        port: u16::try_from(v.get("port")?.as_u64()?).ok()?,
        security: Security::parse(v.get("security").and_then(Value::as_str).unwrap_or("tls")),
    })
}

fn folder_prefix(account_id: &str) -> String {
    format!("folder:{account_id}:")
}

fn collect_accounts(engine: &Engine) -> Result<Local> {
    let hidden = engine.ui_prefs().sidebar_hidden;
    let mut out = Local::new();
    for a in engine.accounts()? {
        let mut r = LocalRecord::default();
        let f = &mut r.fields;
        f.insert(account::EMAIL.into(), json!(a.email));
        f.insert(account::DISPLAY_NAME.into(), json!(a.display_name));
        f.insert(account::USERNAME.into(), json!(a.username));
        f.insert(account::PROVIDER.into(), json!(account_provider(&a.email, &a.imap.host, &a.auth).as_str()));
        f.insert(account::AUTH.into(), json!(a.auth));
        f.insert(account::IMAP.into(), server_json(&a.imap));
        f.insert(account::SMTP.into(), server_json(&a.smtp));
        f.insert(account::COLOR.into(), json!(a.color));
        f.insert(account::SORT_ORDER.into(), json!(a.sort_order));
        f.insert(account::HIDDEN_IN_UNIFIED.into(), json!(a.hidden_in_unified));
        f.insert(account::CREATED_AT.into(), json!(a.created_at));
        r.sets.insert(account::AUTO_BCC.into(), sorted(&a.auto_bcc));
        if let Some(h) = &hidden {
            let prefix = folder_prefix(&a.id);
            let labels: Vec<String> = h.iter().filter_map(|e| e.strip_prefix(&prefix)).map(String::from).collect();
            r.sets.insert(account::HIDDEN_FOLDERS.into(), sorted(&labels));
        }
        out.insert(a.id, r);
    }
    Ok(out)
}

/// How this device writes and reads credential sections.
enum CredMode {
    /// Sealed with the account's vault key (docs/platform/zero-knowledge.md).
    Sealed(VaultKey),
    /// The account seals them, but this device hasn't the key yet (it asks
    /// for the password): it neither writes nor reads them.
    Locked,
    /// An account not converted yet: as before (the server's encryption).
    Plain,
}

fn cred_mode(engine: &Engine) -> CredMode {
    let me: Option<remail_proto::api::Me> = engine.store().setting(super::ME_KEY);
    match me.and_then(|m| m.vault_key_id) {
        None => CredMode::Plain,
        Some(id) => match vault::load(engine) {
            Some(vk) if vk.id == id => CredMode::Sealed(vk),
            _ => CredMode::Locked,
        },
    }
}

fn collect_cred(engine: &Engine, owner: &str) -> Result<Option<Local>> {
    let mut out = Local::new();
    let secrets = engine.secrets();
    let mode = cred_mode(engine);
    let mut r = LocalRecord { exhaustive: true, ..Default::default() };
    if owner.starts_with("acc_") {
        let Some(acc) = engine.store().account(owner)? else { return Ok(Some(out)) };
        if acc.auth == crate::oauth::GOOGLE && matches!(mode, CredMode::Plain) {
            // Unsealed, Google sign-ins never leave the device: no Gmail access
            // rests on the server in a form it could read (Google's
            // restricted-scope rules). An older upload is deleted here.
            return Ok(Some(out));
        }
        let Some(secret) = secrets.get(owner)? else { return Ok(None) };
        if acc.auth == "password" {
            r.fields.insert(cred::KIND.into(), json!("password"));
            r.fields.insert(cred::PASSWORD.into(), json!(secret));
        } else {
            let Ok(grant) = serde_json::from_str::<Value>(&secret) else { return Ok(None) };
            r.fields.insert(cred::KIND.into(), json!("oauth"));
            r.fields.insert(cred::GRANT.into(), grant);
        }
    } else if owner.starts_with("cal_") {
        let Some(c) = engine.store().synced_cal_accounts()?.into_iter().find(|c| c.id == owner) else { return Ok(Some(out)) };
        if c.mail_account_id.is_some() {
            return Ok(Some(out)); // signs in with its mail account's credential
        }
        match secrets.get(&format!("cal:{owner}"))? {
            Some(p) => {
                r.fields.insert(cred::KIND.into(), json!("password"));
                r.fields.insert(cred::PASSWORD.into(), json!(p));
            }
            None if c.username.is_empty() => return Ok(Some(out)),
            None => return Ok(None),
        }
    } else {
        return Ok(None);
    }
    match mode {
        CredMode::Plain => {}
        CredMode::Locked => return Ok(None),
        CredMode::Sealed(vk) => {
            let plain = zeroize::Zeroizing::new(serde_json::to_vec(&r.fields)?);
            let sealed = vault::seal(&vk, owner, &plain)?;
            r.fields = BTreeMap::new();
            r.fields.insert(cred::KIND.into(), json!(cred::SEALED));
            r.fields.insert(cred::VAULT_KEY.into(), json!(vk.id));
            r.fields.insert(cred::SEALED.into(), json!(sealed));
        }
    }
    out.insert(SINGLETON.into(), r);
    Ok(Some(out))
}

fn collect_calendars(engine: &Engine) -> Result<Local> {
    let store = engine.store();
    let mut out = Local::new();
    for c in store.synced_cal_accounts()? {
        let mut r = LocalRecord::default();
        r.fields.insert("kind".into(), json!(c.kind));
        r.fields.insert("name".into(), json!(c.name));
        r.fields.insert("url".into(), json!(c.url));
        r.fields.insert("username".into(), json!(c.username));
        r.fields.insert("mailAccountId".into(), json!(c.mail_account_id));
        r.fields.insert("createdAt".into(), json!(c.created_at));
        out.insert(c.id, r);
    }
    for (key, color, enabled) in store.calendar_looks()? {
        let mut r = LocalRecord::default();
        r.fields.insert("color".into(), json!(color));
        r.fields.insert("enabled".into(), json!(enabled));
        out.insert(format!("{OVERRIDE_PREFIX}{key}"), r);
    }
    let p = engine.settings().calendar;
    let mut r = LocalRecord::default();
    r.fields.insert("weekStart".into(), json!(p.week_start));
    r.fields.insert("weekNumbers".into(), json!(p.week_numbers));
    r.fields.insert("weather".into(), json!(p.weather));
    r.fields.insert("units".into(), json!(p.units));
    // Only a default this device can name portably (one whose calendar
    // hasn't been found here yet mustn't overwrite the user's choice).
    if let Some((cal, href)) = p.default_calendar.as_deref().map(|id| store.calendar_ref(id)).transpose()?.flatten() {
        r.fields.insert("defaultCalendar".into(), json!({ "cal": cal, "href": href }));
    }
    out.insert(CAL_PREFS.into(), r);
    let off: Vec<String> = store.setting(OUTLOOK_OFF).unwrap_or_default();
    let mut r = LocalRecord::default();
    r.sets.insert(CAL_OUTLOOK_OFF.into(), sorted(&off));
    out.insert(CAL_OUTLOOK.into(), r);
    Ok(out)
}

fn collect_signatures(engine: &Engine) -> Result<Local> {
    Ok(engine
        .signatures()?
        .into_iter()
        .map(|s| {
            let mut r = LocalRecord::default();
            r.fields.insert("accountId".into(), json!(s.account_id));
            r.fields.insert("name".into(), json!(s.name));
            r.fields.insert("html".into(), json!(s.html));
            r.fields.insert("isDefault".into(), json!(s.is_default));
            (s.id, r)
        })
        .collect())
}

fn collect_templates(engine: &Engine) -> Result<Local> {
    Ok(engine
        .templates()?
        .into_iter()
        .map(|t| {
            let mut r = LocalRecord::default();
            r.fields.insert("name".into(), json!(t.name));
            r.fields.insert("subject".into(), json!(t.subject));
            r.fields.insert("html".into(), json!(t.html));
            (t.id, r)
        })
        .collect())
}

fn collect_sender_rules(engine: &Engine) -> Result<Local> {
    Ok(engine
        .sender_rules()?
        .into_iter()
        .map(|s| {
            let mut r = LocalRecord::default();
            r.fields.insert("category".into(), json!(s.category));
            (s.email, r)
        })
        .collect())
}

/// Snoozes travel by the newest message's Message-ID: thread ids are local.
fn collect_snoozes(engine: &Engine) -> Result<Local> {
    let google = google_accounts(engine)?;
    let db = engine.store().db();
    let rows: Vec<(String, String, i64)> = {
        let mut stmt = db.prepare("SELECT account_id, thread_id, until FROM snoozes WHERE until > ?")?;
        let rows = stmt.query_map([now_ms()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut out = Local::new();
    for (acc, thread, until) in rows {
        if google.contains(&acc) {
            continue;
        }
        let mid: Option<String> = db
            .query_row(
                "SELECT message_id FROM messages WHERE account_id = ? AND thread_id = ? AND message_id IS NOT NULL ORDER BY date DESC LIMIT 1",
                params![acc, thread],
                |r| r.get(0),
            )
            .optional()?;
        let Some(mid) = mid else { continue };
        let mut r = LocalRecord::default();
        r.fields.insert("account".into(), json!(acc));
        r.fields.insert("messageId".into(), json!(mid));
        r.fields.insert("until".into(), json!(until));
        out.insert(snooze_key(&acc, &mid), r);
    }
    Ok(out)
}

/// Google accounts: their snoozes stay on the device too (a Message-ID is
/// Gmail data, which mustn't rest on the Remail server).
fn google_accounts(engine: &Engine) -> Result<BTreeSet<String>> {
    Ok(engine.accounts()?.into_iter().filter(|a| a.auth == crate::oauth::GOOGLE).map(|a| a.id).collect())
}

fn snooze_key(account_id: &str, message_id: &str) -> String {
    format!("{account_id}|{message_id}")
}

fn collect_prefs(engine: &Engine) -> Result<Local> {
    let app = serde_json::to_value(engine.settings())?;
    let mut r = LocalRecord::default();
    for k in USER_PREFS {
        if let Some(v) = app.get(*k) {
            r.fields.insert((*k).into(), v.clone());
        }
    }
    let clean = engine.settings().auto_clean;
    r.fields.insert("autoClean.enabled".into(), json!(clean.enabled));
    r.fields.insert("autoClean.action".into(), json!(clean.action));
    for rule in &clean.rules {
        r.fields.insert(format!("autoClean.after.{}", rule.kind), json!(rule.after_minutes));
    }
    r.sets.insert("autoClean.rules".into(), sorted(&clean.rules.iter().map(|c| c.kind.clone()).collect::<Vec<_>>()));
    let ai = engine.ai_settings();
    r.fields.insert("ai.enabled".into(), json!(ai.enabled));
    r.fields.insert("ai.instructions".into(), json!(ai.instructions));
    r.fields.insert("ai.smartSort".into(), json!(ai.smart_sort));
    let ui = engine.ui_prefs();
    r.fields.insert("lastComposeAccount".into(), json!(ui.last_compose_account));
    if let Some(pins) = &ui.mobile_pins {
        r.fields.insert("mobilePins".into(), json!(pins));
    }
    if let Some(swipe) = &ui.swipe {
        r.fields.insert("swipe".into(), json!(swipe));
    }
    if let Some(h) = &ui.sidebar_hidden {
        // Folder entries travel with their accounts (`hiddenFolders`).
        let global: Vec<String> = h.iter().filter(|e| !e.starts_with("folder:")).cloned().collect();
        r.sets.insert("sidebarHidden".into(), sorted(&global));
    }
    Ok([(SINGLETON.to_string(), r)].into())
}

fn collect_shortcuts(engine: &Engine) -> Local {
    let r = LocalRecord {
        fields: engine.ui_prefs().shortcuts.into_iter().map(|(k, v)| (k, json!(v))).collect(),
        exhaustive: true,
        ..Default::default()
    };
    [(SINGLETON.to_string(), r)].into()
}

// ----------------------------------------------------------------------------
// Apply
// ----------------------------------------------------------------------------

/// Make the store match `doc` for the records in `only` (all when None).
/// `reset`: the server's copy replaces this device's, so local records the
/// document doesn't have go too.
pub fn apply(engine: &Engine, section: &str, doc: &SectionDoc, only: Option<&BTreeSet<String>>, reset: bool) -> Result<()> {
    match section {
        sections::ACCOUNTS => apply_accounts(engine, doc, only, reset),
        sections::CALENDARS => apply_calendars(engine, doc, only, reset),
        sections::SIGNATURES => apply_signatures(engine, doc, only, reset),
        sections::TEMPLATES => apply_templates(engine, doc, only, reset),
        sections::SENDER_RULES => apply_sender_rules(engine, doc, only, reset),
        sections::SNOOZES => apply_snoozes(engine, doc, reset),
        sections::PREFS => apply_prefs(engine, doc),
        sections::SHORTCUTS => apply_shortcuts(engine, doc),
        s => match s.strip_prefix(sections::CRED_PREFIX) {
            Some(owner) => apply_cred(engine, owner, doc),
            None => Ok(()),
        },
    }
}

struct AccountRec {
    new: NewAccount,
    auth: String,
    look: AccountLook,
}

impl AccountRec {
    fn parse(r: &Record) -> Option<AccountRec> {
        let email = text(r, account::EMAIL)?.trim().to_lowercase();
        let username = text(r, account::USERNAME).filter(|u| !u.trim().is_empty()).unwrap_or_else(|| email.clone());
        let new = NewAccount {
            display_name: text(r, account::DISPLAY_NAME).unwrap_or_default(),
            username,
            password: String::new(),
            imap: server_from(r.get(account::IMAP)?)?,
            smtp: server_from(r.get(account::SMTP)?)?,
            allow_invalid_certs: false,
            email,
        };
        let look = AccountLook {
            color: text(r, account::COLOR).unwrap_or_else(|| "#3b81f6".into()),
            sort_order: r.get(account::SORT_ORDER).and_then(Value::as_i64).unwrap_or(0),
            hidden_in_unified: r.get(account::HIDDEN_IN_UNIFIED).and_then(Value::as_bool).unwrap_or(false),
            auto_bcc: r.set_items(account::AUTO_BCC),
            created_at: r.get(account::CREATED_AT).and_then(Value::as_i64).unwrap_or_else(now_ms),
        };
        Some(AccountRec { new, auth: text(r, account::AUTH).unwrap_or_else(|| "password".into()), look })
    }

    /// Gmail files sent mail itself; the worker can't tell before it connects.
    fn is_gmail(&self) -> bool {
        let host = self.new.imap.host.to_ascii_lowercase();
        self.auth == crate::oauth::GOOGLE || host == "imap.gmail.com" || host == "imap.googlemail.com"
    }
}

fn apply_accounts(engine: &Engine, doc: &SectionDoc, only: Option<&BTreeSet<String>>, reset: bool) -> Result<()> {
    let store = engine.store();
    let local: HashMap<String, Account> = store.accounts()?.into_iter().map(|a| (a.id.clone(), a)).collect();
    let mut hidden: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, rec) in &doc.records {
        if !wanted(only, id) {
            continue;
        }
        if rec.is_deleted() {
            if local.contains_key(id) {
                engine.remove_account_here(id)?;
            }
            continue;
        }
        let Some(want) = AccountRec::parse(rec) else {
            tracing::warn!(account = %id, "skipping an account record that can't be read");
            continue;
        };
        match local.get(id) {
            None => {
                if local.values().any(|a| a.id != *id && a.email.eq_ignore_ascii_case(&want.new.email)) {
                    tracing::warn!(account = %id, "an account with the same address is here under another id; not adding it twice");
                    continue;
                }
                store.insert_account_with_id(id, &want.new, want.is_gmail(), &want.auth, &want.look)?;
                engine.start_worker(id);
                engine.emit_event(EngineEvent::MailboxChanged { account_id: id.clone() });
            }
            Some(cur) => {
                let servers = cur.imap != want.new.imap || cur.smtp != want.new.smtp || cur.username != want.new.username || cur.auth != want.auth;
                let mut changed = false;
                if servers {
                    store.update_account_servers(id, &NewAccount { allow_invalid_certs: cur.allow_invalid_certs, ..want.new.clone() })?;
                    store.set_account_auth(id, &want.auth)?;
                    changed = true;
                } else if cur.display_name != want.new.display_name.trim() {
                    store.update_account_prefs(id, Some(&want.new.display_name), None, None, None)?;
                    changed = true;
                }
                let look = AccountLook { created_at: cur.created_at, ..want.look.clone() };
                let cur_look = AccountLook {
                    color: cur.color.clone(),
                    sort_order: cur.sort_order,
                    hidden_in_unified: cur.hidden_in_unified,
                    auto_bcc: sorted(&cur.auto_bcc),
                    created_at: cur.created_at,
                };
                if look != cur_look {
                    store.set_account_look(id, &look)?;
                    changed = true;
                }
                if servers {
                    engine.restart_worker(id);
                }
                if changed {
                    engine.emit_event(EngineEvent::MailboxChanged { account_id: id.clone() });
                }
            }
        }
        if rec.sets.contains_key(account::HIDDEN_FOLDERS) {
            hidden.insert(id.clone(), rec.set_items(account::HIDDEN_FOLDERS));
        }
    }
    if reset {
        for id in local.keys().filter(|id| doc.record(id).is_none()) {
            engine.remove_account_here(id)?;
        }
    }
    if !hidden.is_empty() {
        let mut ui = engine.ui_prefs();
        let mut list = ui.sidebar_hidden.clone().unwrap_or_default();
        for (acc, labels) in &hidden {
            let prefix = folder_prefix(acc);
            list.retain(|e| !e.starts_with(&prefix));
            list.extend(labels.iter().map(|l| format!("{prefix}{l}")));
        }
        if ui.sidebar_hidden.as_ref() != Some(&list) {
            ui.sidebar_hidden = Some(list);
            store.set_setting(UI_PREFS, &ui)?;
            engine.settings_applied();
        }
    }
    Ok(())
}

fn apply_cred(engine: &Engine, owner: &str, doc: &SectionDoc) -> Result<()> {
    // Written straight to the store: this is the server's copy, nothing to sync back.
    let secret_id = crate::engine::credential_key(owner);
    let secrets = engine.secrets();
    let Some(rec) = doc.records.get(SINGLETON) else { return Ok(()) };
    let have = secrets.get(&secret_id)?;
    let google = |v: Option<&Value>| v.and_then(|g| g.get("provider")).and_then(Value::as_str) == Some(crate::oauth::GOOGLE);
    let sealed = rec.get(cred::KIND).and_then(Value::as_str) == Some(cred::SEALED);
    // Unsealed, the server has (and sends) no Google sign-in: keep this device's own.
    if !sealed && google(have.as_deref().and_then(|h| serde_json::from_str::<Value>(h).ok()).as_ref()) {
        return Ok(());
    }
    if rec.is_deleted() {
        return secrets.delete(&secret_id);
    }
    // What the record holds: opened with the vault key when sealed.
    let fields: serde_json::Map<String, Value> = if sealed {
        let CredMode::Sealed(vk) = cred_mode(engine) else { return Ok(()) };
        if rec.get(cred::VAULT_KEY).and_then(Value::as_str) != Some(vk.id.as_str()) {
            return Ok(());
        }
        let Some(plain) = rec.get(cred::SEALED).and_then(Value::as_str).and_then(|s| vault::open(&vk, owner, s)) else {
            tracing::warn!(owner, "a sealed sign-in didn't open with this account's vault key");
            return Ok(());
        };
        serde_json::from_slice(&plain)?
    } else {
        [cred::KIND, cred::PASSWORD, cred::GRANT].iter().filter_map(|k| Some((k.to_string(), rec.get(k)?.clone()))).collect()
    };
    let want = match fields.get(cred::KIND).and_then(Value::as_str) {
        Some("oauth") => fields.get(cred::GRANT).filter(|g| g.is_object()).cloned(),
        _ => fields.get(cred::PASSWORD).filter(|p| p.is_string()).cloned(),
    };
    // Unsealed: a Google sign-in uploaded before they stayed on devices isn't used.
    let Some(want) = want.filter(|w| sealed || !google(Some(w))) else { return Ok(()) };
    // Grants compare as JSON (key order differs between writers).
    let same = match (&want, &have) {
        (Value::String(p), Some(h)) => p == h,
        (v, Some(h)) => serde_json::from_str::<Value>(h).ok().as_ref() == Some(v),
        (_, None) => false,
    };
    if same {
        return Ok(());
    }
    // The same sign-in with a rotated refresh token (another device
    // refreshed): keep it for the next refresh, but don't drop this device's
    // access token. Restarting would refresh at once, rotate again and send
    // the new token back: devices would keep refreshing each other.
    let rotated_only = match (&want, have.as_deref().and_then(|h| serde_json::from_str::<Value>(h).ok())) {
        (Value::Object(new), Some(Value::Object(old))) => ["provider", "clientId", "subject"].iter().all(|k| new.get(*k) == old.get(*k)),
        _ => false,
    };
    let value = match want {
        Value::String(p) => p,
        v => v.to_string(),
    };
    secrets.set(&secret_id, &value)?;
    if let Some(acc) = engine.store().account(owner)? {
        if acc.status == AccountStatus::AuthError {
            // Signed in again elsewhere: this device can use the new credential now.
            engine.store().set_account_status(owner, AccountStatus::Active, None)?;
        } else if rotated_only {
            return Ok(());
        }
        engine.restart_worker(owner);
    }
    Ok(())
}

fn apply_calendars(engine: &Engine, doc: &SectionDoc, only: Option<&BTreeSet<String>>, reset: bool) -> Result<()> {
    let store = engine.store();
    let local: BTreeSet<String> = store.synced_cal_accounts()?.into_iter().map(|c| c.id).collect();
    // Colours first, so calendars found from here on start with them.
    let overrides: HashMap<String, CalOverride> = doc
        .live()
        .filter_map(|(id, r)| {
            let key = id.strip_prefix(OVERRIDE_PREFIX)?;
            Some((key.to_string(), CalOverride { color: text(r, "color"), enabled: r.get("enabled").and_then(Value::as_bool) }))
        })
        .collect();
    store.set_setting(CAL_OVERRIDES, &overrides)?;
    let mut new_accounts = Vec::new();
    let mut looks_changed = false;
    for (id, rec) in &doc.records {
        if !wanted(only, id) {
            continue;
        }
        if id.starts_with(OVERRIDE_PREFIX) {
            looks_changed = true;
        } else if id.starts_with("cal_") {
            if rec.is_deleted() {
                engine.cal_remove_here(id)?;
                continue;
            }
            let a = SyncedCalAccount {
                id: id.clone(),
                kind: text(rec, "kind").unwrap_or_else(|| crate::calendar::KIND_CALDAV.into()),
                mail_account_id: text(rec, "mailAccountId"),
                name: text(rec, "name").unwrap_or_default(),
                url: text(rec, "url").unwrap_or_default(),
                username: text(rec, "username").unwrap_or_default(),
                created_at: rec.get("createdAt").and_then(Value::as_i64).unwrap_or_else(now_ms),
            };
            if a.url.is_empty() {
                continue;
            }
            if engine.cal_apply_account(&a)? && !local.contains(id) {
                new_accounts.push(id.clone());
            }
        } else if id == CAL_PREFS && !rec.is_deleted() {
            apply_calendar_prefs(engine, rec)?;
        } else if id == CAL_OUTLOOK && !rec.is_deleted() {
            let off = rec.set_items(CAL_OUTLOOK_OFF);
            let before: Vec<String> = store.setting(OUTLOOK_OFF).unwrap_or_default();
            if sorted(&before) != off {
                store.set_setting(OUTLOOK_OFF, &off)?;
                let newly: Vec<String> = off.iter().filter(|m| !before.contains(m)).cloned().collect();
                engine.cal_remove_outlook(&newly)?;
            }
        }
    }
    if looks_changed {
        engine.cal_apply_overrides(&overrides)?;
    }
    if reset {
        for id in local.iter().filter(|id| doc.record(id).is_none()) {
            engine.cal_remove_here(id)?;
        }
    }
    engine.cal_applied(new_accounts);
    Ok(())
}

fn apply_calendar_prefs(engine: &Engine, r: &Record) -> Result<()> {
    let mut app = engine.settings();
    let before = app.calendar.clone();
    let p = &mut app.calendar;
    if let Some(v) = r.get("weekStart").and_then(Value::as_u64).and_then(|v| u8::try_from(v).ok()).filter(|v| *v <= 6) {
        p.week_start = v;
    }
    if let Some(v) = r.get("weekNumbers").and_then(Value::as_bool) {
        p.week_numbers = v;
    }
    if let Some(v) = r.get("weather") {
        if let Ok(place) = serde_json::from_value(v.clone()) {
            p.weather = place;
        }
    }
    if let Some(v) = text(r, "units") {
        p.units = v;
    }
    if let Some(v) = r.get("defaultCalendar") {
        let key = v.get("cal").and_then(Value::as_str);
        let href = v.get("href").and_then(Value::as_str);
        if let (Some(key), Some(href)) = (key, href) {
            // Not found here yet: keep what this device has until it is.
            if let Some(local) = engine.store().calendar_by_ref(key, href)? {
                p.default_calendar = Some(local);
            }
        } else if v.is_null() {
            p.default_calendar = None;
        }
    }
    if app.calendar != before {
        engine.store().set_setting("app", &app)?;
        engine.settings_applied();
    }
    Ok(())
}

fn apply_signatures(engine: &Engine, doc: &SectionDoc, only: Option<&BTreeSet<String>>, reset: bool) -> Result<()> {
    let store = engine.store();
    let local: HashMap<String, Signature> = store.signatures()?.into_iter().map(|s| (s.id.clone(), s)).collect();
    for (id, rec) in &doc.records {
        if !wanted(only, id) {
            continue;
        }
        if rec.is_deleted() {
            if local.contains_key(id) {
                store.delete_signature(id)?;
            }
            continue;
        }
        let want = Signature {
            id: id.clone(),
            account_id: text(rec, "accountId"),
            name: text(rec, "name").unwrap_or_default(),
            html: text(rec, "html").unwrap_or_default(),
            is_default: rec.get("isDefault").and_then(Value::as_bool).unwrap_or(false),
        };
        if let Some(acc) = &want.account_id {
            if store.account(acc)?.is_none() {
                continue; // its account hasn't arrived (yet)
            }
        }
        let same = local.get(id).is_some_and(|s| (&s.account_id, &s.name, &s.html, s.is_default) == (&want.account_id, &want.name, &want.html, want.is_default));
        if !same {
            store.db().execute(
                "INSERT INTO signatures(id, account_id, name, html, is_default, updated_at) VALUES(?,?,?,?,?,?)
                 ON CONFLICT(id) DO UPDATE SET account_id = excluded.account_id, name = excluded.name, html = excluded.html,
                   is_default = excluded.is_default, updated_at = excluded.updated_at",
                params![want.id, want.account_id, want.name, want.html, want.is_default, now_ms()],
            )?;
        }
    }
    if reset {
        for id in local.keys().filter(|id| doc.record(id).is_none()) {
            store.delete_signature(id)?;
        }
    }
    Ok(())
}

fn apply_templates(engine: &Engine, doc: &SectionDoc, only: Option<&BTreeSet<String>>, reset: bool) -> Result<()> {
    let store = engine.store();
    let local: HashMap<String, Template> = store.templates()?.into_iter().map(|t| (t.id.clone(), t)).collect();
    for (id, rec) in &doc.records {
        if !wanted(only, id) {
            continue;
        }
        if rec.is_deleted() {
            if local.contains_key(id) {
                store.delete_template(id)?;
            }
            continue;
        }
        let want = Template {
            id: id.clone(),
            name: text(rec, "name").unwrap_or_default(),
            subject: text(rec, "subject").unwrap_or_default(),
            html: text(rec, "html").unwrap_or_default(),
            updated_at: now_ms(),
        };
        if local.get(id).is_none_or(|t| (&t.name, &t.subject, &t.html) != (&want.name, &want.subject, &want.html)) {
            store.save_template(&want)?;
        }
    }
    if reset {
        for id in local.keys().filter(|id| doc.record(id).is_none()) {
            store.delete_template(id)?;
        }
    }
    Ok(())
}

fn apply_sender_rules(engine: &Engine, doc: &SectionDoc, only: Option<&BTreeSet<String>>, reset: bool) -> Result<()> {
    let store = engine.store();
    let local: HashMap<String, String> = store.sender_rules()?.into_iter().map(|r| (r.email, r.category)).collect();
    let mut changed = false;
    for (email, rec) in &doc.records {
        if !wanted(only, email) {
            continue;
        }
        let want = if rec.is_deleted() { None } else { text(rec, "category").filter(|c| crate::classify::Category::parse(c).is_some()) };
        if local.get(email) != want.as_ref() {
            store.set_sender_rule(email, want.as_deref())?;
            changed = true;
        }
    }
    if reset {
        for email in local.keys().filter(|e| doc.record(e).is_none()) {
            store.set_sender_rule(email, None)?;
            changed = true;
        }
    }
    if changed {
        for a in engine.accounts()? {
            engine.emit_event(EngineEvent::MailboxChanged { account_id: a.id });
        }
    }
    Ok(())
}

/// Snoozes are applied in full every time: one whose message hasn't synced
/// here yet is picked up once it has.
fn apply_snoozes(engine: &Engine, doc: &SectionDoc, reset: bool) -> Result<()> {
    let store = engine.store();
    let now = now_ms();
    let thread_of = |acc: &str, mid: &str| -> Result<Option<String>> {
        Ok(store
            .db()
            .query_row("SELECT thread_id FROM messages WHERE account_id = ? AND message_id = ? LIMIT 1", params![acc, mid], |r| r.get(0))
            .optional()?)
    };
    let google = google_accounts(engine)?;
    let mut touched = BTreeSet::new();
    for (id, rec) in &doc.records {
        let Some((acc, mid)) = id.split_once('|') else { continue };
        if google.contains(acc) {
            continue;
        }
        let Some(thread) = thread_of(acc, mid)? else { continue };
        if rec.is_deleted() {
            if store.snoozed_until(acc, &thread)?.is_some_and(|u| u > now) && store.unsnooze(acc, &thread)? {
                touched.insert(acc.to_string());
            }
            continue;
        }
        let Some(until) = rec.get("until").and_then(Value::as_i64).filter(|u| *u > now) else { continue };
        if store.snoozed_until(acc, &thread)? != Some(until) {
            store.snooze(acc, &thread, until)?;
            touched.insert(acc.to_string());
        }
    }
    if reset {
        let live: BTreeSet<&String> = doc.live().map(|(id, _)| id).collect();
        for (key, r) in collect_snoozes(engine)? {
            if !live.contains(&key) {
                let (Some(acc), Some(mid)) = (r.fields.get("account").and_then(Value::as_str), r.fields.get("messageId").and_then(Value::as_str)) else {
                    continue;
                };
                if let Some(thread) = thread_of(acc, mid)? {
                    store.unsnooze(acc, &thread)?;
                    touched.insert(acc.to_string());
                }
            }
        }
    }
    if !touched.is_empty() {
        engine.snoozes_applied(&touched.into_iter().collect::<Vec<_>>());
    }
    Ok(())
}

fn apply_prefs(engine: &Engine, doc: &SectionDoc) -> Result<()> {
    let Some(r) = doc.record(SINGLETON) else { return Ok(()) };
    let mut changed = false;

    let before = engine.settings();
    let before_v = serde_json::to_value(&before)?;
    let mut v = before_v.clone();
    for k in USER_PREFS {
        if let Some(x) = r.get(k).filter(|x| !x.is_null()) {
            v[*k] = x.clone();
        }
    }
    let mut app: AppSettings = serde_json::from_value(v).unwrap_or_else(|_| before.clone());
    if let Some(x) = r.get("autoClean.enabled").and_then(Value::as_bool) {
        app.auto_clean.enabled = x;
    }
    if let Some(x) = text(r, "autoClean.action") {
        app.auto_clean.action = x;
    }
    if r.sets.contains_key("autoClean.rules") {
        app.auto_clean.rules = r
            .set_items("autoClean.rules")
            .into_iter()
            .map(|kind| CleanRule {
                after_minutes: r.get(&format!("autoClean.after.{kind}")).and_then(Value::as_i64).unwrap_or(0),
                // When a rule started counting is this device's own business.
                since: before.auto_clean.rules.iter().find(|b| b.kind == kind).map(|b| b.since).unwrap_or(0),
                kind,
            })
            .collect();
    }
    if serde_json::to_value(&app)? != before_v {
        engine.set_settings(&app)?;
        changed = true;
    }

    let mut ai = engine.ai_settings();
    let ai_before = (ai.enabled, ai.instructions.clone(), ai.smart_sort);
    if let Some(x) = r.get("ai.enabled").and_then(Value::as_bool) {
        ai.enabled = x;
    }
    if let Some(x) = text(r, "ai.instructions") {
        ai.instructions = x;
    }
    if let Some(x) = r.get("ai.smartSort").and_then(Value::as_bool) {
        ai.smart_sort = x;
    }
    if (ai.enabled, ai.instructions.clone(), ai.smart_sort) != ai_before {
        engine.set_ai_settings(&ai, None)?;
        changed = true;
    }

    let mut ui = engine.ui_prefs();
    let ui_before = ui.clone();
    if let Some(x) = r.get("lastComposeAccount") {
        ui.last_compose_account = x.as_str().map(String::from);
    }
    if let Some(x) = r.get("mobilePins") {
        ui.mobile_pins = serde_json::from_value(x.clone()).unwrap_or(None);
    }
    if let Some(x) = r.get("swipe") {
        ui.swipe = serde_json::from_value(x.clone()).unwrap_or(None);
    }
    if r.sets.contains_key("sidebarHidden") {
        let mut list = r.set_items("sidebarHidden");
        list.extend(ui.sidebar_hidden.iter().flatten().filter(|e| e.starts_with("folder:")).cloned());
        ui.sidebar_hidden = Some(list);
    }
    if ui != ui_before {
        engine.store().set_setting(UI_PREFS, &ui)?;
        changed = true;
    }
    if changed {
        engine.settings_applied();
    }
    Ok(())
}

fn apply_shortcuts(engine: &Engine, doc: &SectionDoc) -> Result<()> {
    let Some(r) = doc.record(SINGLETON) else { return Ok(()) };
    let map: BTreeMap<String, Vec<String>> =
        r.fields.iter().filter_map(|(k, l)| serde_json::from_value::<Vec<String>>(l.value.clone()).ok().map(|v| (k.clone(), v))).collect();
    let mut ui = engine.ui_prefs();
    if ui.shortcuts != map {
        ui.shortcuts = map;
        engine.store().set_setting(UI_PREFS, &ui)?;
        engine.settings_applied();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(ms: i64) -> Stamp {
        Stamp::new(ms, "dev")
    }

    #[test]
    fn stamping_changes_only_what_changed() {
        let mut local = Local::new();
        let mut r = LocalRecord::default();
        r.fields.insert("a".into(), json!(1));
        r.fields.insert("b".into(), json!(2));
        r.sets.insert("s".into(), vec!["y".into(), "x".into()]);
        local.insert("r1".into(), r.clone());
        let mut doc = SectionDoc::default();
        stamp(&mut doc, &local, &BTreeSet::new(), &st(10));
        assert_eq!(doc.record("r1").unwrap().set_items("s"), vec!["x", "y"]);
        let snapshot = doc.clone();
        // Nothing changed: nothing is restamped.
        stamp(&mut doc, &local, &["r1".to_string()].into(), &st(20));
        assert_eq!(doc, snapshot);
        // One field changed: only it gets the new stamp.
        local.get_mut("r1").unwrap().fields.insert("b".into(), json!(3));
        stamp(&mut doc, &local, &["r1".to_string()].into(), &st(30));
        let rec = doc.record("r1").unwrap();
        assert_eq!((rec.fields["a"].at.ms, rec.fields["b"].at.ms), (10, 30));
        // Gone here, and known here before: deleted.
        stamp(&mut doc, &Local::new(), &["r1".to_string()].into(), &st(40));
        assert!(doc.record("r1").is_none());
        // Never here (say, still waiting to be applied): left alone.
        let mut other = snapshot.clone();
        stamp(&mut other, &Local::new(), &BTreeSet::new(), &st(50));
        assert!(other.record("r1").is_some());
        assert_eq!(changed_ids(&snapshot, &doc), ["r1".to_string()].into());
    }

    #[test]
    fn exhaustive_records_clear_missing_fields() {
        let mut doc = SectionDoc::default();
        let mut l = LocalRecord { exhaustive: true, ..Default::default() };
        l.fields.insert("compose".into(), json!(["c"]));
        l.fields.insert("archive".into(), json!(["e"]));
        stamp(&mut doc, &[("_".to_string(), l.clone())].into(), &BTreeSet::new(), &st(1));
        l.fields.remove("archive");
        stamp(&mut doc, &[("_".to_string(), l)].into(), &BTreeSet::new(), &st(2));
        assert_eq!(doc.record("_").unwrap().get("archive"), Some(&Value::Null));
    }
}
