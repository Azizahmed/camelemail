//! SQLite persistence.
//!
//! One database file holds accounts, folders, message metadata, raw message
//! bytes, the full-text index, and the local-only features (snooze, outbox,
//! drafts, signatures, templates, settings). Message rows have a stable id;
//! their IMAP location (`path`, `uid_validity`, `uid`) is updatable so a server
//! move doesn't change the id the UI holds.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use serde::{de::DeserializeOwned, Serialize};

use crate::mime::ParsedMessage;
use crate::model::*;
use crate::search;

const SCHEMA_VERSION: i64 = 4;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS accounts (
  id TEXT PRIMARY KEY,
  email TEXT NOT NULL UNIQUE,
  display_name TEXT NOT NULL DEFAULT '',
  username TEXT NOT NULL,
  imap_host TEXT NOT NULL,
  imap_port INTEGER NOT NULL,
  imap_security TEXT NOT NULL,
  smtp_host TEXT NOT NULL,
  smtp_port INTEGER NOT NULL,
  smtp_security TEXT NOT NULL,
  allow_invalid_certs INTEGER NOT NULL DEFAULT 0,
  color TEXT NOT NULL,
  sort_order INTEGER NOT NULL DEFAULT 0,
  hidden_in_unified INTEGER NOT NULL DEFAULT 0,
  auto_bcc TEXT NOT NULL DEFAULT '[]',
  is_gmail INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'active',
  last_error TEXT,
  created_at INTEGER NOT NULL,
  -- 'password', or the OAuth provider that signs the account in ('microsoft')
  auth TEXT NOT NULL DEFAULT 'password'
);

-- IMAP folders, mapped onto labels.
CREATE TABLE IF NOT EXISTS folders (
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  label_id TEXT NOT NULL,
  path TEXT NOT NULL,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  parent_id TEXT,
  delimiter TEXT,
  synced INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (account_id, label_id)
);

-- Per-folder sync cursor.
CREATE TABLE IF NOT EXISTS folder_state (
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  path TEXT NOT NULL,
  uid_validity INTEGER NOT NULL,
  uid_next INTEGER NOT NULL,
  highest_modseq INTEGER,
  oldest_uid INTEGER,
  fully_synced INTEGER NOT NULL DEFAULT 0,
  unread_offset INTEGER NOT NULL DEFAULT 0,
  last_sync INTEGER NOT NULL,
  PRIMARY KEY (account_id, path)
);

CREATE TABLE IF NOT EXISTS messages (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  thread_id TEXT NOT NULL,
  folder TEXT NOT NULL,
  path TEXT NOT NULL,
  uid_validity INTEGER NOT NULL,
  uid INTEGER NOT NULL,
  message_id TEXT,
  in_reply_to TEXT,
  refs_json TEXT NOT NULL DEFAULT '[]',
  subject TEXT NOT NULL DEFAULT '',
  from_name TEXT NOT NULL DEFAULT '',
  from_email TEXT NOT NULL DEFAULT '',
  to_json TEXT NOT NULL DEFAULT '[]',
  cc_json TEXT NOT NULL DEFAULT '[]',
  bcc_json TEXT NOT NULL DEFAULT '[]',
  reply_to_json TEXT NOT NULL DEFAULT '[]',
  date INTEGER NOT NULL,
  snippet TEXT NOT NULL DEFAULT '',
  is_unread INTEGER NOT NULL DEFAULT 0,
  is_starred INTEGER NOT NULL DEFAULT 0,
  is_draft INTEGER NOT NULL DEFAULT 0,
  is_answered INTEGER NOT NULL DEFAULT 0,
  has_attachments INTEGER NOT NULL DEFAULT 0,
  unsub_json TEXT,
  body_synced INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  -- Smart Inbox bucket from headers (classify.rs), and the AI's opinion.
  category TEXT,
  ai_category TEXT,
  -- When the message became read (auto-clean), and whether it's a one-time code.
  read_at INTEGER,
  otp INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_location
  ON messages(account_id, path, uid_validity, uid) WHERE uid > 0;
CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages(account_id, thread_id, date);
CREATE INDEX IF NOT EXISTS idx_messages_folder_date ON messages(account_id, folder, date DESC);
-- The thread list walks it newest first (`walk_candidates`).
CREATE INDEX IF NOT EXISTS idx_messages_date ON messages(date DESC);
CREATE INDEX IF NOT EXISTS idx_messages_msgid ON messages(account_id, message_id);
CREATE INDEX IF NOT EXISTS idx_messages_sender ON messages(account_id, from_email);
-- Redundant with the (folder, is_unread, …) index below.
DROP INDEX IF EXISTS idx_messages_unread;
CREATE INDEX IF NOT EXISTS idx_messages_read_at ON messages(folder, is_unread, read_at);
CREATE INDEX IF NOT EXISTS idx_messages_otp_unknown ON messages(otp) WHERE otp IS NULL;
CREATE INDEX IF NOT EXISTS idx_messages_uncategorized ON messages(date DESC) WHERE category IS NULL;

-- Raw RFC 5322 bytes, fetched during sync or on open.
CREATE TABLE IF NOT EXISTS bodies (
  message_id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
  raw BLOB NOT NULL
);

-- Text is indexed through `search::normalize_ar` (v4; older rows are
-- re-indexed in the background, see `reindex_fts_batch`).
CREATE VIRTUAL TABLE IF NOT EXISTS message_fts USING fts5(
  subject, participants, body, content='', contentless_delete=1, tokenize='unicode61 remove_diacritics 2', prefix='2 3'
);
CREATE TRIGGER IF NOT EXISTS messages_fts_delete AFTER DELETE ON messages BEGIN
  DELETE FROM message_fts WHERE rowid = old.seq;
END;

CREATE TABLE IF NOT EXISTS snoozes (
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  thread_id TEXT NOT NULL,
  until INTEGER NOT NULL,
  notified INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (account_id, thread_id)
);
CREATE INDEX IF NOT EXISTS idx_snoozes_until ON snoozes(until);

CREATE TABLE IF NOT EXISTS drafts (
  id TEXT PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  json TEXT NOT NULL,
  thread_id TEXT,
  server_message_id TEXT,
  pending_send INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS signatures (
  id TEXT PRIMARY KEY,
  account_id TEXT REFERENCES accounts(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  html TEXT NOT NULL,
  is_default INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS templates (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  subject TEXT NOT NULL DEFAULT '',
  html TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS outbox (
  id TEXT PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  draft_json TEXT NOT NULL,
  send_at INTEGER NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_outbox_send_at ON outbox(status, send_at);

-- Pending server mutations (archive/trash/move/flags) with retry.
CREATE TABLE IF NOT EXISTS op_queue (
  id INTEGER PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  op_json TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  next_attempt_at INTEGER NOT NULL,
  last_error TEXT,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS contacts (
  email TEXT PRIMARY KEY,
  name TEXT NOT NULL DEFAULT '',
  frequency INTEGER NOT NULL DEFAULT 0,
  last_seen INTEGER NOT NULL,
  -- Messages we sent to this address (people we write to are Important).
  sent_count INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_contacts_rank ON contacts(frequency DESC, last_seen DESC);

-- Conversations the user rescued from auto-clean.
CREATE TABLE IF NOT EXISTS autoclean_exempt (
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  thread_id TEXT NOT NULL,
  PRIMARY KEY (account_id, thread_id)
);

-- Smart Inbox overrides: "always Important" / "always Other" per sender.
CREATE TABLE IF NOT EXISTS sender_rules (
  email TEXT PRIMARY KEY,
  category TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);

-- Setup sections from the Remail server (cloud::cache): the merged document,
-- AES-256-GCM encrypted with this device's cache key.
CREATE TABLE IF NOT EXISTS cloud_sections (
  id TEXT PRIMARY KEY,
  version INTEGER NOT NULL DEFAULT 0,
  doc_json_encrypted BLOB NOT NULL,
  -- Local changes the server hasn't taken yet.
  dirty INTEGER NOT NULL DEFAULT 0,
  last_sync INTEGER NOT NULL DEFAULT 0
);
"#;

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2", params![table, column], |r| r.get(0))?;
    Ok(n > 0)
}

/// Bring an existing database up to date, before `SCHEMA` runs. Works from
/// what the tables contain rather than the version number alone, and runs in
/// one transaction so an interrupted upgrade is simply redone.
fn migrate(conn: &mut Connection, version: i64) -> Result<()> {
    if !has_column(conn, "messages", "seq")? {
        return Ok(()); // new database
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if version < 4 {
        // v4 indexes Arabic without harakat/hamza variants and with prefix
        // indexes. The table is contentless, so `SCHEMA` recreates it empty
        // and `reindex_fts_batch` refills it from the stored messages.
        tx.execute_batch("DROP TABLE IF EXISTS message_fts;")?;
        let below: i64 = tx.query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM messages", [], |r| r.get(0))?;
        tx.execute(
            "INSERT INTO settings(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![FTS_REINDEX_KEY, below.to_string()],
        )?;
        // Calendar search also covers attendees now: `calendar::upgrade` fills it in again.
        if has_column(&tx, "cal_objects", "search_text")? {
            tx.execute_batch("UPDATE cal_objects SET search_text = NULL;")?;
        }
    }
    if !has_column(&tx, "messages", "category")? {
        tx.execute_batch("ALTER TABLE messages ADD COLUMN category TEXT;")?;
    }
    if !has_column(&tx, "messages", "ai_category")? {
        tx.execute_batch("ALTER TABLE messages ADD COLUMN ai_category TEXT;")?;
    }
    if !has_column(&tx, "messages", "read_at")? {
        tx.execute_batch("ALTER TABLE messages ADD COLUMN read_at INTEGER;")?;
    }
    if !has_column(&tx, "messages", "otp")? {
        tx.execute_batch("ALTER TABLE messages ADD COLUMN otp INTEGER;")?;
    }
    if !has_column(&tx, "accounts", "auth")? {
        tx.execute_batch("ALTER TABLE accounts ADD COLUMN auth TEXT NOT NULL DEFAULT 'password';")?;
    }
    if !has_column(&tx, "contacts", "sent_count")? {
        tx.execute_batch("ALTER TABLE contacts ADD COLUMN sent_count INTEGER NOT NULL DEFAULT 0;")?;
        // Rebuild "people we write to" from the Sent folder.
        let rows: Vec<(String, String, String)> = {
            let mut stmt = tx.prepare("SELECT to_json, cc_json, bcc_json FROM messages WHERE folder = 'SENT'")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut bump = tx.prepare("UPDATE contacts SET sent_count = sent_count + 1 WHERE email = lower(?)")?;
        for (to, cc, bcc) in rows {
            for list in [to, cc, bcc] {
                for a in from_json::<Vec<EmailAddress>>(&list) {
                    bump.execute([a.email])?;
                }
            }
        }
    }
    tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
    tx.commit()?;
    Ok(())
}

/// Settings key: messages with a lower `seq` still wait for `reindex_fts_batch`.
const FTS_REINDEX_KEY: &str = "fts_reindex_below";

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn new_id(prefix: &str) -> String {
    format!("{}_{}", prefix, uuid::Uuid::new_v4().simple())
}

/// Message row to insert or refresh during sync.
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub account_id: String,
    pub thread_id: String,
    pub folder: String,
    pub path: String,
    pub uid_validity: u32,
    pub uid: u32,
    pub date: i64,
    pub is_unread: bool,
    pub is_starred: bool,
    pub is_draft: bool,
    pub is_answered: bool,
    pub parsed: ParsedMessage,
    pub raw: Option<Vec<u8>>,
}

/// IMAP location of a stored message.
#[derive(Debug, Clone)]
pub struct Location {
    pub id: String,
    pub account_id: String,
    pub thread_id: String,
    pub folder: String,
    pub path: String,
    pub uid_validity: u32,
    pub uid: u32,
    pub message_id: Option<String>,
    pub is_unread: bool,
    pub is_starred: bool,
}

#[derive(Debug, Clone)]
pub struct FolderState {
    pub uid_validity: u32,
    pub uid_next: u32,
    pub highest_modseq: Option<u64>,
    pub oldest_uid: Option<u32>,
    pub fully_synced: bool,
}

#[derive(Debug, Clone)]
pub struct FolderRow {
    pub label_id: String,
    pub path: String,
    pub name: String,
    pub kind: LabelKind,
    pub parent_id: Option<String>,
    pub delimiter: Option<String>,
    pub synced: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartCounts {
    pub important: i64,
    pub other: i64,
}

/// What auto-clean and Smart Inbox know about one conversation.
#[derive(Debug, Clone)]
pub struct ThreadFacts {
    pub account_id: String,
    pub thread_id: String,
    /// Smart Inbox bucket (`primary` = Important).
    pub category: String,
    pub unread: i64,
    pub starred: bool,
    /// The user wrote in it.
    pub mine: bool,
    pub otp: bool,
    pub in_inbox: bool,
    /// Latest time an Inbox message in it was read.
    pub read_at: Option<i64>,
    /// Some Inbox message was read before read times were recorded.
    pub read_unknown: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SenderRule {
    pub email: String,
    pub category: String,
    pub updated_at: i64,
}

/// Inbox message waiting for the AI to sort it.
#[derive(Debug, Clone)]
pub struct AiSortCandidate {
    pub id: String,
    pub from: EmailAddress,
    pub subject: String,
    pub snippet: String,
    pub heuristic: String,
    pub bulk: bool,
}

#[derive(Debug, Clone)]
pub struct QueuedOp {
    pub id: i64,
    pub account_id: String,
    pub op_json: String,
    pub attempts: i64,
}

/// The synced, cosmetic part of an account row.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountLook {
    pub color: String,
    pub sort_order: i64,
    pub hidden_in_unified: bool,
    pub auto_bcc: Vec<String>,
    pub created_at: i64,
}

pub struct Store {
    conn: Mutex<Connection>,
    /// Messages inserted since the planner's statistics were last refreshed.
    inserted: std::sync::atomic::AtomicU32,
    /// Second, read-only connection to a database file so list and count
    /// queries don't wait behind sync writes (WAL lets both run at once).
    /// In-memory stores have a single connection.
    reader: Option<Mutex<Connection>>,
    path: Option<std::path::PathBuf>,
}

/// Per-connection tuning shared by the writer and the reader.
const CONNECTION_PRAGMAS: &str = "PRAGMA busy_timeout = 5000; PRAGMA temp_store = MEMORY; \
    PRAGMA cache_size = -32000; PRAGMA mmap_size = 268435456;";

fn to_json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

fn from_json<T: DeserializeOwned + Default>(s: &str) -> T {
    serde_json::from_str(s).unwrap_or_default()
}

const MESSAGE_COLUMNS: &str = "m.id, m.account_id, m.thread_id, m.folder, m.uid, m.message_id, m.subject, \
    m.from_name, m.from_email, m.to_json, m.cc_json, m.bcc_json, m.reply_to_json, m.date, m.snippet, \
    m.is_unread, m.is_starred, m.is_draft, m.is_answered, m.has_attachments, m.unsub_json, m.body_synced";

fn message_meta_from_row(r: &Row<'_>) -> rusqlite::Result<MessageMeta> {
    let folder: String = r.get(3)?;
    let is_starred: bool = r.get(16)?;
    let mut labels = vec![folder.clone()];
    if is_starred {
        labels.push(labels::STARRED.into());
    }
    let unsub: Option<String> = r.get(20)?;
    Ok(MessageMeta {
        id: r.get(0)?,
        account_id: r.get(1)?,
        thread_id: r.get(2)?,
        folder,
        uid: r.get::<_, i64>(4)? as u32,
        message_id: r.get(5)?,
        subject: r.get(6)?,
        from: EmailAddress { name: r.get(7)?, email: r.get(8)? },
        to: from_json(&r.get::<_, String>(9)?),
        cc: from_json(&r.get::<_, String>(10)?),
        bcc: from_json(&r.get::<_, String>(11)?),
        reply_to: from_json(&r.get::<_, String>(12)?),
        date: r.get(13)?,
        snippet: r.get(14)?,
        is_unread: r.get(15)?,
        is_starred,
        is_draft: r.get(17)?,
        is_answered: r.get(18)?,
        has_attachments: r.get(19)?,
        labels,
        unsubscribe: unsub.and_then(|s| serde_json::from_str(&s).ok()),
        body_synced: r.get(21)?,
    })
}

fn account_from_row(r: &Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account {
        id: r.get("id")?,
        email: r.get("email")?,
        display_name: r.get("display_name")?,
        username: r.get("username")?,
        imap: ServerConfig {
            host: r.get("imap_host")?,
            port: r.get::<_, i64>("imap_port")? as u16,
            security: Security::parse(&r.get::<_, String>("imap_security")?),
        },
        smtp: ServerConfig {
            host: r.get("smtp_host")?,
            port: r.get::<_, i64>("smtp_port")? as u16,
            security: Security::parse(&r.get::<_, String>("smtp_security")?),
        },
        allow_invalid_certs: r.get("allow_invalid_certs")?,
        color: r.get("color")?,
        sort_order: r.get("sort_order")?,
        hidden_in_unified: r.get("hidden_in_unified")?,
        auto_bcc: from_json(&r.get::<_, String>("auto_bcc")?),
        is_gmail: r.get("is_gmail")?,
        auth: r.get("auth")?,
        status: AccountStatus::parse(&r.get::<_, String>("status")?),
        last_error: r.get("last_error")?,
        created_at: r.get("created_at")?,
    })
}

/// A message's Smart Inbox bucket: the user's sender rule, then "people we
/// write to are Important", then the AI's verdict, then the header heuristic.
const EFFECTIVE_CATEGORY: &str =
    "COALESCE(r.category, CASE WHEN c.sent_count > 0 THEN 'primary' END, m.ai_category, m.category, 'primary')";

/// A thread is Important when any received message is, when we took part
/// in it, or when it's starred; otherwise it takes its latest message's bucket.
const THREAD_CATEGORY: &str =
    "CASE WHEN agg.any_primary OR agg.mine OR agg.starred THEN 'primary' ELSE COALESCE(l.ecat, 'primary') END";

/// `important` | `other` | a bucket name → SQL condition on `tcat`.
fn category_filter(category: Option<&str>) -> String {
    match category.map(str::to_ascii_lowercase).as_deref() {
        None | Some("") | Some("all") => "1".into(),
        Some("important") | Some("primary") => "tcat = 'primary'".into(),
        Some("other") => "tcat != 'primary'".into(),
        Some(c) => match crate::classify::Category::parse(c) {
            Some(cat) => format!("tcat = '{}'", cat.as_str()),
            None => "1".into(),
        },
    }
}

/// Accent colours assigned to accounts in creation order.
const ACCOUNT_COLORS: &[&str] = &["#3b81f6", "#f97316", "#10b981", "#a855f7", "#ef4444", "#eab308", "#06b6d4", "#ec4899"];

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut store = Self::init(conn)?;
        // Opened after `init` so it sees the finished schema.
        store.reader = Some(Mutex::new(Self::open_reader(path)?));
        store.path = Some(path.to_path_buf());
        Ok(store)
    }

    fn open_reader(path: &Path) -> Result<Connection> {
        let reader = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        reader.execute_batch(CONNECTION_PRAGMAS)?;
        reader.execute_batch("PRAGMA query_only = 1;")?;
        Ok(reader)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Store> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON; \
             PRAGMA journal_size_limit = 67108864;",
        )?;
        conn.execute_batch(CONNECTION_PRAGMAS)?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            anyhow::bail!("database schema version {version} is newer than this build supports");
        }
        // Idempotent (checks the actual columns), so it also repairs databases
        // whose version number got ahead of their tables.
        migrate(&mut conn, version).context("upgrading the database")?;
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(crate::calendar::SCHEMA)?;
        conn.execute_batch(crate::graphmail::SCHEMA)?;
        crate::calendar::upgrade(&conn).context("upgrading the calendar tables")?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
        // Collect planner statistics where they're missing or stale (bounded
        // per index, so this stays quick on large mailboxes).
        conn.execute_batch("PRAGMA analysis_limit = 400; PRAGMA optimize = 0x10002;")?;
        Ok(Store { conn: Mutex::new(conn), inserted: Default::default(), reader: None, path: None })
    }

    pub(crate) fn db(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Connection for plain reads: the read-only one when the store has it.
    /// Never call while holding `db()` (in memory they're the same lock).
    fn reader(&self) -> MutexGuard<'_, Connection> {
        match &self.reader {
            Some(r) => r.lock().unwrap_or_else(|p| p.into_inner()),
            None => self.db(),
        }
    }

    /// Refresh planner statistics; cheap, meant for idle times and quit.
    pub fn optimize(&self) -> Result<()> {
        self.db().execute_batch("PRAGMA optimize;")?;
        self.reload_reader();
        Ok(())
    }

    /// A connection reads planner statistics once, when it opens: reopen the
    /// reader so it plans with the ones `optimize` just refreshed.
    fn reload_reader(&self) {
        if let (Some(reader), Some(path)) = (&self.reader, &self.path) {
            match Self::open_reader(path) {
                Ok(fresh) => *reader.lock().unwrap_or_else(|p| p.into_inner()) = fresh,
                Err(e) => tracing::warn!("reopening the database reader failed: {e:#}"),
            }
        }
    }

    // ------------------------------------------------------------------
    // Settings
    // ------------------------------------------------------------------

    pub fn setting<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let raw: Option<String> = self
            .reader()
            .query_row("SELECT value FROM settings WHERE key = ?", [key], |r| r.get(0))
            .optional()
            .ok()
            .flatten();
        raw.and_then(|s| serde_json::from_str(&s).ok())
    }

    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.db().execute(
            "INSERT INTO settings(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, to_json(value)],
        )?;
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> Result<()> {
        self.db().execute("DELETE FROM settings WHERE key = ?", [key])?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Accounts
    // ------------------------------------------------------------------

    /// `auth`: `password` or the OAuth provider that signs the account in.
    pub fn insert_account(&self, a: &NewAccount, is_gmail: bool, auth: &str) -> Result<Account> {
        let db = self.db();
        let count: i64 = db.query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0))?;
        let id = new_id("acc");
        let color = ACCOUNT_COLORS[(count as usize) % ACCOUNT_COLORS.len()];
        let username = if a.username.trim().is_empty() { a.email.trim() } else { a.username.trim() };
        db.execute(
            "INSERT INTO accounts(id, email, display_name, username, imap_host, imap_port, imap_security,
               smtp_host, smtp_port, smtp_security, allow_invalid_certs, color, sort_order, is_gmail, created_at, auth)
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                id,
                a.email.trim().to_lowercase(),
                a.display_name.trim(),
                username,
                a.imap.host.trim(),
                a.imap.port,
                a.imap.security.as_str(),
                a.smtp.host.trim(),
                a.smtp.port,
                a.smtp.security.as_str(),
                a.allow_invalid_certs,
                color,
                count,
                is_gmail,
                now_ms(),
                auth
            ],
        )?;
        drop(db);
        self.account(&id)?.context("account vanished after insert")
    }

    /// Insert an account that already has an id (synced from the Remail
    /// server, which is the authority for ids). Device-only columns start fresh.
    pub fn insert_account_with_id(&self, id: &str, a: &NewAccount, is_gmail: bool, auth: &str, look: &AccountLook) -> Result<Account> {
        let username = if a.username.trim().is_empty() { a.email.trim() } else { a.username.trim() };
        self.db().execute(
            "INSERT INTO accounts(id, email, display_name, username, imap_host, imap_port, imap_security,
               smtp_host, smtp_port, smtp_security, allow_invalid_certs, color, sort_order, hidden_in_unified, auto_bcc,
               is_gmail, created_at, auth)
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                id,
                a.email.trim().to_lowercase(),
                a.display_name.trim(),
                username,
                a.imap.host.trim(),
                a.imap.port,
                a.imap.security.as_str(),
                a.smtp.host.trim(),
                a.smtp.port,
                a.smtp.security.as_str(),
                a.allow_invalid_certs,
                look.color,
                look.sort_order,
                look.hidden_in_unified,
                to_json(&look.auto_bcc),
                is_gmail,
                look.created_at,
                auth
            ],
        )?;
        self.account(id)?.context("account vanished after insert")
    }

    /// Give a local account another id (the one the Remail server knows it
    /// by), with everything that refers to it: mail, folders, drafts, outbox,
    /// snoozes, signatures and calendars.
    pub fn remap_account_id(&self, old: &str, new: &str) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        // Children point at the old id until the parent row moves too.
        tx.execute_batch("PRAGMA defer_foreign_keys = ON;")?;
        for table in ["folders", "folder_state", "messages", "snoozes", "drafts", "outbox", "op_queue", "autoclean_exempt", "signatures"] {
            tx.execute(&format!("UPDATE {table} SET account_id = ?1 WHERE account_id = ?2"), params![new, old])?;
        }
        tx.execute("UPDATE cal_accounts SET mail_account_id = ?1 WHERE mail_account_id = ?2", params![new, old])?;
        tx.execute("UPDATE accounts SET id = ?1 WHERE id = ?2", params![new, old])?;
        tx.commit()?;
        Ok(())
    }

    /// Order, colour and unified-inbox choice of an account, set together on
    /// accounts that come from another device.
    pub fn set_account_look(&self, id: &str, look: &AccountLook) -> Result<()> {
        self.db().execute(
            "UPDATE accounts SET color = ?, sort_order = ?, hidden_in_unified = ?, auto_bcc = ? WHERE id = ?",
            params![look.color, look.sort_order, look.hidden_in_unified, to_json(&look.auto_bcc), id],
        )?;
        Ok(())
    }

    /// How the account signs in: `password` or an OAuth provider id.
    pub fn set_account_auth(&self, id: &str, auth: &str) -> Result<()> {
        self.db().execute("UPDATE accounts SET auth = ? WHERE id = ?", params![auth, id])?;
        Ok(())
    }

    /// Whether the account's server is Gmail (it files sent mail itself).
    pub fn set_account_gmail(&self, id: &str, is_gmail: bool) -> Result<()> {
        self.db().execute("UPDATE accounts SET is_gmail = ? WHERE id = ?", params![is_gmail, id])?;
        Ok(())
    }

    pub fn update_account_servers(&self, id: &str, a: &NewAccount) -> Result<()> {
        let username = if a.username.trim().is_empty() { a.email.trim() } else { a.username.trim() };
        self.db().execute(
            "UPDATE accounts SET display_name=?, username=?, imap_host=?, imap_port=?, imap_security=?,
               smtp_host=?, smtp_port=?, smtp_security=?, allow_invalid_certs=?, status='active', last_error=NULL
             WHERE id=?",
            params![
                a.display_name.trim(),
                username,
                a.imap.host.trim(),
                a.imap.port,
                a.imap.security.as_str(),
                a.smtp.host.trim(),
                a.smtp.port,
                a.smtp.security.as_str(),
                a.allow_invalid_certs,
                id
            ],
        )?;
        Ok(())
    }

    pub fn accounts(&self) -> Result<Vec<Account>> {
        let db = self.reader();
        let mut stmt = db.prepare_cached("SELECT * FROM accounts ORDER BY sort_order, created_at")?;
        let rows = stmt.query_map([], account_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn account(&self, id: &str) -> Result<Option<Account>> {
        Ok(self.reader().query_row("SELECT * FROM accounts WHERE id = ?", [id], account_from_row).optional()?)
    }

    pub fn delete_account(&self, id: &str) -> Result<()> {
        self.db().execute("DELETE FROM accounts WHERE id = ?", [id])?;
        Ok(())
    }

    pub fn set_account_status(&self, id: &str, status: AccountStatus, error: Option<&str>) -> Result<()> {
        self.db().execute(
            "UPDATE accounts SET status = ?, last_error = ? WHERE id = ?",
            params![status.as_str(), error, id],
        )?;
        Ok(())
    }

    pub fn update_account_prefs(
        &self,
        id: &str,
        display_name: Option<&str>,
        color: Option<&str>,
        hidden_in_unified: Option<bool>,
        auto_bcc: Option<&[String]>,
    ) -> Result<()> {
        let db = self.db();
        if let Some(v) = display_name {
            db.execute("UPDATE accounts SET display_name = ? WHERE id = ?", params![v.trim(), id])?;
        }
        if let Some(v) = color {
            db.execute("UPDATE accounts SET color = ? WHERE id = ?", params![v, id])?;
        }
        if let Some(v) = hidden_in_unified {
            db.execute("UPDATE accounts SET hidden_in_unified = ? WHERE id = ?", params![v, id])?;
        }
        if let Some(v) = auto_bcc {
            db.execute("UPDATE accounts SET auto_bcc = ? WHERE id = ?", params![to_json(&v), id])?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Folders
    // ------------------------------------------------------------------

    pub fn replace_folders(&self, account_id: &str, folders: &[FolderRow]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute("DELETE FROM folders WHERE account_id = ?", [account_id])?;
        for f in folders {
            tx.execute(
                "INSERT OR REPLACE INTO folders(account_id, label_id, path, name, kind, parent_id, delimiter, synced)
                 VALUES(?,?,?,?,?,?,?,?)",
                params![
                    account_id,
                    f.label_id,
                    f.path,
                    f.name,
                    if f.kind == LabelKind::System { "system" } else { "user" },
                    f.parent_id,
                    f.delimiter,
                    f.synced
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn folders(&self, account_id: &str) -> Result<Vec<FolderRow>> {
        let db = self.reader();
        let mut stmt = db.prepare_cached(
            "SELECT label_id, path, name, kind, parent_id, delimiter, synced FROM folders WHERE account_id = ? ORDER BY path",
        )?;
        let rows = stmt.query_map([account_id], |r| {
            Ok(FolderRow {
                label_id: r.get(0)?,
                path: r.get(1)?,
                name: r.get(2)?,
                kind: if r.get::<_, String>(3)? == "system" { LabelKind::System } else { LabelKind::User },
                parent_id: r.get(4)?,
                delimiter: r.get(5)?,
                synced: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn folder_path(&self, account_id: &str, label_id: &str) -> Result<Option<String>> {
        Ok(self
            .db()
            .query_row(
                "SELECT path FROM folders WHERE account_id = ? AND label_id = ?",
                params![account_id, label_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn label_for_path(&self, account_id: &str, path: &str) -> Result<Option<String>> {
        Ok(self
            .db()
            .query_row(
                "SELECT label_id FROM folders WHERE account_id = ? AND path = ?",
                params![account_id, path],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Labels with unread / total thread-message counts. Unread counts for
    /// INBOX and user folders include the server-side offset for mail older
    /// than the local window.
    pub fn labels(&self, account_id: &str) -> Result<Vec<Label>> {
        let db = self.reader();
        let mut stmt = db.prepare_cached(
            "SELECT f.label_id, f.name, f.path, f.kind, f.parent_id,
               (SELECT COUNT(*) FROM messages m WHERE m.account_id = f.account_id AND m.folder = f.label_id AND m.is_unread = 1),
               (SELECT COUNT(*) FROM messages m WHERE m.account_id = f.account_id AND m.folder = f.label_id),
               COALESCE((SELECT unread_offset FROM folder_state s WHERE s.account_id = f.account_id AND s.path = f.path), 0)
             FROM folders f WHERE f.account_id = ?",
        )?;
        let rows = stmt.query_map([account_id], |r| {
            let kind = if r.get::<_, String>(3)? == "system" { LabelKind::System } else { LabelKind::User };
            let label_id: String = r.get(0)?;
            let mut unread: i64 = r.get(5)?;
            let offset: i64 = r.get(7)?;
            if label_id == labels::INBOX || kind == LabelKind::User {
                unread += offset;
            }
            Ok(Label {
                account_id: account_id.to_string(),
                id: label_id,
                name: r.get(1)?,
                path: Some(r.get(2)?),
                kind,
                parent_id: r.get(4)?,
                unread_count: unread,
                total_count: r.get(6)?,
            })
        })?;
        let mut out: Vec<Label> = rows.collect::<rusqlite::Result<_>>()?;
        out.sort_by_key(|l| (label_rank(&l.id), l.name.to_lowercase()));
        Ok(out)
    }

    /// Unread thread counts for the unified sidebar and window badge.
    pub fn unread_counts(&self, now: i64) -> Result<HashMap<String, i64>> {
        let db = self.reader();
        let mut out = HashMap::new();
        let mut stmt = db.prepare_cached(
            "SELECT m.account_id, COUNT(DISTINCT m.thread_id) FROM messages m
             LEFT JOIN snoozes s ON s.account_id = m.account_id AND s.thread_id = m.thread_id AND s.until > ?
             WHERE m.folder = 'INBOX' AND m.is_unread = 1 AND s.until IS NULL GROUP BY m.account_id",
        )?;
        for row in stmt.query_map([now], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (acc, n) = row?;
            out.insert(acc, n);
        }
        Ok(out)
    }

    // ------------------------------------------------------------------
    // Folder sync state
    // ------------------------------------------------------------------

    pub fn folder_state(&self, account_id: &str, path: &str) -> Result<Option<FolderState>> {
        Ok(self
            .db()
            .query_row(
                "SELECT uid_validity, uid_next, highest_modseq, oldest_uid, fully_synced FROM folder_state
                 WHERE account_id = ? AND path = ?",
                params![account_id, path],
                |r| {
                    Ok(FolderState {
                        uid_validity: r.get::<_, i64>(0)? as u32,
                        uid_next: r.get::<_, i64>(1)? as u32,
                        highest_modseq: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                        oldest_uid: r.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                        fully_synced: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn save_folder_state(&self, account_id: &str, path: &str, s: &FolderState) -> Result<()> {
        self.db().execute(
            "INSERT INTO folder_state(account_id, path, uid_validity, uid_next, highest_modseq, oldest_uid, fully_synced, last_sync)
             VALUES(?,?,?,?,?,?,?,?)
             ON CONFLICT(account_id, path) DO UPDATE SET uid_validity = excluded.uid_validity,
               uid_next = excluded.uid_next, highest_modseq = excluded.highest_modseq,
               oldest_uid = excluded.oldest_uid, fully_synced = excluded.fully_synced, last_sync = excluded.last_sync",
            params![
                account_id,
                path,
                s.uid_validity as i64,
                s.uid_next as i64,
                s.highest_modseq.map(|v| v as i64),
                s.oldest_uid.map(|v| v as i64),
                s.fully_synced,
                now_ms()
            ],
        )?;
        Ok(())
    }

    pub fn set_unread_offset(&self, account_id: &str, path: &str, offset: i64) -> Result<()> {
        self.db().execute(
            "UPDATE folder_state SET unread_offset = ? WHERE account_id = ? AND path = ?",
            params![offset.max(0), account_id, path],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Messages
    // ------------------------------------------------------------------

    /// Insert or refresh a message identified by its IMAP location. A row
    /// with an unknown UID (`uid = 0`, left by a move whose new UID we could
    /// not resolve) and the same Message-ID is adopted instead of duplicated.
    /// Returns `(id, inserted)`.
    pub fn upsert_message(&self, m: &IncomingMessage) -> Result<(String, bool)> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let p = &m.parsed;

        // `uid > 0` lets the planner use the partial location index (UIDs from
        // the server are never 0).
        let existing: Option<(String, i64)> = tx
            .prepare_cached(
                "SELECT id, seq FROM messages WHERE account_id = ? AND path = ? AND uid_validity = ? AND uid = ? AND uid > 0",
            )?
            .query_row(params![m.account_id, m.path, m.uid_validity as i64, m.uid as i64], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let existing = match existing {
            Some(e) => Some(e),
            None => match &p.message_id {
                Some(mid) => tx
                    .prepare_cached("SELECT id, seq FROM messages WHERE account_id = ? AND path = ? AND uid = 0 AND message_id = ?")?
                    .query_row(params![m.account_id, m.path, mid], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?,
                None => None,
            },
        };

        let unsub = p.unsubscribe.as_ref().map(to_json);
        let has_body = m.raw.is_some();
        let (id, seq, inserted) = match existing {
            Some((id, seq)) => {
                tx.prepare_cached(
                    "UPDATE messages SET thread_id=?, folder=?, path=?, uid_validity=?, uid=?, message_id=?, in_reply_to=?,
                       refs_json=?, subject=?, from_name=?, from_email=?, to_json=?, cc_json=?, bcc_json=?, reply_to_json=?,
                       date=?, snippet=CASE WHEN ? = '' THEN snippet ELSE ? END, is_unread=?, is_starred=?, is_draft=?,
                       is_answered=?, has_attachments=?, unsub_json=COALESCE(?, unsub_json),
                       body_synced=MAX(body_synced, ?), category=?, otp=?,
                       read_at=CASE WHEN ? THEN NULL WHEN is_unread = 1 THEN ? ELSE read_at END
                     WHERE id=?",
                )?
                .execute(params![
                        m.thread_id,
                        m.folder,
                        m.path,
                        m.uid_validity as i64,
                        m.uid as i64,
                        p.message_id,
                        p.in_reply_to,
                        to_json(&p.references),
                        p.subject,
                        p.from.name,
                        p.from.email.to_lowercase(),
                        to_json(&p.to),
                        to_json(&p.cc),
                        to_json(&p.bcc),
                        to_json(&p.reply_to),
                        m.date,
                        p.snippet,
                        p.snippet,
                        m.is_unread,
                        m.is_starred,
                        m.is_draft,
                        m.is_answered,
                        p.has_attachments(),
                        unsub,
                        has_body,
                        p.category.as_str(),
                        p.otp,
                        m.is_unread,
                        now_ms(),
                        id
                    ])?;
                (id, seq, false)
            }
            None => {
                let id = new_id("msg");
                tx.prepare_cached(
                    "INSERT INTO messages(id, account_id, thread_id, folder, path, uid_validity, uid, message_id, in_reply_to,
                       refs_json, subject, from_name, from_email, to_json, cc_json, bcc_json, reply_to_json, date, snippet,
                       is_unread, is_starred, is_draft, is_answered, has_attachments, unsub_json, body_synced, created_at, category,
                       otp, read_at)
                     VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                )?
                .execute(params![
                        id,
                        m.account_id,
                        m.thread_id,
                        m.folder,
                        m.path,
                        m.uid_validity as i64,
                        m.uid as i64,
                        p.message_id,
                        p.in_reply_to,
                        to_json(&p.references),
                        p.subject,
                        p.from.name,
                        p.from.email.to_lowercase(),
                        to_json(&p.to),
                        to_json(&p.cc),
                        to_json(&p.bcc),
                        to_json(&p.reply_to),
                        m.date,
                        p.snippet,
                        m.is_unread,
                        m.is_starred,
                        m.is_draft,
                        m.is_answered,
                        p.has_attachments(),
                        unsub,
                        has_body,
                        now_ms(),
                        p.category.as_str(),
                        p.otp,
                        // Already read when first seen: new mail (read on another device)
                        // counts as read now; older mail (moved back into the Inbox
                        // elsewhere, or an initial sync) is never auto-cleaned.
                        (!m.is_unread && m.date > now_ms() - 86_400_000).then(now_ms)
                    ])?;
                let seq = tx.last_insert_rowid();
                upsert_contacts(&tx, m)?;
                (id, seq, true)
            }
        };

        if let Some(raw) = &m.raw {
            tx.prepare_cached(
                "INSERT INTO bodies(message_id, raw) VALUES(?, ?) ON CONFLICT(message_id) DO UPDATE SET raw = excluded.raw",
            )?
            .execute(params![id, raw])?;
            index_fts(&tx, seq, p)?;
        } else if inserted {
            index_fts(&tx, seq, p)?;
        }
        tx.commit()?;
        // A first sync grows the tables fast; statistics taken when they were
        // small make the planner pick slow plans (10x at 100k messages).
        if inserted && self.inserted.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 5000 == 4999 {
            // Only a hint for the planner: never fail the sync over it.
            if let Err(e) = db.execute_batch("PRAGMA optimize;") {
                tracing::warn!("refreshing database statistics failed: {e}");
            }
            drop(db);
            self.reload_reader();
        }
        Ok((id, inserted))
    }

    pub fn location(&self, id: &str) -> Result<Option<Location>> {
        Ok(self
            .db()
            .query_row(
                "SELECT id, account_id, thread_id, folder, path, uid_validity, uid, message_id, is_unread, is_starred
                 FROM messages WHERE id = ?",
                [id],
                location_from_row,
            )
            .optional()?)
    }

    pub fn thread_locations(&self, account_id: &str, thread_id: &str) -> Result<Vec<Location>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT id, account_id, thread_id, folder, path, uid_validity, uid, message_id, is_unread, is_starred
             FROM messages WHERE account_id = ? AND thread_id = ? ORDER BY date",
        )?;
        let rows = stmt.query_map(params![account_id, thread_id], location_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Local UIDs held for a folder (the window used for flag/deletion diffs).
    pub fn local_uids(&self, account_id: &str, path: &str, uid_validity: u32) -> Result<Vec<u32>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT uid FROM messages WHERE account_id = ? AND path = ? AND uid_validity = ? AND uid > 0 ORDER BY uid",
        )?;
        let rows = stmt.query_map(params![account_id, path, uid_validity as i64], |r| r.get::<_, i64>(0))?;
        Ok(rows.map(|r| r.map(|v| v as u32)).collect::<rusqlite::Result<_>>()?)
    }

    /// Flag state by UID for a folder.
    pub fn local_flags(&self, account_id: &str, path: &str, uid_validity: u32) -> Result<HashMap<u32, (bool, bool)>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT uid, is_unread, is_starred FROM messages WHERE account_id = ? AND path = ? AND uid_validity = ? AND uid > 0",
        )?;
        let rows = stmt.query_map(params![account_id, path, uid_validity as i64], |r| {
            Ok((r.get::<_, i64>(0)? as u32, (r.get(1)?, r.get(2)?)))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Apply server flag state; returns how many rows changed.
    pub fn apply_flags(
        &self,
        account_id: &str,
        path: &str,
        uid_validity: u32,
        flags: &[(u32, bool, bool, bool)],
    ) -> Result<usize> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let mut changed = 0;
        {
            let mut stmt = tx.prepare(
                "UPDATE messages SET is_unread = ?, is_starred = ?, is_answered = ?,
                   read_at = CASE WHEN ? THEN NULL WHEN is_unread = 1 THEN ? ELSE read_at END
                 WHERE account_id = ? AND path = ? AND uid_validity = ? AND uid = ? AND uid > 0
                   AND (is_unread != ? OR is_starred != ? OR is_answered != ?)",
            )?;
            let now = now_ms();
            for (uid, unread, starred, answered) in flags {
                changed += stmt.execute(params![
                    unread,
                    starred,
                    answered,
                    unread,
                    now,
                    account_id,
                    path,
                    uid_validity as i64,
                    *uid as i64,
                    unread,
                    starred,
                    answered
                ])?;
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    pub fn delete_uids(&self, account_id: &str, path: &str, uid_validity: u32, uids: &[u32]) -> Result<usize> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let mut n = 0;
        {
            let mut stmt = tx.prepare(
                "DELETE FROM messages WHERE account_id = ? AND path = ? AND uid_validity = ? AND uid = ? AND uid > 0",
            )?;
            for uid in uids {
                n += stmt.execute(params![account_id, path, uid_validity as i64, *uid as i64])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Drop every message stored for a folder (UIDVALIDITY changed).
    pub fn delete_path_messages(&self, account_id: &str, path: &str) -> Result<usize> {
        Ok(self.db().execute("DELETE FROM messages WHERE account_id = ? AND path = ?", params![account_id, path])?)
    }

    pub fn delete_messages(&self, ids: &[String]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        {
            let mut stmt = tx.prepare_cached("DELETE FROM messages WHERE id = ?")?;
            for id in ids {
                stmt.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Optimistically record a move. `uid` is the new UID when known, else 0.
    pub fn set_location(&self, id: &str, folder: &str, path: &str, uid_validity: u32, uid: u32) -> Result<()> {
        let db = self.db();
        if uid > 0 {
            // A sync may already have inserted the moved copy; keep our row and drop that one.
            db.execute(
                "DELETE FROM messages WHERE account_id = (SELECT account_id FROM messages WHERE id = ?1)
                   AND path = ?2 AND uid_validity = ?3 AND uid = ?4 AND uid > 0 AND id != ?1",
                params![id, path, uid_validity as i64, uid as i64],
            )?;
        }
        db.execute(
            "UPDATE messages SET folder = ?, path = ?, uid_validity = ?, uid = ? WHERE id = ?",
            params![folder, path, uid_validity as i64, uid as i64, id],
        )?;
        Ok(())
    }

    /// Optimistic label change ahead of the server move; the IMAP location
    /// stays until the queued op has run.
    pub fn set_folder_local(&self, ids: &[String], folder: &str) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        {
            let mut stmt = tx.prepare_cached("UPDATE messages SET folder = ? WHERE id = ?")?;
            for id in ids {
                stmt.execute(params![folder, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// `(Message-ID, In-Reply-To, References)` of a stored message.
    #[allow(clippy::type_complexity)]
    pub fn threading_headers(&self, id: &str) -> Result<Option<(Option<String>, Option<String>, Vec<String>)>> {
        Ok(self
            .db()
            .query_row("SELECT message_id, in_reply_to, refs_json FROM messages WHERE id = ?", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, from_json(&r.get::<_, String>(2)?)))
            })
            .optional()?)
    }

    /// Thread of the first stored message (in `message_ids` order) with one of
    /// these Message-IDs, compared exactly as stored (normalised by `mime`).
    pub fn thread_for_references(&self, account_id: &str, message_ids: &[String]) -> Result<Option<String>> {
        if message_ids.is_empty() {
            return Ok(None);
        }
        let db = self.db();
        let marks = vec!["?"; message_ids.len()].join(",");
        let mut stmt =
            db.prepare(&format!("SELECT message_id, thread_id FROM messages WHERE account_id = ? AND message_id IN ({marks})"))?;
        let args = std::iter::once(account_id).chain(message_ids.iter().map(String::as_str));
        let rows = stmt.query_map(params_from_iter(args), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let found: HashMap<String, String> = rows.collect::<rusqlite::Result<_>>()?;
        Ok(message_ids.iter().find_map(|m| found.get(m).cloned()))
    }

    /// Is a message with this Message-ID stored in the account?
    pub fn message_id_known(&self, account_id: &str, message_id: &str) -> Result<bool> {
        Ok(self
            .db()
            .prepare_cached("SELECT 1 FROM messages WHERE account_id = ? AND message_id = ?")?
            .exists(params![account_id, message_id])?)
    }

    /// Move thread `from` into thread `into`: its messages, and its snooze,
    /// auto-clean exemption and drafts (where `into` has none of its own).
    /// Returns how many messages moved.
    pub fn merge_thread(&self, account_id: &str, from: &str, into: &str) -> Result<usize> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let moved =
            tx.execute("UPDATE messages SET thread_id = ?1 WHERE account_id = ?2 AND thread_id = ?3", params![into, account_id, from])?;
        for table in ["snoozes", "autoclean_exempt"] {
            tx.execute(
                &format!("UPDATE OR IGNORE {table} SET thread_id = ?1 WHERE account_id = ?2 AND thread_id = ?3"),
                params![into, account_id, from],
            )?;
            tx.execute(&format!("DELETE FROM {table} WHERE account_id = ?1 AND thread_id = ?2"), params![account_id, from])?;
        }
        tx.execute("UPDATE drafts SET thread_id = ?1 WHERE account_id = ?2 AND thread_id = ?3", params![into, account_id, from])?;
        tx.commit()?;
        Ok(moved)
    }

    pub fn set_flags_local(&self, ids: &[String], unread: Option<bool>, starred: Option<bool>) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        {
            let mut set_unread = tx.prepare_cached(
                "UPDATE messages SET is_unread = ?1,
                   read_at = CASE WHEN ?1 THEN NULL WHEN is_unread = 1 THEN ?2 ELSE read_at END
                 WHERE id = ?3",
            )?;
            let mut set_starred = tx.prepare_cached("UPDATE messages SET is_starred = ? WHERE id = ?")?;
            for id in ids {
                if let Some(u) = unread {
                    set_unread.execute(params![u, now_ms(), id])?;
                }
                if let Some(s) = starred {
                    set_starred.execute(params![s, id])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn raw_body(&self, id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.db().query_row("SELECT raw FROM bodies WHERE message_id = ?", [id], |r| r.get(0)).optional()?)
    }

    pub fn store_body(&self, id: &str, raw: &[u8], parsed: &ParsedMessage) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute(
            "INSERT INTO bodies(message_id, raw) VALUES(?, ?) ON CONFLICT(message_id) DO UPDATE SET raw = excluded.raw",
            params![id, raw],
        )?;
        let seq: i64 = tx.query_row("SELECT seq FROM messages WHERE id = ?", [id], |r| r.get(0))?;
        tx.execute(
            "UPDATE messages SET body_synced = 1, snippet = CASE WHEN ? = '' THEN snippet ELSE ? END,
               has_attachments = ?, unsub_json = COALESCE(?, unsub_json), category = ?, otp = ? WHERE id = ?",
            params![
                parsed.snippet,
                parsed.snippet,
                parsed.has_attachments(),
                parsed.unsubscribe.as_ref().map(to_json),
                parsed.category.as_str(),
                parsed.otp,
                id
            ],
        )?;
        index_fts(&tx, seq, parsed)?;
        tx.commit()?;
        Ok(())
    }

    /// Re-index up to `limit` messages stored before the current full-text
    /// index (schema v4), newest first. Returns how many; 0 once the backlog
    /// is done. Bodies are parsed outside the write lock.
    pub fn reindex_fts_batch(&self, limit: usize) -> Result<usize> {
        let limit = limit.max(1);
        match self.reindex_fts_read(limit)? {
            Some(rows) => self.reindex_fts_write(&rows, limit),
            None => Ok(0),
        }
    }

    /// The next batch of `reindex_fts_batch`, with its text ready to index.
    fn reindex_fts_read(&self, limit: usize) -> Result<Option<Vec<FtsPending>>> {
        let Some(below) = self.setting::<i64>(FTS_REINDEX_KEY) else {
            return Ok(None);
        };
        let mut rows: Vec<FtsPending> = {
            let db = self.reader();
            let mut stmt = db.prepare_cached(
                "SELECT m.seq, m.id, m.subject, m.from_name, m.from_email, m.to_json, m.cc_json, m.snippet, b.raw
                 FROM messages m LEFT JOIN bodies b ON b.message_id = m.id
                 WHERE m.seq < ? ORDER BY m.seq DESC LIMIT ?",
            )?;
            let rows = stmt.query_map(params![below, limit as i64], |r| {
                let from = EmailAddress { name: r.get(3)?, email: r.get(4)? };
                let to: Vec<EmailAddress> = from_json(&r.get::<_, String>(5)?);
                let cc: Vec<EmailAddress> = from_json(&r.get::<_, String>(6)?);
                Ok(FtsPending {
                    seq: r.get(0)?,
                    id: r.get(1)?,
                    text: (r.get(2)?, fts_participants(&from, &to, &cc), r.get(7)?),
                    raw: r.get(8)?,
                    had_body: false,
                })
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for p in &mut rows {
            p.had_body = p.raw.is_some();
            if let Some(parsed) = p.raw.take().and_then(|raw| crate::mime::parse(&raw)) {
                p.text = fts_text(&parsed);
            }
        }
        Ok(Some(rows))
    }

    fn reindex_fts_write(&self, rows: &[FtsPending], limit: usize) -> Result<usize> {
        let mut db = self.db();
        let tx = db.transaction()?;
        {
            // Skip rows deleted (or replaced) since they were read, and rows
            // whose body arrived meanwhile: `store_body` indexed all of it.
            let mut exists = tx.prepare_cached("SELECT 1 FROM messages WHERE seq = ? AND id = ?")?;
            let mut has_body = tx.prepare_cached("SELECT 1 FROM bodies WHERE message_id = ?")?;
            for p in rows {
                if exists.exists(params![p.seq, p.id])? && (p.had_body || !has_body.exists([&p.id])?) {
                    index_fts_text(&tx, p.seq, &p.text.0, &p.text.1, &p.text.2)?;
                }
            }
        }
        match rows.last() {
            Some(last) if rows.len() == limit => {
                tx.execute("UPDATE settings SET value = ? WHERE key = ?", params![last.seq.to_string(), FTS_REINDEX_KEY])?
            }
            _ => tx.execute("DELETE FROM settings WHERE key = ?", [FTS_REINDEX_KEY])?,
        };
        tx.commit()?;
        Ok(rows.len())
    }

    pub fn message_meta(&self, id: &str) -> Result<Option<MessageMeta>> {
        Self::message_meta_on(&self.reader(), id)
    }

    fn message_meta_on(db: &Connection, id: &str) -> Result<Option<MessageMeta>> {
        Ok(db
            .prepare_cached(&format!("SELECT {MESSAGE_COLUMNS} FROM messages m WHERE m.id = ?"))?
            .query_row([id], message_meta_from_row)
            .optional()?)
    }

    pub fn thread_messages(&self, account_id: &str, thread_id: &str) -> Result<Vec<MessageMeta>> {
        let db = self.reader();
        let mut stmt = db.prepare_cached(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages m WHERE m.account_id = ? AND m.thread_id = ? ORDER BY m.date, m.seq"
        ))?;
        let rows = stmt.query_map(params![account_id, thread_id], message_meta_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Oldest-UID cursor for "load more": the lowest stored UID in a folder.
    pub fn min_uid(&self, account_id: &str, path: &str, uid_validity: u32) -> Result<Option<u32>> {
        let v: Option<i64> = self.db().query_row(
            "SELECT MIN(uid) FROM messages WHERE account_id = ? AND path = ? AND uid_validity = ? AND uid > 0",
            params![account_id, path, uid_validity as i64],
            |r| r.get(0),
        )?;
        Ok(v.map(|v| v as u32))
    }

    /// Accounts the unified view covers: everything not hidden (or every
    /// account when only one exists). One `IN` list so the planner can still
    /// use the `account_id` indexes.
    fn unified_filter() -> &'static str {
        "m.account_id IN (SELECT id FROM accounts WHERE hidden_in_unified = 0 OR (SELECT COUNT(*) FROM accounts) = 1)"
    }

    /// Which messages put a thread in the list `q` asks for.
    fn thread_filter(&self, q: &ThreadQuery, now: i64) -> ThreadFilter {
        let mut matches: Vec<String> = Vec::new();
        let mut params: Vec<rusqlite::types::Value> = Vec::new();
        use rusqlite::types::Value as V;

        match &q.account_id {
            Some(acc) => {
                matches.push("m.account_id = ?".into());
                params.push(V::Text(acc.clone()));
            }
            None => matches.push(Self::unified_filter().into()),
        }

        let parsed = q.search.as_deref().map(|s| search::parse(s, now)).filter(|s| !s.is_empty());
        let mut label = q.label.clone();
        // `in:work` names a folder the way the user sees it.
        let mut typed_folder = None;
        if let Some(sq) = &parsed {
            if let Some(f) = &sq.folder {
                label = resolve_label_name(f);
                typed_folder = (label == *f).then(|| f.clone());
            }
        }
        let searching = parsed.is_some();

        match label.as_str() {
            labels::STARRED => matches.push("m.is_starred = 1 AND m.folder NOT IN ('TRASH','SPAM')".into()),
            labels::UNREAD => matches.push("m.is_unread = 1 AND m.folder = 'INBOX'".into()),
            labels::SNOOZED => {
                matches.push("EXISTS (SELECT 1 FROM snoozes s WHERE s.account_id = m.account_id AND s.thread_id = m.thread_id AND s.until > ?)".into());
                params.push(V::Integer(now));
            }
            // Search without in: covers everything except trash/spam.
            labels::ALL | "" => matches.push("m.folder NOT IN ('TRASH','SPAM')".into()),
            other => match typed_folder {
                Some(name) => {
                    matches.push(
                        "(m.folder = ?1 OR m.folder IN (SELECT f.label_id FROM folders f WHERE f.account_id = m.account_id
                           AND (f.label_id = ?1 COLLATE NOCASE OR f.name = ?1 COLLATE NOCASE OR f.path = ?1 COLLATE NOCASE)))"
                            .replace("?1", &format!("?{}", params.len() + 1)),
                    );
                    params.push(V::Text(name));
                }
                None => {
                    matches.push("m.folder = ?".into());
                    params.push(V::Text(other.to_string()));
                }
            },
        }
        if q.unread_only {
            matches.push("m.is_unread = 1".into());
        }
        if let Some(sq) = &parsed {
            let (clauses, ps) = search::sql_filters(sq);
            matches.extend(clauses);
            params.extend(ps.into_iter().map(V::Text));
            if let Some(fts) = search::fts_match(sq) {
                let fts_clause = "m.seq IN (SELECT rowid FROM message_fts WHERE message_fts MATCH ?)";
                params.push(V::Text(fts));
                // Messages the background re-index hasn't reached are matched
                // on their visible fields meanwhile.
                match self.setting::<i64>(FTS_REINDEX_KEY) {
                    Some(below) => {
                        let words = search::text_words(sq);
                        let like = "(lower(m.subject) LIKE ? ESCAPE '\\' OR lower(m.snippet) LIKE ? ESCAPE '\\' \
                                    OR lower(m.from_name) LIKE ? ESCAPE '\\' OR m.from_email LIKE ? ESCAPE '\\')";
                        let likes = vec![like; words.len()].join(" AND ");
                        matches.push(format!("({fts_clause} OR (m.seq < ? AND {likes}))"));
                        params.push(V::Integer(below));
                        for w in words {
                            let w = format!("%{}%", w.to_lowercase().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
                            params.extend(std::iter::repeat_n(V::Text(w), 4));
                        }
                    }
                    None => matches.push(fts_clause.into()),
                }
            }
        }

        // Which of a thread's messages count towards its row.
        let scope = match label.as_str() {
            labels::TRASH => "= 'TRASH'",
            labels::SPAM => "= 'SPAM'",
            _ => "NOT IN ('TRASH','SPAM')",
        };
        let snooze_filter = if label == labels::SNOOZED {
            "s.until > :now"
        } else if searching || label == labels::TRASH || label == labels::SPAM {
            "1"
        } else {
            "(s.until IS NULL OR s.until <= :now)"
        };
        let (accounts, account_params) = (matches[0].clone(), q.account_id.iter().map(|a| V::Text(a.clone())).collect());
        ThreadFilter {
            matches: matches.join(" AND "),
            params,
            accounts,
            account_params,
            snoozed: label == labels::SNOOZED,
            hide_snoozed: !searching && label != labels::SNOOZED && label != labels::TRASH && label != labels::SPAM,
            walkable: !searching && label != labels::SNOOZED && label != labels::STARRED,
            scope,
            snooze_filter,
        }
    }

    pub fn list_threads(&self, q: &ThreadQuery, now: i64) -> Result<Vec<ThreadSummary>> {
        let f = self.thread_filter(q, now);
        let limit = q.limit.clamp(1, 500);
        let cat_filter = category_filter(q.category.as_deref());
        // The newest `cand` threads are summarised; a category filter may
        // leave fewer than `limit` of them, so look further, then at all.
        let mut cand = if cat_filter == "1" { limit } else { limit * 4 };
        let out = loop {
            let (rows, cand_n) = self.thread_page(q, &f, &cat_filter, now, limit, cand)?;
            if rows.len() as i64 >= limit || cand < 0 || cand_n.is_some_and(|n| n < cand) {
                break rows;
            }
            cand = if cand < limit * 32 { limit * 32 } else { -1 };
        };
        self.fill_participants(out, f.scope)
    }

    /// One page of `list_threads` over the first `cand` threads (-1: all) in
    /// list order; also returns how many candidates there were, when known.
    fn thread_page(
        &self,
        q: &ThreadQuery,
        f: &ThreadFilter,
        cat_filter: &str,
        now: i64,
        limit: i64,
        cand: i64,
    ) -> Result<(Vec<ThreadSummary>, Option<i64>)> {
        let keyset = q.before.is_some() && q.before_account_id.is_some() && q.before_thread_id.is_some();
        // Ties on the sort key are broken by account and thread so keyset
        // pages neither skip nor repeat threads.
        let (cand_order, order, cursor) = if f.snoozed {
            (
                "s.until ASC, t.account_id ASC, t.thread_id ASC",
                "cand.until ASC, cand.account_id ASC, cand.thread_id ASC",
                // The list is in wake-up order: continue after the cursor thread's snooze.
                if keyset {
                    "(s.until, t.account_id, t.thread_id) > (COALESCE((SELECT until FROM snoozes
                       WHERE account_id = :ba AND thread_id = :bt), :before), :ba, :bt)"
                } else {
                    "(:before IS NULL OR sort_date < :before)"
                },
            )
        } else {
            (
                "sort_date DESC, t.account_id DESC, t.thread_id DESC",
                "cand.sort_date DESC, cand.account_id DESC, cand.thread_id DESC",
                if keyset {
                    "(sort_date, t.account_id, t.thread_id) < (:before, :ba, :bt)"
                } else {
                    "(:before IS NULL OR sort_date < :before)"
                },
            )
        };
        // Walking pays off for a plain list, where the newest messages are mostly
        // in it; with a category filter most of what it walks is filtered out.
        let walked = match f.walkable && cand > 0 && cat_filter == "1" {
            true => self.walk_candidates(q, f, now, cand as usize)?,
            false => None,
        };
        let cand_sql = match &walked {
            Some(_) => "cand AS MATERIALIZED (
               SELECT CAST(value ->> 0 AS TEXT) AS account_id, CAST(value ->> 1 AS TEXT) AS thread_id,
                 value ->> 2 AS tdate, value ->> 3 AS until, value ->> 4 AS sort_date
               FROM json_each(:cands)
             )"
            .to_string(),
            None => format!(
                "sel AS MATERIALIZED (
               SELECT DISTINCT m.account_id, m.thread_id FROM messages m WHERE {matches}
             ),
             cand AS MATERIALIZED (
               SELECT t.account_id, t.thread_id, t.tdate, s.until,
                 MAX(t.tdate, CASE WHEN s.until IS NOT NULL AND s.until <= :now THEN s.until ELSE 0 END) AS sort_date
               FROM (
                 SELECT m.account_id, m.thread_id,
                   COALESCE(MAX(CASE WHEN m.is_draft = 0 THEN m.date END), MAX(m.date)) AS tdate
                 FROM sel JOIN messages m ON m.account_id = sel.account_id AND m.thread_id = sel.thread_id
                 WHERE m.folder {scope} GROUP BY m.account_id, m.thread_id
               ) t
               LEFT JOIN snoozes s ON s.account_id = t.account_id AND s.thread_id = t.thread_id
               WHERE {snooze_filter} AND {cursor}
               ORDER BY {cand_order} LIMIT :cand
             )",
                matches = f.matches,
                scope = f.scope,
                snooze_filter = f.snooze_filter,
            ),
        };
        let sql = format!(
            "WITH {cand_sql},
             tm AS MATERIALIZED (
               SELECT m.seq, m.id, m.account_id, m.thread_id, m.message_id, m.folder, m.date, m.is_unread,
                 m.is_starred, m.is_draft, m.has_attachments, m.unsub_json, {ecat} AS ecat,
                 (m.folder = 'SENT' OR m.from_email = lower(a.email)) AS mine
               -- CROSS JOIN: look the few candidates up, never scan messages.
               FROM cand CROSS JOIN messages m ON m.account_id = cand.account_id AND m.thread_id = cand.thread_id
               LEFT JOIN sender_rules r ON r.email = m.from_email
               LEFT JOIN contacts c ON c.email = m.from_email
               LEFT JOIN accounts a ON a.id = m.account_id
               WHERE m.folder {scope}
             ),
             agg AS (
               SELECT account_id, thread_id,
                 COUNT(DISTINCT COALESCE(message_id, id)) AS n,
                 COUNT(DISTINCT CASE WHEN is_unread = 1 THEN COALESCE(message_id, id) END) AS unread,
                 MAX(is_starred) AS starred,
                 MAX(has_attachments) AS att, MAX(is_draft) AS has_draft,
                 group_concat(DISTINCT folder) AS folders, MAX(unsub_json) AS unsub,
                 MAX(ecat = 'primary' AND folder NOT IN ('SENT','DRAFTS')) AS any_primary,
                 MAX(mine) AS mine
               FROM tm GROUP BY account_id, thread_id
             ),
             latest AS (
               SELECT * FROM (
                 SELECT account_id, thread_id, id, ecat,
                   ROW_NUMBER() OVER (PARTITION BY account_id, thread_id ORDER BY is_draft ASC, date DESC, seq DESC) AS rn
                 FROM tm
               ) WHERE rn = 1
             )
             SELECT agg.account_id, agg.thread_id, lm.subject, lm.snippet, cand.tdate, agg.n, agg.unread, agg.starred,
                    agg.att, agg.has_draft, agg.folders, cand.until, l.id, agg.unsub, cand.sort_date,
                    {tcat} AS tcat, lm.from_email, (SELECT COUNT(*) FROM cand)
             FROM cand
             JOIN agg ON agg.account_id = cand.account_id AND agg.thread_id = cand.thread_id
             JOIN latest l ON l.account_id = cand.account_id AND l.thread_id = cand.thread_id
             JOIN messages lm ON lm.id = l.id
             WHERE {cat_filter}
             ORDER BY {order} LIMIT :limit",
            scope = f.scope,
            ecat = EFFECTIVE_CATEGORY,
            tcat = THREAD_CATEGORY,
        );

        let db = self.reader();
        let mut stmt = db.prepare_cached(&sql)?;
        let named: [(&str, rusqlite::types::Value); 7] = [
            (":now", now.into()),
            (":before", q.before.into()),
            (":ba", q.before_account_id.clone().into()),
            (":bt", q.before_thread_id.clone().into()),
            (":cand", cand.into()),
            (":limit", limit.into()),
            (":cands", walked.as_deref().map(WalkCand::json).into()),
        ];
        // Positional params (the filter's, unless the walk replaced it) first, then the named ones.
        if walked.is_none() {
            for (idx, p) in (1..).zip(f.params.iter()) {
                stmt.raw_bind_parameter(idx, p)?;
            }
        }
        for (name, v) in &named {
            if let Some(idx) = stmt.parameter_index(name)? {
                stmt.raw_bind_parameter(idx, v)?;
            }
        }

        let mut rows = stmt.raw_query();
        let mut out = Vec::new();
        let mut cand_n = walked.as_ref().map(|c| c.len() as i64);
        while let Some(r) = rows.next()? {
            cand_n = Some(r.get(17)?);
            out.push(thread_summary_from_row(r, now)?);
        }
        Ok((out, cand_n))
    }

    /// The first `cand` threads of a list in list order, found by walking
    /// messages newest first and stopping once no unseen thread can rank
    /// higher. `None` when that takes too many rows (a sparse folder): the
    /// SQL ranking over the folder's threads is cheaper then.
    fn walk_candidates(&self, q: &ThreadQuery, f: &ThreadFilter, now: i64, cand: usize) -> Result<Option<Vec<WalkCand>>> {
        use std::cmp::Reverse;
        use std::collections::{BinaryHeap, HashSet};
        let db = self.reader();
        let snoozes: HashMap<(String, String), i64> = {
            let mut stmt = db.prepare_cached("SELECT account_id, thread_id, until FROM snoozes")?;
            let rows = stmt.query_map([], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut in_list = db.prepare_cached(&format!(
            "SELECT EXISTS (SELECT 1 FROM messages m WHERE {} AND m.account_id = :acc AND m.thread_id = :thr)",
            f.matches
        ))?;
        for (idx, p) in (1..).zip(f.params.iter()) {
            in_list.raw_bind_parameter(idx, p)?;
        }
        let (acc_idx, thr_idx) = (in_list.parameter_index(":acc")?.context("acc")?, in_list.parameter_index(":thr")?.context("thr")?);
        let mut in_list = |a: &str, t: &str| -> Result<bool> {
            in_list.raw_bind_parameter(acc_idx, a)?;
            in_list.raw_bind_parameter(thr_idx, t)?;
            let mut rows = in_list.raw_query();
            Ok(match rows.next()? {
                Some(r) => r.get(0)?,
                None => false,
            })
        };
        let mut tdate_stmt = db.prepare_cached(&format!(
            "SELECT COALESCE(MAX(CASE WHEN is_draft = 0 THEN date END), MAX(date)) FROM messages
             WHERE account_id = ? AND thread_id = ? AND folder {}",
            f.scope
        ))?;
        let mut tdate = |a: &str, t: &str| -> Result<Option<i64>> { Ok(tdate_stmt.query_row(params![a, t], |r| r.get(0))?) };
        // Past the cursor (the same test as the SQL ranking's).
        let after_cursor = |c: &WalkCand| match (q.before, &q.before_account_id, &q.before_thread_id) {
            (Some(b), Some(ba), Some(bt)) => (c.sort_date, c.account_id.as_str(), c.thread_id.as_str()) < (b, ba, bt),
            (Some(b), _, _) => c.sort_date < b,
            _ => true,
        };

        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut found: Vec<WalkCand> = Vec::new();
        // The `cand` highest sort dates found so far.
        let mut top: BinaryHeap<Reverse<i64>> = BinaryHeap::new();
        let mut keep = |c: WalkCand, found: &mut Vec<WalkCand>| {
            top.push(Reverse(c.sort_date));
            if top.len() > cand {
                top.pop();
            }
            found.push(c);
            top.peek().map(|r| r.0).filter(|_| top.len() == cand)
        };
        let mut nth_best: Option<i64> = None;

        // A woken snooze ranks the thread by its wake-up time, however old its mail.
        for ((a, t), until) in snoozes.iter().filter(|(_, u)| **u <= now) {
            seen.insert((a.clone(), t.clone()));
            let Some(td) = tdate(a, t)? else { continue };
            let c = WalkCand { account_id: a.clone(), thread_id: t.clone(), tdate: td, until: Some(*until), sort_date: td.max(*until) };
            if after_cursor(&c) && in_list(a, t)? {
                nth_best = keep(c, &mut found);
            }
        }

        let mut walk = db.prepare_cached(&format!(
            "SELECT m.account_id, m.thread_id, m.date, m.is_draft FROM messages m INDEXED BY idx_messages_date
             WHERE {} AND m.folder {} ORDER BY m.date DESC",
            f.accounts, f.scope
        ))?;
        let mut rows = walk.query(params_from_iter(f.account_params.iter()))?;
        // A sparse list (an archive among inbox mail) is cheaper to rank in SQL.
        let budget = 5 * cand + 100;
        #[cfg(test)]
        let budget = tests::WALK_BUDGET.get().unwrap_or(budget);
        let mut walked = 0;
        while let Some(r) = rows.next()? {
            let date: i64 = r.get(2)?;
            // Threads not seen yet have no newer mail than this.
            if nth_best.is_some_and(|n| n > date.max(0)) {
                break;
            }
            // Rows newer than the cursor are cheap to pass over (later pages walk them again).
            if q.before.is_none_or(|b| date <= b) {
                walked += 1;
                if walked > budget {
                    return Ok(None);
                }
            }
            let key: (String, String) = (r.get(0)?, r.get(1)?);
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key.clone());
            let until = snoozes.get(&key).copied();
            if f.hide_snoozed && until.is_some_and(|u| u > now) {
                continue;
            }
            // The newest message decides, unless it's a draft.
            let td = if r.get::<_, bool>(3)? { tdate(&key.0, &key.1)?.unwrap_or(date) } else { date };
            let c = WalkCand { account_id: key.0, thread_id: key.1, tdate: td, until, sort_date: td.max(0) };
            if after_cursor(&c) && in_list(&c.account_id, &c.thread_id)? {
                nth_best = keep(c, &mut found);
            }
        }
        found.sort_by(|a, b| {
            (b.sort_date, &b.account_id, &b.thread_id).cmp(&(a.sort_date, &a.account_id, &a.thread_id))
        });
        found.truncate(cand);
        Ok(Some(found))
    }

    /// Participants, oldest first, from the messages that count towards each row.
    fn fill_participants(&self, mut out: Vec<ThreadSummary>, scope: &str) -> Result<Vec<ThreadSummary>> {
        let db = self.reader();
        let mut pstmt = db.prepare_cached(&format!(
            "SELECT from_name, from_email FROM messages WHERE account_id = ? AND thread_id = ? AND folder {scope} ORDER BY date"
        ))?;
        // "me" is the thread's own account, not any configured account.
        let own: HashMap<String, String> = {
            let mut s = db.prepare_cached("SELECT id, email FROM accounts")?;
            let rows = s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for t in &mut out {
            let me = own.get(&t.account_id).cloned().unwrap_or_default();
            let rows = pstmt.query_map(params![t.account_id, t.thread_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut names: Vec<String> = Vec::new();
            for row in rows {
                let (name, email) = row?;
                let display = if me.eq_ignore_ascii_case(&email) {
                    "me".to_string()
                } else {
                    EmailAddress { name, email }.display()
                };
                if !names.contains(&display) {
                    names.push(display);
                }
            }
            t.participants = names;
        }
        Ok(out)
    }

    /// `(account_id, thread_id, thread category, unread inbox messages)` for
    /// threads picked by `sel` (a `SELECT account_id, thread_id` over `messages m`).
    fn thread_categories(&self, sel: &str, params: &[rusqlite::types::Value], now: i64) -> Result<Vec<(String, String, String, i64)>> {
        Ok(self.thread_facts(sel, params, now)?.into_iter().map(|f| (f.account_id, f.thread_id, f.category, f.unread)).collect())
    }

    /// Smart Inbox and auto-clean facts about the (non-snoozed) threads picked by `sel`.
    fn thread_facts(&self, sel: &str, params: &[rusqlite::types::Value], now: i64) -> Result<Vec<ThreadFacts>> {
        let sql = format!(
            "WITH sel AS MATERIALIZED (
               SELECT sel.account_id, sel.thread_id FROM ({sel}) sel
               LEFT JOIN snoozes s ON s.account_id = sel.account_id AND s.thread_id = sel.thread_id
               WHERE s.until IS NULL OR s.until <= ?
             ),
             tm AS MATERIALIZED (
               SELECT m.seq, m.id, m.account_id, m.thread_id, m.message_id, m.folder, m.date, m.is_unread,
                 m.is_starred, m.is_draft, m.read_at, m.otp, {ecat} AS ecat,
                 (m.folder = 'SENT' OR m.from_email = lower(a.email)) AS mine
               FROM sel JOIN messages m ON m.account_id = sel.account_id AND m.thread_id = sel.thread_id
               LEFT JOIN sender_rules r ON r.email = m.from_email
               LEFT JOIN contacts c ON c.email = m.from_email
               LEFT JOIN accounts a ON a.id = m.account_id
               WHERE m.folder NOT IN ('TRASH','SPAM')
             ),
             agg AS (
               SELECT account_id, thread_id, MAX(is_starred) AS starred,
                 COUNT(DISTINCT CASE WHEN is_unread = 1 AND folder = 'INBOX' THEN COALESCE(message_id, id) END) AS unread,
                 MAX(ecat = 'primary' AND folder NOT IN ('SENT','DRAFTS')) AS any_primary,
                 MAX(mine) AS mine,
                 MAX(folder = 'INBOX') AS in_inbox,
                 MAX(CASE WHEN folder = 'INBOX' THEN read_at END) AS read_at,
                 MAX(folder = 'INBOX' AND is_unread = 0 AND read_at IS NULL) AS read_unknown
               FROM tm GROUP BY account_id, thread_id
             ),
             latest AS (
               SELECT account_id, thread_id, ecat, otp,
                 ROW_NUMBER() OVER (PARTITION BY account_id, thread_id ORDER BY is_draft ASC, date DESC, seq DESC) AS rn
               FROM tm
             )
             SELECT agg.account_id, agg.thread_id, {tcat}, agg.unread, agg.starred, agg.mine, COALESCE(l.otp, 0), agg.in_inbox,
                    agg.read_at, agg.read_unknown
             FROM agg
             JOIN latest l ON l.account_id = agg.account_id AND l.thread_id = agg.thread_id AND l.rn = 1",
            ecat = EFFECTIVE_CATEGORY,
            tcat = THREAD_CATEGORY,
        );
        let db = self.reader();
        let mut stmt = db.prepare_cached(&sql)?;
        let mut all: Vec<rusqlite::types::Value> = params.to_vec();
        all.push(rusqlite::types::Value::Integer(now));
        let rows = stmt.query_map(params_from_iter(all.iter()), |r| {
            Ok(ThreadFacts {
                account_id: r.get(0)?,
                thread_id: r.get(1)?,
                category: r.get(2)?,
                unread: r.get(3)?,
                starred: r.get(4)?,
                mine: r.get(5)?,
                otp: r.get(6)?,
                in_inbox: r.get(7)?,
                read_at: r.get(8)?,
                read_unknown: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Inbox conversations read between `since` and `read_before` that
    /// auto-clean may consider. Rescued and snoozed ones (also after they
    /// come back) are left out.
    pub fn autoclean_facts(&self, since: i64, read_before: i64, now: i64) -> Result<Vec<ThreadFacts>> {
        use rusqlite::types::Value as V;
        let sel = "SELECT DISTINCT m.account_id, m.thread_id FROM messages m
                   WHERE m.folder = 'INBOX' AND m.is_unread = 0 AND m.read_at >= ? AND m.read_at <= ?
                     AND NOT EXISTS (SELECT 1 FROM autoclean_exempt e WHERE e.account_id = m.account_id AND e.thread_id = m.thread_id)
                     AND NOT EXISTS (SELECT 1 FROM snoozes s WHERE s.account_id = m.account_id AND s.thread_id = m.thread_id)";
        self.thread_facts(sel, &[V::Integer(since), V::Integer(read_before)], now)
    }

    /// Inbox copies of a conversation (what auto-clean moves).
    pub fn inbox_message_ids(&self, account_id: &str, thread_id: &str) -> Result<Vec<String>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT id FROM messages WHERE account_id = ? AND thread_id = ? AND folder = 'INBOX'")?;
        let rows = stmt.query_map(params![account_id, thread_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Keep these conversations out of auto-clean from now on.
    pub fn autoclean_exempt(&self, threads: &[(String, String)]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        for (a, t) in threads {
            tx.execute("INSERT OR IGNORE INTO autoclean_exempt(account_id, thread_id) VALUES(?, ?)", params![a, t])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Messages whose one-time-code flag predates detection: `(id, subject, snippet)`.
    pub fn otp_unknown(&self, limit: i64) -> Result<Vec<(String, String, String)>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT id, subject, snippet FROM messages WHERE otp IS NULL LIMIT ?")?;
        let rows = stmt.query_map([limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_otp(&self, rows: &[(String, bool)]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        {
            let mut stmt = tx.prepare_cached("UPDATE messages SET otp = ? WHERE id = ?")?;
            for (id, otp) in rows {
                stmt.execute(params![otp, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Unread Inbox threads split into Smart Inbox's Important and Other.
    pub fn smart_unread_counts(&self, account_id: Option<&str>, now: i64) -> Result<SmartCounts> {
        let (filter, params) = match account_id {
            Some(a) => ("m.account_id = ?".to_string(), vec![rusqlite::types::Value::Text(a.to_string())]),
            None => (Self::unified_filter().to_string(), vec![]),
        };
        let sel = format!("SELECT DISTINCT m.account_id, m.thread_id FROM messages m WHERE {filter} AND m.folder = 'INBOX' AND m.is_unread = 1");
        let mut out = SmartCounts::default();
        for (_, _, cat, unread) in self.thread_categories(&sel, &params, now)? {
            if unread == 0 {
                continue;
            }
            if cat == "primary" {
                out.important += 1;
            } else {
                out.other += 1;
            }
        }
        Ok(out)
    }

    /// Smart Inbox bucket of one thread.
    pub fn thread_category(&self, account_id: &str, thread_id: &str, now: i64) -> Result<String> {
        use rusqlite::types::Value as V;
        let sel = "SELECT ? AS account_id, ? AS thread_id";
        let rows = self.thread_categories(sel, &[V::Text(account_id.into()), V::Text(thread_id.into())], now)?;
        Ok(rows.into_iter().next().map(|r| r.2).unwrap_or_else(|| "primary".into()))
    }

    /// "Always Important" / "always Other" for a sender; `None` clears it.
    pub fn set_sender_rule(&self, email: &str, category: Option<&str>) -> Result<()> {
        let email = email.trim().to_lowercase();
        match category {
            Some(c) => self.db().execute(
                "INSERT INTO sender_rules(email, category, updated_at) VALUES(?, ?, ?)
                 ON CONFLICT(email) DO UPDATE SET category = excluded.category, updated_at = excluded.updated_at",
                params![email, c, now_ms()],
            )?,
            None => self.db().execute("DELETE FROM sender_rules WHERE email = ?", [email])?,
        };
        Ok(())
    }

    pub fn sender_rules(&self) -> Result<Vec<SenderRule>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT email, category, updated_at FROM sender_rules ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], |r| Ok(SenderRule { email: r.get(0)?, category: r.get(1)?, updated_at: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Messages synced before Smart Inbox existed: `(id, start of raw message, from, subject, has_unsubscribe)`.
    #[allow(clippy::type_complexity)]
    pub fn uncategorized(&self, limit: i64) -> Result<Vec<(String, Option<Vec<u8>>, EmailAddress, String, bool)>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT m.id, substr(b.raw, 1, 65536), m.from_name, m.from_email, m.subject, m.unsub_json IS NOT NULL
             FROM messages m LEFT JOIN bodies b ON b.message_id = m.id
             WHERE m.category IS NULL ORDER BY m.date DESC LIMIT ?",
        )?;
        let rows = stmt.query_map([limit], |r| {
            Ok((r.get(0)?, r.get(1)?, EmailAddress { name: r.get(2)?, email: r.get(3)? }, r.get(4)?, r.get(5)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_categories(&self, rows: &[(String, &str)], ai: bool) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let sql = if ai { "UPDATE messages SET ai_category = ? WHERE id = ?" } else { "UPDATE messages SET category = ? WHERE id = ?" };
        {
            let mut stmt = tx.prepare_cached(sql)?;
            for (id, cat) in rows {
                stmt.execute(params![cat, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Forget the AI's sorting (when the user turns it off).
    pub fn clear_ai_categories(&self) -> Result<()> {
        self.db().execute("UPDATE messages SET ai_category = NULL WHERE ai_category IS NOT NULL", [])?;
        Ok(())
    }

    /// Recent Inbox mail the AI hasn't sorted yet, newest first.
    pub fn ai_unsorted(&self, since: i64, limit: i64) -> Result<Vec<AiSortCandidate>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT m.id, m.from_name, m.from_email, m.subject, m.snippet, COALESCE(m.category, 'primary'), m.unsub_json IS NOT NULL
             FROM messages m
             WHERE m.folder = 'INBOX' AND m.ai_category IS NULL AND m.date >= ?
               AND m.from_email NOT IN (SELECT lower(email) FROM accounts)
             ORDER BY m.date DESC LIMIT ?",
        )?;
        let rows = stmt.query_map(params![since, limit], |r| {
            Ok(AiSortCandidate {
                id: r.get(0)?,
                from: EmailAddress { name: r.get(1)?, email: r.get(2)? },
                subject: r.get(3)?,
                snippet: r.get(4)?,
                heuristic: r.get(5)?,
                bulk: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ------------------------------------------------------------------
    // Snooze
    // ------------------------------------------------------------------

    pub fn snooze(&self, account_id: &str, thread_id: &str, until: i64) -> Result<()> {
        self.db().execute(
            "INSERT INTO snoozes(account_id, thread_id, until, notified) VALUES(?,?,?,0)
             ON CONFLICT(account_id, thread_id) DO UPDATE SET until = excluded.until, notified = 0",
            params![account_id, thread_id, until],
        )?;
        Ok(())
    }

    pub fn unsnooze(&self, account_id: &str, thread_id: &str) -> Result<bool> {
        Ok(self
            .db()
            .execute("DELETE FROM snoozes WHERE account_id = ? AND thread_id = ?", params![account_id, thread_id])?
            > 0)
    }

    pub fn snoozed_until(&self, account_id: &str, thread_id: &str) -> Result<Option<i64>> {
        Ok(self
            .db()
            .query_row(
                "SELECT until FROM snoozes WHERE account_id = ? AND thread_id = ?",
                params![account_id, thread_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Snoozes whose time has come and that haven't been announced yet.
    pub fn due_snoozes(&self, now: i64) -> Result<Vec<(String, String, String)>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT s.account_id, s.thread_id,
               COALESCE((SELECT subject FROM messages m WHERE m.account_id = s.account_id AND m.thread_id = s.thread_id
                         ORDER BY date DESC LIMIT 1), '')
             FROM snoozes s WHERE s.until <= ? AND s.notified = 0",
        )?;
        let rows = stmt.query_map([now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn mark_snooze_notified(&self, account_id: &str, thread_id: &str) -> Result<()> {
        self.db().execute(
            "UPDATE snoozes SET notified = 1 WHERE account_id = ? AND thread_id = ?",
            params![account_id, thread_id],
        )?;
        Ok(())
    }

    pub fn next_snooze_wake(&self, now: i64) -> Result<Option<i64>> {
        Ok(self.db().query_row("SELECT MIN(until) FROM snoozes WHERE until > ?", [now], |r| r.get(0))?)
    }

    /// A reply arriving in a snoozed thread cancels the snooze (the
    /// conversation needs you again); woken snoozes older than 30 days are dropped.
    pub fn cancel_snooze_for_new_mail(&self, account_id: &str, thread_id: &str, now: i64) -> Result<bool> {
        let db = self.db();
        db.execute("DELETE FROM snoozes WHERE until <= ?", [now - 30 * 86_400_000])?;
        Ok(db.execute(
            "DELETE FROM snoozes WHERE account_id = ? AND thread_id = ? AND until > ?",
            params![account_id, thread_id, now],
        )? > 0)
    }

    // ------------------------------------------------------------------
    // Drafts
    // ------------------------------------------------------------------

    pub fn save_draft(&self, d: &Draft, server_message_id: Option<&str>, pending_send: bool) -> Result<()> {
        self.db().execute(
            "INSERT INTO drafts(id, account_id, json, thread_id, server_message_id, pending_send, updated_at)
             VALUES(?,?,?,?,?,?,?)
             ON CONFLICT(id) DO UPDATE SET account_id = excluded.account_id, json = excluded.json,
               thread_id = excluded.thread_id,
               server_message_id = COALESCE(excluded.server_message_id, server_message_id),
               pending_send = excluded.pending_send, updated_at = excluded.updated_at",
            params![d.id, d.account_id, to_json(d), d.thread_id, server_message_id, pending_send, d.updated_at],
        )?;
        Ok(())
    }

    pub fn draft(&self, id: &str) -> Result<Option<(Draft, Option<String>)>> {
        let row: Option<(String, Option<String>)> = self
            .db()
            .query_row("SELECT json, server_message_id FROM drafts WHERE id = ?", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        Ok(row.and_then(|(j, s)| serde_json::from_str(&j).ok().map(|d| (d, s))))
    }

    /// Local draft that owns a synced Drafts-folder message.
    pub fn draft_for_server_message(&self, message_row_id: &str) -> Result<Option<Draft>> {
        let j: Option<String> = self
            .db()
            .query_row("SELECT json FROM drafts WHERE server_message_id = ?", [message_row_id], |r| r.get(0))
            .optional()?;
        Ok(j.and_then(|j| serde_json::from_str(&j).ok()))
    }

    pub fn drafts(&self) -> Result<Vec<Draft>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT json FROM drafts WHERE pending_send = 0 ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).filter_map(|j| serde_json::from_str(&j).ok()).collect())
    }

    pub fn delete_draft(&self, id: &str) -> Result<Option<String>> {
        let db = self.db();
        let server: Option<Option<String>> = db
            .query_row("SELECT server_message_id FROM drafts WHERE id = ?", [id], |r| r.get(0))
            .optional()?;
        db.execute("DELETE FROM drafts WHERE id = ?", [id])?;
        Ok(server.flatten())
    }

    pub fn set_draft_pending(&self, id: &str, pending: bool) -> Result<()> {
        self.db().execute("UPDATE drafts SET pending_send = ? WHERE id = ?", params![pending, id])?;
        Ok(())
    }

    /// Record the draft's copy in the server's Drafts folder, leaving its
    /// content alone. False when the draft no longer exists.
    pub fn set_draft_server_copy(&self, id: &str, server_message_id: Option<&str>) -> Result<bool> {
        Ok(self.db().execute(
            "UPDATE drafts SET server_message_id = COALESCE(?, server_message_id) WHERE id = ?",
            params![server_message_id, id],
        )? > 0)
    }

    // ------------------------------------------------------------------
    // Signatures & templates
    // ------------------------------------------------------------------

    pub fn signatures(&self) -> Result<Vec<Signature>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT id, account_id, name, html, is_default FROM signatures ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| {
            Ok(Signature { id: r.get(0)?, account_id: r.get(1)?, name: r.get(2)?, html: r.get(3)?, is_default: r.get(4)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_signature(&self, s: &Signature) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let dup: Option<String> = tx
            .query_row(
                "SELECT id FROM signatures WHERE COALESCE(account_id,'') = COALESCE(?,'')
                   AND lower(trim(name)) = lower(trim(?)) AND id != ?",
                params![s.account_id, s.name, s.id],
                |r| r.get(0),
            )
            .optional()?;
        if dup.is_some() {
            anyhow::bail!("This account already has a signature with that name");
        }
        if s.is_default {
            tx.execute(
                "UPDATE signatures SET is_default = 0 WHERE COALESCE(account_id,'') = COALESCE(?,'')",
                params![s.account_id],
            )?;
        }
        tx.execute(
            "INSERT INTO signatures(id, account_id, name, html, is_default, updated_at) VALUES(?,?,?,?,?,?)
             ON CONFLICT(id) DO UPDATE SET account_id = excluded.account_id, name = excluded.name, html = excluded.html,
               is_default = excluded.is_default, updated_at = excluded.updated_at",
            params![s.id, s.account_id, s.name.trim(), s.html, s.is_default, now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_signature(&self, id: &str) -> Result<()> {
        self.db().execute("DELETE FROM signatures WHERE id = ?", [id])?;
        Ok(())
    }

    /// Default signature for an account (falls back to a global default).
    pub fn default_signature(&self, account_id: &str) -> Result<Option<Signature>> {
        Ok(self
            .signatures()?
            .into_iter()
            .filter(|s| s.is_default && (s.account_id.is_none() || s.account_id.as_deref() == Some(account_id)))
            .max_by_key(|s| s.account_id.is_some()))
    }

    pub fn templates(&self) -> Result<Vec<Template>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT id, name, subject, html, updated_at FROM templates ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| {
            Ok(Template { id: r.get(0)?, name: r.get(1)?, subject: r.get(2)?, html: r.get(3)?, updated_at: r.get(4)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_template(&self, t: &Template) -> Result<()> {
        self.db().execute(
            "INSERT INTO templates(id, name, subject, html, updated_at) VALUES(?,?,?,?,?)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, subject = excluded.subject, html = excluded.html,
               updated_at = excluded.updated_at",
            params![t.id, t.name.trim(), t.subject, t.html, now_ms()],
        )?;
        Ok(())
    }

    pub fn delete_template(&self, id: &str) -> Result<()> {
        self.db().execute("DELETE FROM templates WHERE id = ?", [id])?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Outbox (undo send + send later)
    // ------------------------------------------------------------------

    pub fn enqueue_outbox(&self, item: &OutboxItem) -> Result<()> {
        self.db().execute(
            "INSERT INTO outbox(id, account_id, kind, draft_json, send_at, status, attempts, last_error, created_at)
             VALUES(?,?,?,?,?,?,?,?,?)
             ON CONFLICT(id) DO UPDATE SET account_id = excluded.account_id, kind = excluded.kind,
               draft_json = excluded.draft_json, send_at = excluded.send_at, status = excluded.status,
               attempts = excluded.attempts, last_error = excluded.last_error",
            params![
                item.id,
                item.account_id,
                if item.kind == OutboxKind::Undo { "undo" } else { "scheduled" },
                to_json(&item.draft),
                item.send_at,
                item.status.as_str(),
                item.attempts,
                item.last_error,
                item.created_at
            ],
        )?;
        Ok(())
    }

    fn outbox_from_row(r: &Row<'_>) -> rusqlite::Result<OutboxItem> {
        Ok(OutboxItem {
            id: r.get(0)?,
            account_id: r.get(1)?,
            kind: if r.get::<_, String>(2)? == "undo" { OutboxKind::Undo } else { OutboxKind::Scheduled },
            draft: serde_json::from_str(&r.get::<_, String>(3)?).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
            })?,
            send_at: r.get(4)?,
            status: OutboxStatus::parse(&r.get::<_, String>(5)?),
            attempts: r.get(6)?,
            last_error: r.get(7)?,
            created_at: r.get(8)?,
        })
    }

    pub fn outbox(&self) -> Result<Vec<OutboxItem>> {
        let db = self.reader();
        let mut stmt = db.prepare(
            "SELECT id, account_id, kind, draft_json, send_at, status, attempts, last_error, created_at FROM outbox ORDER BY send_at",
        )?;
        let rows = stmt.query_map([], Self::outbox_from_row)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// `(id, send_at)` of items waiting to be sent, soonest first, without
    /// loading their drafts.
    pub fn outbox_schedule(&self) -> Result<Vec<(String, i64)>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT id, send_at FROM outbox WHERE status = 'pending' ORDER BY send_at")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn outbox_item(&self, id: &str) -> Result<Option<OutboxItem>> {
        Ok(self
            .reader()
            .query_row(
                "SELECT id, account_id, kind, draft_json, send_at, status, attempts, last_error, created_at FROM outbox WHERE id = ?",
                [id],
                Self::outbox_from_row,
            )
            .optional()?)
    }

    /// Atomically claim an item for sending. Returns false when it was
    /// cancelled or already claimed.
    pub fn claim_outbox(&self, id: &str) -> Result<bool> {
        Ok(self.db().execute(
            "UPDATE outbox SET status = 'sending', attempts = attempts + 1 WHERE id = ? AND status = 'pending'",
            [id],
        )? > 0)
    }

    pub fn fail_outbox(&self, id: &str, error: &str, retry_at: Option<i64>) -> Result<()> {
        match retry_at {
            Some(at) => self.db().execute(
                "UPDATE outbox SET status = 'pending', send_at = ?, last_error = ? WHERE id = ?",
                params![at, error, id],
            )?,
            None => self.db().execute(
                "UPDATE outbox SET status = 'failed', last_error = ? WHERE id = ?",
                params![error, id],
            )?,
        };
        Ok(())
    }

    /// Remove a pending item. Returns the item if it was still cancellable.
    pub fn take_outbox(&self, id: &str) -> Result<Option<OutboxItem>> {
        let mut db = self.db();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let item = tx
            .query_row(
                "SELECT id, account_id, kind, draft_json, send_at, status, attempts, last_error, created_at FROM outbox WHERE id = ?",
                [id],
                Self::outbox_from_row,
            )
            .optional()?;
        // Nothing deleted: it's being sent (or already gone).
        let deleted = tx.execute("DELETE FROM outbox WHERE id = ? AND status != 'sending'", [id])?;
        tx.commit()?;
        Ok(item.filter(|_| deleted > 0))
    }

    pub fn delete_outbox(&self, id: &str) -> Result<()> {
        self.db().execute("DELETE FROM outbox WHERE id = ?", [id])?;
        Ok(())
    }

    pub fn reschedule_outbox(&self, id: &str, send_at: i64) -> Result<bool> {
        Ok(self.db().execute(
            "UPDATE outbox SET send_at = ?, status = 'pending', last_error = NULL WHERE id = ? AND status != 'sending'",
            params![send_at, id],
        )? > 0)
    }

    /// Items stuck in `sending` (the app died mid-send) go back to pending.
    pub fn reset_stuck_outbox(&self) -> Result<()> {
        self.db().execute("UPDATE outbox SET status = 'pending' WHERE status = 'sending'", [])?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Server op queue
    // ------------------------------------------------------------------

    pub fn enqueue_op(&self, account_id: &str, op_json: &str) -> Result<i64> {
        let db = self.db();
        db.execute(
            "INSERT INTO op_queue(account_id, op_json, next_attempt_at, created_at) VALUES(?,?,?,?)",
            params![account_id, op_json, 0, now_ms()],
        )?;
        Ok(db.last_insert_rowid())
    }

    /// Ops ready to run, in order. An op waiting to retry holds back the
    /// ones queued after it, so the server ends up as the user's last action left it.
    pub fn due_ops(&self, account_id: &str, now: i64) -> Result<Vec<QueuedOp>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT id, account_id, op_json, attempts FROM op_queue
             WHERE account_id = ?1 AND id < COALESCE(
               (SELECT MIN(id) FROM op_queue WHERE account_id = ?1 AND next_attempt_at > ?2), 9223372036854775807)
             ORDER BY id",
        )?;
        let rows = stmt.query_map(params![account_id, now], |r| {
            Ok(QueuedOp { id: r.get(0)?, account_id: r.get(1)?, op_json: r.get(2)?, attempts: r.get(3)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn pending_op_count(&self, account_id: &str) -> Result<i64> {
        Ok(self.db().query_row("SELECT COUNT(*) FROM op_queue WHERE account_id = ?", [account_id], |r| r.get(0))?)
    }

    /// Message row ids whose queued flag changes (read, star, answered) have
    /// not reached the server yet.
    pub fn pending_flag_message_ids(&self, account_id: &str) -> Result<Vec<String>> {
        let db = self.db();
        let mut stmt = db.prepare(r#"SELECT op_json FROM op_queue WHERE account_id = ? AND op_json LIKE '{"kind":"flags"%'"#)?;
        let rows = stmt.query_map([account_id], |r| r.get::<_, String>(0))?;
        let mut ids = Vec::new();
        for json in rows {
            let op: serde_json::Value = serde_json::from_str(&json?).unwrap_or_default();
            if let Some(list) = op.get("messageIds").and_then(|v| v.as_array()) {
                ids.extend(list.iter().filter_map(|v| v.as_str()).map(String::from));
            }
        }
        Ok(ids)
    }

    pub fn finish_op(&self, id: i64) -> Result<()> {
        self.db().execute("DELETE FROM op_queue WHERE id = ?", [id])?;
        Ok(())
    }

    pub fn retry_op(&self, id: i64, next_attempt_at: i64, error: &str) -> Result<()> {
        self.db().execute(
            "UPDATE op_queue SET attempts = attempts + 1, next_attempt_at = ?, last_error = ? WHERE id = ?",
            params![next_attempt_at, error, id],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Contacts & newsletters
    // ------------------------------------------------------------------

    pub fn suggest_contacts(&self, prefix: &str, limit: i64) -> Result<Vec<Contact>> {
        let escaped = prefix.trim().to_lowercase().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        let like = format!("%{escaped}%");
        let db = self.reader();
        let mut stmt = db.prepare_cached(
            "SELECT email, name, frequency, last_seen FROM contacts
             WHERE lower(email) LIKE ?1 ESCAPE '\\' OR lower(name) LIKE ?1 ESCAPE '\\'
             ORDER BY frequency DESC, last_seen DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![like, limit], |r| {
            Ok(Contact { email: r.get(0)?, name: r.get(1)?, frequency: r.get(2)?, last_seen: r.get(3)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Senders with a `List-Unsubscribe` header and at least two messages.
    pub fn newsletter_senders(&self, account_id: Option<&str>, limit: i64) -> Result<Vec<NewsletterSender>> {
        let db = self.reader();
        let (filter, p): (&str, Vec<String>) = match account_id {
            Some(a) => ("m.account_id = ?", vec![a.to_string()]),
            None => ("1 = 1", vec![]),
        };
        let sql = format!(
            "SELECT m.account_id, m.from_email, MAX(m.from_name), COUNT(*), SUM(m.is_unread), MAX(m.date),
               (SELECT id FROM messages x WHERE x.account_id = m.account_id AND x.from_email = m.from_email
                  AND x.unsub_json IS NOT NULL ORDER BY date DESC LIMIT 1),
               (SELECT unsub_json FROM messages x WHERE x.account_id = m.account_id AND x.from_email = m.from_email
                  AND x.unsub_json IS NOT NULL ORDER BY date DESC LIMIT 1)
             FROM messages m
             WHERE {filter} AND m.from_email != '' AND m.folder NOT IN ('SENT','DRAFTS','TRASH')
             GROUP BY m.account_id, m.from_email
             HAVING COUNT(*) >= 2 AND MAX(m.unsub_json IS NOT NULL)
             ORDER BY COUNT(*) DESC LIMIT {limit}"
        );
        let mut stmt = db.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(p.iter()), |r| {
            let unsub: Option<String> = r.get(7)?;
            Ok(NewsletterSender {
                account_id: r.get(0)?,
                sender: EmailAddress { email: r.get(1)?, name: r.get::<_, Option<String>>(2)?.unwrap_or_default() },
                message_count: r.get(3)?,
                unread_count: r.get(4)?,
                last_date: r.get(5)?,
                latest_message_id: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                unsubscribe: unsub.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).filter(|n| n.unsubscribe.is_actionable()).collect())
    }

    /// New, unread INBOX messages not yet announced (for notifications).
    pub fn messages_by_ids(&self, ids: &[String]) -> Result<Vec<MessageMeta>> {
        let db = self.reader();
        let mut out = Vec::new();
        for id in ids {
            if let Some(m) = Self::message_meta_on(&db, id)? {
                out.push(m);
            }
        }
        Ok(out)
    }
}

/// The messages that put a thread in a list, as SQL over `messages m`.
/// A message `reindex_fts_batch` indexes again.
struct FtsPending {
    seq: i64,
    id: String,
    text: (String, String, String),
    raw: Option<Vec<u8>>,
    had_body: bool,
}

struct ThreadFilter {
    matches: String,
    params: Vec<rusqlite::types::Value>,
    /// The account part of `matches`, and its parameters.
    accounts: String,
    account_params: Vec<rusqlite::types::Value>,
    snoozed: bool,
    /// Not snoozed now (`snooze_filter` is the SQL for it).
    hide_snoozed: bool,
    /// Ranked by date and cheap to test per thread (no search), so
    /// `walk_candidates` can find the first threads.
    walkable: bool,
    /// Condition on `folder` for the messages that count towards a row.
    scope: &'static str,
    /// Condition on the thread's snooze row `s`.
    snooze_filter: &'static str,
}

/// A thread `walk_candidates` picked, with its ranking.
struct WalkCand {
    account_id: String,
    thread_id: String,
    tdate: i64,
    until: Option<i64>,
    sort_date: i64,
}

impl WalkCand {
    /// The `json_each` rows the list query reads them from.
    fn json(cands: &[WalkCand]) -> String {
        let rows: Vec<_> =
            cands.iter().map(|c| serde_json::json!([c.account_id, c.thread_id, c.tdate, c.until, c.sort_date])).collect();
        serde_json::Value::Array(rows).to_string()
    }
}

/// A `list_threads` result row (columns 0–16 of its query).
fn thread_summary_from_row(r: &Row<'_>, now: i64) -> rusqlite::Result<ThreadSummary> {
    let starred: bool = r.get(7)?;
    let folders: String = r.get::<_, Option<String>>(10)?.unwrap_or_default();
    let mut labels: Vec<String> = folders.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
    if starred {
        labels.push(labels::STARRED.into());
    }
    let snoozed: Option<i64> = r.get(11)?;
    Ok(ThreadSummary {
        account_id: r.get(0)?,
        thread_id: r.get(1)?,
        subject: r.get(2)?,
        participants: Vec::new(),
        snippet: r.get(3)?,
        date: r.get(14)?,
        message_count: r.get(5)?,
        unread_count: r.get(6)?,
        is_starred: starred,
        has_attachments: r.get(8)?,
        has_draft: r.get(9)?,
        labels,
        snoozed_until: snoozed.filter(|u| *u > now),
        latest_message_id: r.get(12)?,
        unsubscribe: r.get::<_, Option<String>>(13)?.and_then(|s| serde_json::from_str(&s).ok()),
        category: r.get(15)?,
        sender_email: r.get(16)?,
    })
}

fn location_from_row(r: &Row<'_>) -> rusqlite::Result<Location> {
    Ok(Location {
        id: r.get(0)?,
        account_id: r.get(1)?,
        thread_id: r.get(2)?,
        folder: r.get(3)?,
        path: r.get(4)?,
        uid_validity: r.get::<_, i64>(5)? as u32,
        uid: r.get::<_, i64>(6)? as u32,
        message_id: r.get(7)?,
        is_unread: r.get(8)?,
        is_starred: r.get(9)?,
    })
}

fn index_fts(tx: &rusqlite::Transaction<'_>, seq: i64, p: &ParsedMessage) -> Result<()> {
    let (subject, participants, body) = fts_text(p);
    index_fts_text(tx, seq, &subject, &participants, &body)
}

/// `(subject, participants, body)` as indexed.
fn fts_text(p: &ParsedMessage) -> (String, String, String) {
    let body = match (&p.text, &p.html) {
        (Some(t), _) if !t.trim().is_empty() => t.clone(),
        (_, Some(h)) => crate::mime::html_to_text(h),
        _ => p.snippet.clone(),
    };
    (p.subject.clone(), fts_participants(&p.from, &p.to, &p.cc), body)
}

fn fts_participants(from: &EmailAddress, to: &[EmailAddress], cc: &[EmailAddress]) -> String {
    std::iter::once(from).chain(to).chain(cc).map(|a| format!("{} {}", a.name, a.email)).collect::<Vec<_>>().join(" ")
}

fn index_fts_text(tx: &rusqlite::Transaction<'_>, seq: i64, subject: &str, participants: &str, body: &str) -> Result<()> {
    use search::normalize_ar as n;
    tx.prepare_cached("DELETE FROM message_fts WHERE rowid = ?")?.execute([seq])?;
    tx.prepare_cached("INSERT INTO message_fts(rowid, subject, participants, body) VALUES(?,?,?,?)")?
        .execute(params![seq, n(subject), n(participants), n(body)])?;
    Ok(())
}

fn upsert_contacts(tx: &rusqlite::Transaction<'_>, m: &IncomingMessage) -> Result<()> {
    let p = &m.parsed;
    // Mail we sent says more about who we talk to than mail we receive.
    let (people, weight): (Vec<&EmailAddress>, i64) = if m.folder == labels::SENT {
        (p.to.iter().chain(p.cc.iter()).chain(p.bcc.iter()).collect(), 3)
    } else {
        (vec![&p.from], 1)
    };
    let sent = i64::from(m.folder == labels::SENT);
    let mut stmt = tx.prepare_cached(
        "INSERT INTO contacts(email, name, frequency, last_seen, sent_count) VALUES(lower(?), ?, ?, ?, ?)
         ON CONFLICT(email) DO UPDATE SET frequency = frequency + excluded.frequency,
           name = CASE WHEN excluded.name != '' THEN excluded.name ELSE name END,
           last_seen = MAX(last_seen, excluded.last_seen),
           sent_count = sent_count + excluded.sent_count",
    )?;
    for a in people {
        if !a.email.contains('@') {
            continue;
        }
        stmt.execute(params![a.email, a.name, weight, m.date, sent])?;
    }
    Ok(())
}

/// Sidebar order for system labels.
fn label_rank(id: &str) -> u8 {
    match id {
        labels::INBOX => 0,
        labels::STARRED => 1,
        labels::DRAFTS => 3,
        labels::SENT => 4,
        labels::ARCHIVE => 5,
        labels::ALL => 6,
        labels::SPAM => 7,
        labels::TRASH => 8,
        _ => 20,
    }
}

/// `in:sent`, `in:trash`, … → label id. Anything else is taken as a folder label id.
pub fn resolve_label_name(name: &str) -> String {
    match name.to_lowercase().as_str() {
        "inbox" => labels::INBOX.into(),
        "sent" | "sent mail" => labels::SENT.into(),
        "draft" | "drafts" => labels::DRAFTS.into(),
        "trash" | "bin" => labels::TRASH.into(),
        "spam" | "junk" => labels::SPAM.into(),
        "archive" | "archived" => labels::ARCHIVE.into(),
        "starred" => labels::STARRED.into(),
        "unread" => labels::UNREAD.into(),
        "snoozed" => labels::SNOOZED.into(),
        "all" | "anywhere" => labels::ALL.into(),
        _ => name.to_string(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    thread_local! {
        /// Overrides `walk_candidates`' row budget (0: always rank in SQL).
        pub(super) static WALK_BUDGET: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    }

    pub(crate) fn account(store: &Store, email: &str) -> Account {
        let srv = ServerConfig { host: "127.0.0.1".into(), port: 1, security: Security::None };
        let a = NewAccount {
            email: email.into(),
            display_name: "Me".into(),
            username: String::new(),
            password: "x".into(),
            imap: srv.clone(),
            smtp: srv,
            allow_invalid_certs: false,
        };
        store.insert_account(&a, false, "password").unwrap()
    }

    pub(crate) fn raw_message(from: &str, to: &str, subject: &str, extra_headers: &str, msg_id: &str) -> Vec<u8> {
        format!(
            "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nMessage-ID: <{msg_id}>\r\nDate: Mon, 19 Oct 2026 09:00:00 +0000\r\n{extra_headers}\
             Content-Type: text/plain; charset=utf-8\r\n\r\nHello there, this is {subject}.\r\n"
        )
        .into_bytes()
    }

    pub(crate) fn put(store: &Store, acc: &Account, folder: &str, uid: u32, raw: Vec<u8>, date: i64) -> String {
        let parsed = crate::mime::parse(&raw).unwrap();
        let thread_id = format!("t:{}", parsed.message_id.clone().unwrap());
        store
            .upsert_message(&IncomingMessage {
                account_id: acc.id.clone(),
                thread_id,
                folder: folder.into(),
                path: folder.into(),
                uid_validity: 1,
                uid,
                date,
                is_unread: true,
                is_starred: false,
                is_draft: false,
                is_answered: false,
                parsed,
                raw: Some(raw),
            })
            .unwrap()
            .0
    }

    fn inbox(store: &Store, category: Option<&str>) -> Vec<String> {
        let q = ThreadQuery {
            account_id: None,
            label: labels::INBOX.into(),
            unread_only: false,
            search: None,
            limit: 50,
            before: None,
            before_account_id: None,
            before_thread_id: None,
            category: category.map(String::from),
        };
        store.list_threads(&q, now_ms()).unwrap().into_iter().map(|t| t.subject).collect()
    }

    #[test]
    fn smart_inbox_buckets() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        put(&store, &me, "INBOX", 1, raw_message("Sara <sara@acme.com>", "me@example.com", "Lunch?", "", "a@x"), 1000);
        let bulk = "List-Unsubscribe: <https://shop.test/u>\r\nX-Mailchimp-Campaign: 1\r\n";
        put(&store, &me, "INBOX", 2, raw_message("Shop <news@shop.test>", "me@example.com", "50% off", bulk, "b@x"), 2000);
        put(&store, &me, "INBOX", 3, raw_message("Bank <no-reply@bank.test>", "me@example.com", "Your statement", "", "c@x"), 3000);

        assert_eq!(inbox(&store, None).len(), 3);
        assert_eq!(inbox(&store, Some("important")), vec!["Lunch?"]);
        assert_eq!(inbox(&store, Some("other")), vec!["Your statement", "50% off"]);
        assert_eq!(inbox(&store, Some("promotions")), vec!["50% off"]);
        let counts = store.smart_unread_counts(None, now_ms()).unwrap();
        assert_eq!((counts.important, counts.other), (1, 2));

        // A sender rule wins over the heuristic.
        store.set_sender_rule("news@shop.test", Some("primary")).unwrap();
        assert_eq!(inbox(&store, Some("important")), vec!["50% off", "Lunch?"]);
        store.set_sender_rule("news@shop.test", None).unwrap();

        // People we write to are Important.
        put(&store, &me, "SENT", 1, raw_message("Me <me@example.com>", "no-reply@bank.test", "Question", "", "d@x"), 500);
        assert_eq!(inbox(&store, Some("important")), vec!["Your statement", "Lunch?"]);
        assert_eq!(store.thread_category(&me.id, "t:b@x", now_ms()).unwrap(), "promotions");
    }

    #[test]
    fn upgrades_v1_database() {
        let path = std::env::temp_dir().join(format!("remail-v1-{}.db", uuid::Uuid::new_v4().simple()));
        {
            let conn = Connection::open(&path).unwrap();
            let v1 = SCHEMA
                .replace(",\n  -- Smart Inbox bucket from headers (classify.rs), and the AI's opinion.\n  category TEXT,\n  ai_category TEXT,\n  -- When the message became read (auto-clean), and whether it's a one-time code.\n  read_at INTEGER,\n  otp INTEGER", "")
                .replace(",\n  -- Messages we sent to this address (people we write to are Important).\n  sent_count INTEGER NOT NULL DEFAULT 0", "");
            let v1 = &v1[..v1.find("-- Conversations the user rescued").unwrap()];
            let v1 = v1.replace("CREATE INDEX IF NOT EXISTS idx_messages_read_at ON messages(folder, is_unread, read_at);\n", "")
                .replace("CREATE INDEX IF NOT EXISTS idx_messages_otp_unknown ON messages(otp) WHERE otp IS NULL;\n", "")
                .replace("CREATE INDEX IF NOT EXISTS idx_messages_uncategorized ON messages(date DESC) WHERE category IS NULL;\n", "");
            assert!(!v1.contains("category") && !v1.contains("sent_count") && !v1.contains("read_at") && !v1.contains("otp"), "{v1}");
            conn.execute_batch(&v1).unwrap();
            conn.execute_batch("PRAGMA user_version = 1").unwrap();
            conn.execute(
                "INSERT INTO accounts(id, email, username, imap_host, imap_port, imap_security, smtp_host, smtp_port, smtp_security, color, created_at)
                 VALUES('a', 'me@example.com', 'me', 'h', 1, 'none', 'h', 1, 'none', '#000', 0)",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO contacts(email, name, frequency, last_seen) VALUES('bob@x.test', 'Bob', 3, 0)", []).unwrap();
            conn.execute(
                "INSERT INTO messages(id, account_id, thread_id, folder, path, uid_validity, uid, to_json, date, created_at)
                 VALUES('m1', 'a', 't1', 'SENT', 'Sent', 1, 1, '[{\"name\":\"Bob\",\"email\":\"Bob@x.test\"}]', 0, 0)",
                [],
            )
            .unwrap();
        }
        {
            // An earlier upgrade that died after adding one column.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("ALTER TABLE messages ADD COLUMN category TEXT").unwrap();
        }
        let store = Store::open(&path).unwrap();
        let sent: i64 = store.db().query_row("SELECT sent_count FROM contacts WHERE email = 'bob@x.test'", [], |r| r.get(0)).unwrap();
        assert_eq!(sent, 1);
        assert_eq!(store.uncategorized(10).unwrap().len(), 1);
        assert_eq!(store.otp_unknown(10).unwrap().len(), 1);
        for col in ["read_at", "otp", "ai_category"] {
            assert!(has_column(&store.db(), "messages", col).unwrap(), "{col}");
        }
        let v: i64 = store.db().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    /// One message of a test mailbox.
    struct Msg<'a> {
        folder: &'a str,
        uid: u32,
        thread: &'a str,
        msg_id: &'a str,
        from: &'a str,
        subject: &'a str,
        extra: &'a str,
        date: i64,
        unread: bool,
        starred: bool,
        draft: bool,
    }

    fn put_msg(store: &Store, acc: &Account, m: &Msg) -> String {
        let raw = raw_message(m.from, &acc.email, m.subject, m.extra, m.msg_id);
        let parsed = crate::mime::parse(&raw).unwrap();
        store
            .upsert_message(&IncomingMessage {
                account_id: acc.id.clone(),
                thread_id: m.thread.into(),
                folder: m.folder.into(),
                path: m.folder.into(),
                uid_validity: 1,
                uid: m.uid,
                date: m.date,
                is_unread: m.unread,
                is_starred: m.starred,
                is_draft: m.draft,
                is_answered: false,
                parsed,
                raw: Some(raw),
            })
            .unwrap()
            .0
    }

    fn query(account_id: Option<&str>, label: &str, search: Option<&str>, category: Option<&str>, limit: i64) -> ThreadQuery {
        ThreadQuery {
            account_id: account_id.map(String::from),
            label: label.into(),
            unread_only: false,
            search: search.map(String::from),
            limit,
            before: None,
            before_account_id: None,
            before_thread_id: None,
            category: category.map(String::from),
        }
    }

    /// Three accounts (one hidden from the unified view), threads spread over
    /// every kind of folder with tied dates, duplicate Message-IDs, drafts,
    /// woken and pending snoozes, sender rules and stars.
    fn varied_mailbox(store: &Store, now: i64) -> Vec<Account> {
        let accs = vec![account(store, "me@a.test"), account(store, "me@b.test"), account(store, "me@c.test")];
        store.update_account_prefs(&accs[2].id, None, None, Some(true), None).unwrap();
        for a in &accs {
            let work = FolderRow {
                label_id: "Work".into(),
                path: "Work".into(),
                name: "Work".into(),
                kind: LabelKind::User,
                parent_id: None,
                delimiter: Some("/".into()),
                synced: true,
            };
            store.replace_folders(&a.id, &[work]).unwrap();
        }
        let mut seed: u64 = 7;
        let mut rnd = |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let folders = ["INBOX", "INBOX", "INBOX", "INBOX", "ARCHIVE", "SENT", "DRAFTS", "TRASH", "SPAM", "Work"];
        let senders = ["Sara <sara@acme.test>", "Bob <bob@x.test>", "Shop <news@shop.test>", "Bank <no-reply@bank.test>", ""];
        let subjects = ["Invoice 42", "Q3 - report", "Lunch?", "إجتماع الفريق", "meeting notes", "شُكْرًا"];
        let bulk = "List-Unsubscribe: <https://shop.test/u>\r\nX-Mailchimp-Campaign: 1\r\n";
        let mut uid = 0;
        for t in 0..160 {
            let acc = &accs[rnd(3) as usize];
            let thread = format!("t{t}");
            let old = rnd(10) == 0;
            for k in 0..1 + rnd(4) {
                uid += 1;
                let folder = folders[rnd(folders.len() as u64) as usize];
                // Some copies share a Message-ID with the thread's first message.
                let msg_id = if k > 0 && rnd(6) == 0 { format!("m{t}-0@x") } else { format!("m{t}-{k}@x") };
                let s = rnd(senders.len() as u64) as usize;
                let from = if senders[s].is_empty() { format!("Me <{}>", acc.email) } else { senders[s].to_string() };
                let date = now - rnd(40) as i64 * 60_000 - if old { 400 * 86_400_000 } else { 0 };
                put_msg(
                    store,
                    acc,
                    &Msg {
                        folder,
                        uid,
                        thread: &thread,
                        msg_id: &msg_id,
                        from: &from,
                        subject: subjects[rnd(subjects.len() as u64) as usize],
                        extra: if s == 2 { bulk } else { "" },
                        date,
                        unread: rnd(3) == 0,
                        starred: rnd(8) == 0,
                        draft: folder == "DRAFTS" || rnd(15) == 0,
                    },
                );
            }
            if t % 9 == 0 {
                // Woken, sometimes later than the thread's newest mail.
                store.snooze(&acc.id, &thread, now - rnd(60) as i64 * 60_000).unwrap();
            } else if t % 7 == 0 {
                store.snooze(&acc.id, &thread, now + (1 + rnd(3)) as i64 * 3_600_000).unwrap();
            }
        }
        store.set_sender_rule("bob@x.test", Some("promotions")).unwrap();
        store.set_sender_rule("no-reply@bank.test", Some("primary")).unwrap();
        accs
    }

    /// `list_threads` as it was before candidate ranking (aggregating every
    /// thread in the view), plus a tie-break so the order is defined; without
    /// the cursor and limit (see `oracle_page`).
    fn list_threads_oracle(store: &Store, q: &ThreadQuery, now: i64) -> Vec<ThreadSummary> {
        let f = store.thread_filter(q, now);
        let order = if f.snoozed {
            "s.until ASC, agg.account_id ASC, agg.thread_id ASC"
        } else {
            "sort_date DESC, agg.account_id DESC, agg.thread_id DESC"
        };
        let sql = format!(
            "WITH sel AS (
               SELECT DISTINCT m.account_id, m.thread_id FROM messages m WHERE {matches}
             ),
             tm AS (
               SELECT m.*, {ecat} AS ecat
               FROM messages m JOIN sel ON m.account_id = sel.account_id AND m.thread_id = sel.thread_id
               LEFT JOIN sender_rules r ON r.email = m.from_email
               LEFT JOIN contacts c ON c.email = m.from_email
             ),
             scoped AS (SELECT * FROM tm WHERE tm.folder {scope}),
             agg AS (
               SELECT account_id, thread_id,
                 COALESCE(MAX(CASE WHEN is_draft = 0 THEN date END), MAX(date)) AS tdate,
                 COUNT(DISTINCT COALESCE(message_id, id)) AS n,
                 COUNT(DISTINCT CASE WHEN is_unread = 1 THEN COALESCE(message_id, id) END) AS unread,
                 MAX(is_starred) AS starred,
                 MAX(has_attachments) AS att, MAX(is_draft) AS has_draft,
                 group_concat(DISTINCT folder) AS folders,
                 MAX(ecat = 'primary' AND folder NOT IN ('SENT','DRAFTS')) AS any_primary,
                 MAX(folder = 'SENT' OR from_email = (SELECT lower(a.email) FROM accounts a WHERE a.id = scoped.account_id)) AS mine
               FROM scoped GROUP BY account_id, thread_id
             ),
             latest AS (
               SELECT scoped.*, ROW_NUMBER() OVER (PARTITION BY account_id, thread_id ORDER BY is_draft ASC, date DESC, seq DESC) AS rn
               FROM scoped
             ),
             unsub AS (
               SELECT account_id, thread_id, MAX(unsub_json) AS u FROM scoped WHERE unsub_json IS NOT NULL GROUP BY account_id, thread_id
             )
             SELECT agg.account_id, agg.thread_id, l.subject, l.snippet, agg.tdate, agg.n, agg.unread, agg.starred,
                    agg.att, agg.has_draft, agg.folders, s.until, l.id, unsub.u,
                    MAX(agg.tdate, CASE WHEN s.until IS NOT NULL AND s.until <= :now THEN s.until ELSE 0 END) AS sort_date,
                    {tcat} AS tcat, l.from_email
             FROM agg
             JOIN latest l ON l.account_id = agg.account_id AND l.thread_id = agg.thread_id AND l.rn = 1
             LEFT JOIN snoozes s ON s.account_id = agg.account_id AND s.thread_id = agg.thread_id
             LEFT JOIN unsub ON unsub.account_id = agg.account_id AND unsub.thread_id = agg.thread_id
             WHERE {snooze_filter} AND {cat_filter}
             ORDER BY {order}",
            matches = f.matches,
            scope = f.scope,
            snooze_filter = f.snooze_filter,
            ecat = EFFECTIVE_CATEGORY,
            tcat = THREAD_CATEGORY,
            cat_filter = category_filter(q.category.as_deref()),
        );
        {
            let db = store.db();
            let mut stmt = db.prepare(&sql).unwrap();
            for (idx, p) in (1..).zip(f.params.iter()) {
                stmt.raw_bind_parameter(idx, p).unwrap();
            }
            if let Some(idx) = stmt.parameter_index(":now").unwrap() {
                stmt.raw_bind_parameter(idx, now).unwrap();
            }
            let mut rows = stmt.raw_query();
            let mut out = Vec::new();
            while let Some(r) = rows.next().unwrap() {
                out.push(thread_summary_from_row(r, now).unwrap());
            }
            out
        }
    }

    /// The page of the oracle's list that `q`'s cursor and limit select.
    fn oracle_page(store: &Store, all: &[ThreadSummary], q: &ThreadQuery, now: i64) -> Vec<ThreadSummary> {
        let f = store.thread_filter(q, now);
        let snooze_cursor = || {
            let (ba, bt) = (q.before_account_id.as_deref().unwrap(), q.before_thread_id.as_deref().unwrap());
            store.snoozed_until(ba, bt).unwrap().unwrap_or(q.before.unwrap())
        };
        let page: Vec<ThreadSummary> = all
            .iter()
            .filter(|t| match (q.before, &q.before_account_id, &q.before_thread_id) {
                (Some(_), Some(ba), Some(bt)) if f.snoozed => {
                    (t.snoozed_until.unwrap(), &t.account_id, &t.thread_id) > (snooze_cursor(), ba, bt)
                }
                (Some(b), Some(ba), Some(bt)) => (t.date, &t.account_id, &t.thread_id) < (b, ba, bt),
                (Some(b), _, _) => t.date < b,
                _ => true,
            })
            .take(q.limit.clamp(1, 500) as usize)
            .cloned()
            .collect();
        store.fill_participants(page, f.scope).unwrap()
    }

    fn comparable(threads: &[ThreadSummary]) -> serde_json::Value {
        let mut threads = threads.to_vec();
        for t in &mut threads {
            t.labels.sort();
        }
        serde_json::to_value(threads).unwrap()
    }

    /// Up to `pages` pages of `q` (keyset cursor), checked against the
    /// oracle; returns how many threads they held.
    fn check_pages(store: &Store, q: &ThreadQuery, now: i64, pages: usize) -> usize {
        let all = list_threads_oracle(store, q, now);
        let mut q = q.clone();
        let mut total = 0;
        for _ in 0..pages {
            let got = store.list_threads(&q, now).unwrap();
            assert_eq!(comparable(&got), comparable(&oracle_page(store, &all, &q, now)), "{q:?}");
            if q.before.is_none() && got.len() > 1 {
                // The timestamp-only cursor of older clients.
                let legacy = ThreadQuery { before: Some(got[0].date), ..q.clone() };
                let want = oracle_page(store, &all, &legacy, now);
                assert_eq!(comparable(&store.list_threads(&legacy, now).unwrap()), comparable(&want), "{legacy:?}");
            }
            total += got.len();
            let Some(last) = got.last() else { break };
            q.before = Some(last.date);
            q.before_account_id = Some(last.account_id.clone());
            q.before_thread_id = Some(last.thread_id.clone());
        }
        total
    }

    #[test]
    fn thread_list_matches_the_full_aggregate() {
        let store = Store::open_in_memory().unwrap();
        let now = now_ms();
        let accs = varied_mailbox(&store, now);
        let labels_ = [
            labels::INBOX, labels::ARCHIVE, labels::SENT, labels::DRAFTS, labels::TRASH, labels::SPAM, "Work",
            labels::STARRED, labels::UNREAD, labels::SNOOZED, labels::ALL, "",
        ];
        let accounts = [None, Some(accs[0].id.as_str()), Some(accs[2].id.as_str())];
        let categories = [None, Some("important"), Some("other"), Some("promotions"), Some("updates")];
        let mut seen = 0;
        for budget in [None, Some(0), Some(usize::MAX)] {
            WALK_BUDGET.set(budget);
            for label in labels_ {
                for acc in accounts {
                    for cat in categories {
                        for unread_only in [false, true] {
                            for (limit, pages) in [(1, 3), (4, 100)] {
                                let q = ThreadQuery { unread_only, ..query(acc, label, None, cat, limit) };
                                seen += check_pages(&store, &q, now, pages);
                            }
                        }
                    }
                }
            }
            let searches = [
                "invoice", "report", "from:bob", "in:trash report", "in:work", "is:starred", "-invoice", "Q3 - report",
                "اجتماع", "شكرا", "is:unread lunch", "has:attachment",
            ];
            for search in searches {
                for label in ["", labels::INBOX] {
                    for acc in accounts {
                        seen += check_pages(&store, &query(acc, label, Some(search), None, 3), now, 100);
                    }
                }
            }
        }
        WALK_BUDGET.set(None);
        assert!(seen > 5000, "{seen}");
    }

    fn subjects(threads: &[ThreadSummary]) -> Vec<&str> {
        threads.iter().map(|t| t.subject.as_str()).collect()
    }

    fn search(store: &Store, text: &str) -> Vec<String> {
        let q = query(None, "", Some(text), None, 50);
        store.list_threads(&q, now_ms()).unwrap().into_iter().map(|t| t.subject).collect()
    }

    fn plan(db: &Connection, sql: &str) -> String {
        let mut stmt = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(3)).unwrap();
        rows.collect::<rusqlite::Result<Vec<_>>>().unwrap().join("\n")
    }

    #[test]
    fn location_lookups_use_the_location_index() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        // As in upsert_message, apply_flags, delete_uids and set_location.
        for sql in [
            "SELECT id, seq FROM messages WHERE account_id = 'a' AND path = 'p' AND uid_validity = 1 AND uid = 2 AND uid > 0",
            "UPDATE messages SET is_unread = 1 WHERE account_id = 'a' AND path = 'p' AND uid_validity = 1 AND uid = 2 AND uid > 0
               AND (is_unread != 1 OR is_starred != 0 OR is_answered != 0)",
            "DELETE FROM messages WHERE account_id = 'a' AND path = 'p' AND uid_validity = 1 AND uid = 2 AND uid > 0",
            "DELETE FROM messages WHERE account_id = (SELECT account_id FROM messages WHERE id = 'x')
               AND path = 'p' AND uid_validity = 1 AND uid = 2 AND uid > 0 AND id != 'x'",
        ] {
            assert!(plan(&db, sql).contains("idx_messages_location"), "{sql}\n{}", plan(&db, sql));
        }
        let unc = "SELECT m.id FROM messages m LEFT JOIN bodies b ON b.message_id = m.id
                   WHERE m.category IS NULL ORDER BY m.date DESC LIMIT 5";
        assert!(plan(&db, unc).contains("idx_messages_uncategorized"), "{}", plan(&db, unc));
    }

    #[test]
    fn pages_continue_through_equal_timestamps() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        for (i, subject) in ["One", "Two", "Three"].iter().enumerate() {
            let raw = raw_message("a@x.test", "me@example.com", subject, "", &format!("{i}@x"));
            put(&store, &me, "INBOX", i as u32 + 1, raw, 5000);
        }
        let now = now_ms();
        let first = store.list_threads(&query(None, labels::INBOX, None, None, 2), now).unwrap();
        assert_eq!(first.len(), 2);
        let last = first.last().unwrap();
        let next = ThreadQuery {
            before: Some(last.date),
            before_account_id: Some(last.account_id.clone()),
            before_thread_id: Some(last.thread_id.clone()),
            ..query(None, labels::INBOX, None, None, 2)
        };
        let second = store.list_threads(&next, now).unwrap();
        let mut all: Vec<&str> = subjects(&first).into_iter().chain(subjects(&second)).collect();
        all.sort();
        assert_eq!(all, vec!["One", "Three", "Two"]);
        // A timestamp-only cursor still means "older than".
        let legacy = ThreadQuery { before: Some(5000), ..query(None, labels::INBOX, None, None, 2) };
        assert!(store.list_threads(&legacy, now).unwrap().is_empty());
    }

    #[test]
    fn snoozed_view_pages_in_wake_up_order() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        let now = now_ms();
        for (i, subject) in ["A", "B", "C", "D"].iter().enumerate() {
            let raw = raw_message("a@x.test", "me@example.com", subject, "", &format!("{i}@x"));
            put(&store, &me, "INBOX", i as u32 + 1, raw, 1000 * i as i64);
            // B and C wake together.
            let until = now + [1, 2, 2, 3][i] * 3_600_000;
            store.snooze(&me.id, &format!("t:{i}@x"), until).unwrap();
        }
        let mut q = query(None, labels::SNOOZED, None, None, 1);
        let mut order = Vec::new();
        loop {
            let page = store.list_threads(&q, now).unwrap();
            let Some(t) = page.last() else { break };
            order.push(t.subject.clone());
            q.before = Some(t.date);
            q.before_account_id = Some(t.account_id.clone());
            q.before_thread_id = Some(t.thread_id.clone());
        }
        assert_eq!(order, vec!["A", "B", "C", "D"]);
    }

    #[test]
    fn snoozed_threads_leave_the_inbox_until_they_wake() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        put(&store, &me, "INBOX", 1, raw_message("a@x.test", "me@example.com", "Later", "", "l@x"), 1000);
        put(&store, &me, "INBOX", 2, raw_message("a@x.test", "me@example.com", "Now", "", "n@x"), 2000);
        let now = now_ms();
        let until = now + 3_600_000;
        store.snooze(&me.id, "t:l@x", until).unwrap();
        let inbox = |at| subjects(&store.list_threads(&query(None, labels::INBOX, None, None, 50), at).unwrap()).join(",");
        assert_eq!(inbox(now), "Now");
        assert_eq!(store.unread_counts(now).unwrap()[&me.id], 1);
        assert_eq!(subjects(&store.list_threads(&query(None, labels::SNOOZED, None, None, 50), now).unwrap()), vec!["Later"]);
        // Back on top at its wake-up time.
        assert_eq!(inbox(until), "Later,Now");
        assert_eq!(store.unread_counts(until).unwrap()[&me.id], 2);
        assert!(store.list_threads(&query(None, labels::SNOOZED, None, None, 50), until).unwrap().is_empty());
    }

    #[test]
    fn search_survives_odd_input() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        put(&store, &me, "INBOX", 1, raw_message("a@x.test", "me@example.com", "Q3 report", "", "q@x"), 1000);
        let odd = ["\"", "\"\"", "-", "*", "NEAR(", "AND", "(", "^", "from:", "-\"", "\\", "\u{64e}\u{651}", "📧", "a & b", "price –"];
        for text in odd {
            assert!(store.list_threads(&query(None, "", Some(text), None, 50), now_ms()).is_ok(), "{text}");
        }
        // Punctuation between words doesn't hide the match.
        assert_eq!(search(&store, "Q3 - report"), vec!["Q3 report"]);
        assert_eq!(search(&store, "report –"), vec!["Q3 report"]);
    }

    #[test]
    fn arabic_search_ignores_harakat_and_hamza_forms() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        put(&store, &me, "INBOX", 1, raw_message("a@x.test", "me@example.com", "شُكْرًا لك", "", "s@x"), 1000);
        put(&store, &me, "INBOX", 2, raw_message("a@x.test", "me@example.com", "إجتماع الغد", "", "m@x"), 2000);
        assert_eq!(search(&store, "شكرا"), vec!["شُكْرًا لك"]);
        assert_eq!(search(&store, "شُكْرًا"), vec!["شُكْرًا لك"]);
        assert_eq!(search(&store, "اجتماع"), vec!["إجتماع الغد"]);
        assert_eq!(search(&store, "أجتماع"), vec!["إجتماع الغد"]);
    }

    #[test]
    fn in_operator_matches_folders_case_insensitively() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        let work = FolderRow {
            label_id: "Projects/Work".into(),
            path: "Projects/Work".into(),
            name: "Work".into(),
            kind: LabelKind::User,
            parent_id: Some("Projects".into()),
            delimiter: Some("/".into()),
            synced: true,
        };
        store.replace_folders(&me.id, &[work]).unwrap();
        put(&store, &me, "Projects/Work", 1, raw_message("a@x.test", "me@example.com", "Plan", "", "p@x"), 1000);
        put(&store, &me, "INBOX", 2, raw_message("a@x.test", "me@example.com", "Plan B", "", "b@x"), 2000);
        assert_eq!(search(&store, "in:work plan"), vec!["Plan"]);
        assert_eq!(search(&store, "in:projects/WORK"), vec!["Plan"]);
        assert_eq!(search(&store, "in:Projects/Work"), vec!["Plan"]);
    }

    #[test]
    fn deleted_messages_leave_the_index() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        put(&store, &me, "INBOX", 7, raw_message("a@x.test", "me@example.com", "Zebra crossing", "", "z@x"), 1000);
        assert_eq!(search(&store, "zebra").len(), 1);
        assert_eq!(store.delete_uids(&me.id, "INBOX", 1, &[7]).unwrap(), 1);
        assert!(search(&store, "zebra").is_empty());
        let n: i64 =
            store.db().query_row("SELECT COUNT(*) FROM message_fts WHERE message_fts MATCH 'zebra'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn upserts_find_their_row() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        let raw = raw_message("a@x.test", "me@example.com", "Hi", "", "h@x");
        let incoming = |uid, path: &str| IncomingMessage {
            account_id: me.id.clone(),
            thread_id: "t:h@x".into(),
            folder: path.into(),
            path: path.into(),
            uid_validity: 1,
            uid,
            date: 1000,
            is_unread: true,
            is_starred: false,
            is_draft: false,
            is_answered: false,
            parsed: crate::mime::parse(&raw).unwrap(),
            raw: None,
        };
        let (id, inserted) = store.upsert_message(&incoming(5, "INBOX")).unwrap();
        assert!(inserted);
        assert_eq!(store.upsert_message(&incoming(5, "INBOX")).unwrap(), (id.clone(), false));
        let rows: i64 = store.db().query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1);

        // A move whose new UID is unknown, then the copy shows up in a sync.
        store.set_location(&id, labels::ARCHIVE, "Archive", 1, 0).unwrap();
        assert_eq!(store.upsert_message(&incoming(9, "Archive")).unwrap(), (id.clone(), false));
        assert_eq!(store.location(&id).unwrap().unwrap().uid, 9);

        // The same message in another folder is one message of its thread.
        store.upsert_message(&incoming(3, labels::ALL)).unwrap();
        let all = store.list_threads(&query(None, labels::ALL, None, None, 50), now_ms()).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].message_count, 1);
    }

    #[test]
    fn label_counts_add_the_server_offset_where_it_applies() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        let folder = |label_id: &str, kind| FolderRow {
            label_id: label_id.into(),
            path: label_id.into(),
            name: label_id.into(),
            kind,
            parent_id: None,
            delimiter: None,
            synced: true,
        };
        let folders = [
            folder(labels::INBOX, LabelKind::System),
            folder(labels::ARCHIVE, LabelKind::System),
            folder("Work", LabelKind::User),
        ];
        store.replace_folders(&me.id, &folders).unwrap();
        for (uid, f) in [labels::INBOX, labels::ARCHIVE, "Work"].iter().enumerate() {
            put(&store, &me, f, uid as u32 + 1, raw_message("a@x.test", "me@example.com", f, "", &format!("{f}@x")), 1000);
            let state = FolderState { uid_validity: 1, uid_next: 10, highest_modseq: None, oldest_uid: None, fully_synced: false };
            store.save_folder_state(&me.id, f, &state).unwrap();
            store.set_unread_offset(&me.id, f, 10).unwrap();
        }
        let unread: HashMap<String, i64> = store.labels(&me.id).unwrap().into_iter().map(|l| (l.id, l.unread_count)).collect();
        assert_eq!(unread[labels::INBOX], 11);
        assert_eq!(unread["Work"], 11);
        assert_eq!(unread[labels::ARCHIVE], 1);
    }

    #[test]
    fn participants_skip_trash_outside_the_trash() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        let a = put(&store, &me, "INBOX", 1, raw_message("Ann <ann@x.test>", "me@example.com", "Hi", "", "a@x"), 1000);
        let b = put(&store, &me, "TRASH", 2, raw_message("Bob <bob@x.test>", "me@example.com", "Re: Hi", "", "b@x"), 2000);
        let thread = store.location(&a).unwrap().unwrap().thread_id;
        store.db().execute("UPDATE messages SET thread_id = ? WHERE id = ?", params![thread, b]).unwrap();
        let inbox = store.list_threads(&query(None, labels::INBOX, None, None, 50), now_ms()).unwrap();
        assert_eq!(inbox[0].participants, vec!["Ann"]);
        let trash = store.list_threads(&query(None, labels::TRASH, None, None, 50), now_ms()).unwrap();
        assert_eq!(trash[0].participants, vec!["Bob"]);
    }

    #[test]
    fn undo_is_refused_once_sending() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        let draft: Draft = serde_json::from_value(serde_json::json!({
            "id": "d1", "accountId": me.id, "to": [], "cc": [], "bcc": [], "subject": "x", "htmlBody": "", "updatedAt": 0
        }))
        .unwrap();
        let item = |id: &str| OutboxItem {
            id: id.into(),
            account_id: me.id.clone(),
            draft: draft.clone(),
            send_at: 0,
            kind: OutboxKind::Undo,
            status: OutboxStatus::Pending,
            attempts: 0,
            last_error: None,
            created_at: 0,
        };
        store.enqueue_outbox(&item("o1")).unwrap();
        assert!(store.claim_outbox("o1").unwrap());
        assert!(store.take_outbox("o1").unwrap().is_none());
        assert!(store.outbox_item("o1").unwrap().is_some());
        store.enqueue_outbox(&item("o2")).unwrap();
        assert_eq!(store.take_outbox("o2").unwrap().map(|i| i.id), Some("o2".into()));
        assert!(store.take_outbox("o2").unwrap().is_none());
    }

    #[test]
    fn default_signature_prefers_the_account_then_the_global_one() {
        let store = Store::open_in_memory().unwrap();
        let a = account(&store, "a@example.com");
        let b = account(&store, "b@example.com");
        let sig = |id: &str, account_id: Option<&str>| Signature {
            id: id.into(),
            account_id: account_id.map(String::from),
            name: id.into(),
            html: id.into(),
            is_default: true,
        };
        store.save_signature(&sig("global", None)).unwrap();
        store.save_signature(&sig("for-b", Some(&b.id))).unwrap();
        assert_eq!(store.default_signature(&a.id).unwrap().map(|s| s.id), Some("global".into()));
        assert_eq!(store.default_signature(&b.id).unwrap().map(|s| s.id), Some("for-b".into()));
    }

    #[test]
    fn ops_wait_behind_an_earlier_retry() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        let first = store.enqueue_op(&me.id, "archive").unwrap();
        store.enqueue_op(&me.id, "unarchive").unwrap();
        assert_eq!(store.due_ops(&me.id, 100).unwrap().len(), 2);
        store.retry_op(first, 500, "offline").unwrap();
        assert!(store.due_ops(&me.id, 100).unwrap().is_empty());
        let due: Vec<String> = store.due_ops(&me.id, 500).unwrap().into_iter().map(|o| o.op_json).collect();
        assert_eq!(due, vec!["archive", "unarchive"]);
    }

    #[test]
    fn contact_suggestions_match_underscores_literally() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        put(&store, &me, "INBOX", 1, raw_message("John <john_doe@x.test>", "me@example.com", "a", "", "1@x"), 1000);
        put(&store, &me, "INBOX", 2, raw_message("Jo <johnXdoe@x.test>", "me@example.com", "b", "", "2@x"), 1000);
        let found: Vec<String> = store.suggest_contacts("john_doe", 10).unwrap().into_iter().map(|c| c.email).collect();
        assert_eq!(found, vec!["john_doe@x.test"]);
        assert_eq!(store.suggest_contacts("100%", 10).unwrap().len(), 0);
    }

    #[test]
    fn file_store_reads_while_a_write_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(Store::open(&dir.path().join("r.db")).unwrap());
        let me = account(&store, "me@example.com");
        put(&store, &me, "INBOX", 1, raw_message("a@x.test", "me@example.com", "Hi", "", "h@x"), 1000);
        let (locked, done) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
        let writer = {
            let store = store.clone();
            std::thread::spawn(move || {
                let _db = store.db();
                locked.0.send(()).unwrap();
                done.1.recv().unwrap();
            })
        };
        locked.1.recv().unwrap();
        // These would wait forever on the writer's lock.
        assert_eq!(store.list_threads(&query(None, labels::INBOX, None, None, 50), now_ms()).unwrap().len(), 1);
        assert_eq!(store.accounts().unwrap().len(), 1);
        assert!(store.reader().execute("DELETE FROM accounts", []).is_err(), "the reader is read-only");
        done.0.send(()).unwrap();
        writer.join().unwrap();
    }

    #[test]
    fn upgrades_v3_full_text_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v3.db");
        let (me, old_id) = {
            let store = Store::open(&path).unwrap();
            let me = account(&store, "me@example.com");
            let id = put(&store, &me, "INBOX", 1, raw_message("a@x.test", "me@example.com", "شُكْرًا", "", "s@x"), 1000);
            put(&store, &me, "INBOX", 2, raw_message("b@x.test", "me@example.com", "Plain", "", "p@x"), 2000);
            // What v3 left: the old table, filled by the old (unnormalized) indexing.
            let db = store.db();
            db.execute_batch(
                "DROP TABLE message_fts;
                 CREATE VIRTUAL TABLE message_fts USING fts5(subject, participants, body, content='', contentless_delete=1,
                   tokenize='unicode61 remove_diacritics 2');
                 INSERT INTO message_fts(rowid, subject, participants, body) SELECT seq, subject, '', snippet FROM messages;
                 PRAGMA user_version = 3;",
            )
            .unwrap();
            drop(db);
            (me, id)
        };
        let store = Store::open(&path).unwrap();
        let sql: String =
            store.db().query_row("SELECT sql FROM sqlite_master WHERE name = 'message_fts'", [], |r| r.get(0)).unwrap();
        assert!(sql.contains("prefix='2 3'"), "{sql}");
        // Before the backlog runs: matched on what the list shows.
        assert_eq!(search(&store, "plain"), vec!["Plain"]);
        assert_eq!(store.reindex_fts_batch(1).unwrap(), 1);
        assert_eq!(store.reindex_fts_batch(1).unwrap(), 1);
        assert_eq!(store.reindex_fts_batch(1).unwrap(), 0);
        assert_eq!(store.reindex_fts_batch(1).unwrap(), 0);
        assert!(store.setting::<i64>(FTS_REINDEX_KEY).is_none());
        // The body came from the stored raw message.
        assert_eq!(search(&store, "شكرا"), vec!["شُكْرًا"]);
        assert_eq!(search(&store, "hello there plain"), vec!["Plain"]);
        put(&store, &me, "INBOX", 3, raw_message("c@x.test", "me@example.com", "Fresh", "", "f@x"), 3000);
        assert_eq!(search(&store, "fresh"), vec!["Fresh"]);
        assert!(store.message_meta(&old_id).unwrap().is_some());
        let v: i64 = store.db().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        drop(store);
        // Reopening doesn't start over.
        let store = Store::open(&path).unwrap();
        assert_eq!(store.reindex_fts_batch(10).unwrap(), 0);
    }

    #[test]
    fn reindex_keeps_a_body_that_arrives_meanwhile() {
        let store = Store::open_in_memory().unwrap();
        let me = account(&store, "me@example.com");
        let raw = raw_message("a@x.test", "me@example.com", "Report", "", "r@x");
        // Synced without its body: only the headers are known.
        let mut parsed = crate::mime::parse(&raw).unwrap();
        (parsed.text, parsed.html, parsed.snippet) = (None, None, String::new());
        let inc = IncomingMessage {
            account_id: me.id.clone(),
            thread_id: "t:r@x".into(),
            folder: "INBOX".into(),
            path: "INBOX".into(),
            uid_validity: 1,
            uid: 1,
            date: 1000,
            is_unread: true,
            is_starred: false,
            is_draft: false,
            is_answered: false,
            parsed,
            raw: None,
        };
        let (id, _) = store.upsert_message(&inc).unwrap();
        assert!(search(&store, "hello there").is_empty());
        store.set_setting(FTS_REINDEX_KEY, &i64::MAX).unwrap();
        // The backlog reads the headers-only row, then the body is fetched.
        let rows = store.reindex_fts_read(10).unwrap().unwrap();
        store.store_body(&id, &raw, &crate::mime::parse(&raw).unwrap()).unwrap();
        store.reindex_fts_write(&rows, 10).unwrap();
        assert_eq!(search(&store, "hello there"), vec!["Report"]);
    }
}
