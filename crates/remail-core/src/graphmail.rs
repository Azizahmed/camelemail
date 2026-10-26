//! Mail over Microsoft Graph, for Microsoft mailboxes whose IMAP Microsoft
//! refuses ("User is authenticated but not connected") although Graph opens
//! them: some personal accounts are like that with IMAP switched on.
//!
//! Sync stays the IMAP-shaped code in `sync`; this module answers the same
//! questions (folders, UIDs, flags, moves) from a local mirror of each
//! folder's message list, kept current with Graph's change tracking (delta).
//! Each message gets a UID in its folder in arrival order, so "new mail is
//! at or above UIDNEXT" and "the newest N" hold as they do over IMAP, and a
//! change counter per folder stands in for CONDSTORE's MODSEQ. Ids are
//! immutable ones, so a message keeps its id when it moves.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures::future::BoxFuture;
use futures::StreamExt;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::graph::GraphError;
use crate::imap::{FetchedMessage, MailboxStatus, MsgFlags, RemoteFolder, FULL_FETCH_MAX_BYTES};
use crate::model::{Account, Security, ServerConfig};
use crate::search::SearchQuery;
use crate::store::Store;

/// Delegated permissions for reading, filing and sending the user's mail.
/// Asked for at every Microsoft sign-in, so falling back needs no new consent.
pub const SCOPE: &str = "https://graph.microsoft.com/Mail.ReadWrite https://graph.microsoft.com/Mail.Send offline_access";

/// An account whose mail goes through Graph has this as its IMAP and SMTP host.
pub const HOST: &str = "graph.microsoft.com";

const PREFER: &str = r#"IdType="ImmutableId", odata.maxpagesize=200"#;
const DELTA_SELECT: &str = "id,isRead,flag,isDraft,receivedDateTime,internetMessageId";
/// Message downloads running at once.
const PARALLEL: usize = 4;
/// MAPI's PidTagLastVerbExecuted, set to "replied" (102) for \Answered.
const LAST_VERB: &str = "Integer 0x1081";

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS graph_folders (
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  folder_id TEXT NOT NULL,
  -- Where change tracking continues; NULL = list the folder from the start
  delta_link TEXT,
  next_uid INTEGER NOT NULL DEFAULT 1,
  modseq INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (account_id, folder_id)
);
CREATE TABLE IF NOT EXISTS graph_messages (
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  graph_id TEXT NOT NULL,
  folder_id TEXT NOT NULL,
  uid INTEGER NOT NULL,
  received INTEGER NOT NULL DEFAULT 0,
  is_read INTEGER NOT NULL DEFAULT 0,
  flagged INTEGER NOT NULL DEFAULT 0,
  -- Graph has no \Answered: remembered from our own replies
  answered INTEGER NOT NULL DEFAULT 0,
  is_draft INTEGER NOT NULL DEFAULT 0,
  -- Message-ID without <>
  message_id TEXT,
  modseq INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (account_id, graph_id)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_graph_messages_uid ON graph_messages(account_id, folder_id, uid);
CREATE INDEX IF NOT EXISTS idx_graph_messages_msgid ON graph_messages(account_id, message_id);
"#;

/// Whether the account's mail goes through Graph rather than IMAP/SMTP.
pub fn uses_graph(account: &Account) -> bool {
    account.auth == crate::oauth::MICROSOFT && account.imap.host.eq_ignore_ascii_case(HOST)
}

/// The "servers" of an account whose mail goes through Graph.
pub fn servers() -> (ServerConfig, ServerConfig) {
    let s = ServerConfig { host: HOST.into(), port: 443, security: Security::Tls };
    (s.clone(), s)
}

/// Gives an access token for Graph mail; `Some(token)`: that one was
/// refused, get another.
pub type TokenSource = Arc<dyn Fn(Option<String>) -> BoxFuture<'static, Result<String>> + Send + Sync>;

/// A token source that only has `token` (checks before an account exists).
pub fn fixed_token(token: String) -> TokenSource {
    Arc::new(move |_| {
        let t = token.clone();
        Box::pin(async move { Ok(t) })
    })
}

enum Body<'a> {
    None,
    Json(&'a Value),
    /// A whole message, which Graph takes base64-encoded as text/plain.
    Mime(&'a [u8]),
}

/// Requests to Graph as one account.
#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    base: String,
    token: TokenSource,
}

impl Api {
    pub fn new(http: reqwest::Client, base: &str, token: TokenSource) -> Api {
        Api { http, base: base.trim_end_matches('/').to_string(), token }
    }

    /// `{base}/me/{segments…}` with each segment escaped (ids can contain `/` and `+`).
    fn url(&self, segments: &[&str]) -> Result<reqwest::Url> {
        let mut u = reqwest::Url::parse(&format!("{}/me", self.base))?;
        u.path_segments_mut().map_err(|_| anyhow!("bad Graph address"))?.extend(segments);
        Ok(u)
    }

    /// A paging or change link from an answer (or saved from one): only ever
    /// followed to Graph itself, since the token goes with it.
    fn link(&self, url: &str) -> Result<reqwest::Url> {
        if !url.starts_with(&format!("{}/", self.base)) {
            return Err(anyhow!("Microsoft sent a link to somewhere else"));
        }
        Ok(reqwest::Url::parse(url)?)
    }

    /// Send a request and return the answer's bytes. Retries when Graph is
    /// busy (429/503/504, honouring Retry-After), and once with a new token
    /// when it refuses the one it got.
    async fn send(&self, method: reqwest::Method, url: reqwest::Url, body: Body<'_>, timeout: Duration) -> Result<Vec<u8>> {
        use base64::Engine as _;
        let mut token = (self.token)(None).await?;
        let mut renewed = false;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let mut req = self.http.request(method.clone(), url.clone()).bearer_auth(&token).header("Prefer", PREFER).timeout(timeout);
            req = match &body {
                Body::None => req,
                Body::Json(v) => req.json(v),
                Body::Mime(raw) => req.header(reqwest::header::CONTENT_TYPE, "text/plain").body(base64::engine::general_purpose::STANDARD.encode(raw)),
            };
            let resp = req.send().await.map_err(|e| e.without_url()).context("couldn't reach Microsoft")?;
            let status = resp.status().as_u16();
            if matches!(status, 429 | 503 | 504) && attempt < 4 {
                let wait = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .unwrap_or(2 * attempt)
                    .min(30);
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }
            if status == 401 && !renewed {
                renewed = true;
                token = (self.token)(Some(token)).await?;
                continue;
            }
            let bytes = resp.bytes().await.map_err(|e| e.without_url()).context("reading Microsoft's answer")?.to_vec();
            if (200..300).contains(&status) {
                return Ok(bytes);
            }
            let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            let code = v["error"]["code"].as_str().unwrap_or_default().to_string();
            let message = match v["error"]["message"].as_str().map(str::trim).filter(|m| !m.is_empty()) {
                Some(m) => format!("Microsoft: {m} (HTTP {status})"),
                None => format!("Microsoft answered HTTP {status}"),
            };
            // Refused twice, or the permission was never given or was taken back.
            if status == 401 || (status == 403 && code.eq_ignore_ascii_case("ErrorAccessDenied")) {
                return Err(crate::oauth::SignInRequired(format!("Microsoft didn't let this app read the mailbox ({message}). Sign in again.")).into());
            }
            return Err(GraphError { status, code, message }.into());
        }
    }

    async fn json(&self, method: reqwest::Method, url: reqwest::Url, body: Body<'_>) -> Result<Value> {
        let bytes = self.send(method, url, body, Duration::from_secs(60)).await?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).context("Microsoft sent an unreadable answer")
    }

    async fn get(&self, url: reqwest::Url) -> Result<Value> {
        self.json(reqwest::Method::GET, url, Body::None).await
    }

    /// Check that Graph opens the mailbox.
    pub async fn probe(&self) -> Result<()> {
        let mut u = self.url(&["mailFolders", "inbox"])?;
        u.query_pairs_mut().append_pair("$select", "id");
        self.get(u).await?;
        Ok(())
    }

    /// Send a whole message; Microsoft files a copy in Sent Items itself.
    pub async fn send_mime(&self, raw: &[u8]) -> Result<()> {
        let u = self.url(&["sendMail"])?;
        self.send(reqwest::Method::POST, u, Body::Mime(raw), Duration::from_secs(300)).await?;
        Ok(())
    }

    /// The full source of a message; `None` when it's gone.
    async fn mime(&self, graph_id: &str) -> Result<Option<Vec<u8>>> {
        let u = self.url(&["messages", graph_id, "$value"])?;
        match self.send(reqwest::Method::GET, u, Body::None, Duration::from_secs(300)).await {
            Err(e) if not_found(&e) => Ok(None),
            r => r.map(Some),
        }
    }
}

fn not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<GraphError>().is_some_and(|g| g.status == 404)
}

/// Put Bcc back in a message built for SMTP (where it went in the envelope
/// only): Graph takes the recipients from the headers, and leaves Bcc out of
/// what it delivers.
pub fn with_bcc(raw: &[u8], bcc: &[String]) -> Vec<u8> {
    let bcc: Vec<&str> = bcc.iter().map(|a| a.trim()).filter(|a| !a.is_empty() && !a.contains(['\r', '\n'])).collect();
    if bcc.is_empty() {
        return raw.to_vec();
    }
    let mut out = format!("Bcc: {}\r\n", bcc.join(", ")).into_bytes();
    out.extend_from_slice(raw);
    out
}

/// A folder's UIDVALIDITY: from its id, so a folder deleted and made again
/// under the same name starts over, like it would over IMAP.
fn uid_validity(folder_id: &str) -> u32 {
    // FNV-1a
    let mut h: u32 = 0x811c9dc5;
    for b in folder_id.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h.max(1)
}

fn received_ms(v: &Value) -> Option<i64> {
    v["receivedDateTime"].as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp_millis())
}

fn bare_message_id(s: &str) -> Option<String> {
    let s = s.trim().trim_start_matches('<').trim_end_matches('>').trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// What Graph said about a message (fields it left out stay as they were).
#[derive(Debug, Default, Clone)]
struct Seen {
    received: Option<i64>,
    is_read: Option<bool>,
    flagged: Option<bool>,
    is_draft: Option<bool>,
    message_id: Option<String>,
}

impl Seen {
    fn of(v: &Value) -> Seen {
        Seen {
            received: received_ms(v),
            is_read: v["isRead"].as_bool(),
            flagged: v["flag"]["flagStatus"].as_str().map(|s| s.eq_ignore_ascii_case("flagged")),
            is_draft: v["isDraft"].as_bool(),
            message_id: v["internetMessageId"].as_str().and_then(bare_message_id),
        }
    }
}

/// The mirror's row for a message.
#[derive(Debug, Clone)]
struct Row {
    graph_id: String,
    uid: u32,
    received: i64,
    flags: MsgFlags,
}

/// One account's mail through Graph, shaped like an IMAP connection.
pub struct GraphMail {
    api: Api,
    store: Arc<Store>,
    account_id: String,
    /// Folder path → Graph folder id (from the last folder list).
    paths: HashMap<String, String>,
    /// Ids of the folders Outlook gives a role, and that role (`INBOX`, `\Sent`…; `""` for the Outbox).
    roles: Option<HashMap<String, &'static str>>,
}

impl GraphMail {
    pub fn new(api: Api, store: Arc<Store>, account_id: &str) -> GraphMail {
        GraphMail { api, store, account_id: account_id.to_string(), paths: HashMap::new(), roles: None }
    }

    async fn roles(&mut self) -> Result<HashMap<String, &'static str>> {
        if let Some(r) = &self.roles {
            return Ok(r.clone());
        }
        let mut roles = HashMap::new();
        for (name, role) in [
            ("inbox", "INBOX"),
            ("drafts", "\\Drafts"),
            ("sentitems", "\\Sent"),
            ("deleteditems", "\\Trash"),
            ("junkemail", "\\Junk"),
            ("archive", "\\Archive"),
            ("outbox", ""),
        ] {
            let mut u = self.api.url(&["mailFolders", name])?;
            u.query_pairs_mut().append_pair("$select", "id");
            match self.api.get(u).await {
                Ok(v) => {
                    if let Some(id) = v["id"].as_str() {
                        roles.insert(id.to_string(), role);
                    }
                }
                // Not every mailbox has every one (an archive, say).
                Err(e) if not_found(&e) => {}
                Err(e) => return Err(e),
            }
        }
        self.roles = Some(roles.clone());
        Ok(roles)
    }

    /// Every folder with its children, as paths joined by `/`. The inbox is
    /// `INBOX`, as over IMAP.
    pub async fn list_folders(&mut self) -> Result<Vec<RemoteFolder>> {
        let roles = self.roles().await?;
        let inbox = roles.iter().find(|(_, r)| **r == "INBOX").map(|(id, _)| id.clone()).unwrap_or_default();
        let mut out = Vec::new();
        let mut paths = HashMap::new();
        // (folder list URL, parent path)
        let mut queue = vec![(self.api.url(&["mailFolders"])?, None::<String>)];
        let mut requests = 0;
        while let Some((mut url, parent)) = queue.pop() {
            url.query_pairs_mut().append_pair("$select", "id,displayName,childFolderCount").append_pair("$top", "100");
            loop {
                requests += 1;
                if requests > 200 {
                    bail!("Microsoft kept sending more folders");
                }
                let v = self.api.get(url.clone()).await?;
                for f in v["value"].as_array().into_iter().flatten() {
                    let (Some(id), Some(name)) = (f["id"].as_str(), f["displayName"].as_str()) else { continue };
                    let role = roles.get(id).copied();
                    if role == Some("") {
                        continue;
                    }
                    let name = name.replace('/', "∕");
                    let path = if id == inbox {
                        "INBOX".to_string()
                    } else {
                        match &parent {
                            Some(p) => format!("{p}/{name}"),
                            None => name.clone(),
                        }
                    };
                    if f["childFolderCount"].as_i64().unwrap_or(0) > 0 && path.matches('/').count() < 5 {
                        queue.push((self.api.url(&["mailFolders", id, "childFolders"])?, Some(path.clone())));
                    }
                    paths.insert(path.clone(), id.to_string());
                    out.push(RemoteFolder {
                        display: if id == inbox { "Inbox".into() } else { path.clone() },
                        path,
                        delimiter: Some("/".into()),
                        special_use: role.filter(|r| r.starts_with('\\')),
                        selectable: true,
                    });
                }
                match v["@odata.nextLink"].as_str() {
                    Some(n) => url = self.api.link(n)?,
                    None => break,
                }
            }
        }
        self.paths = paths;
        Ok(out)
    }

    async fn folder_id(&mut self, path: &str) -> Result<String> {
        if !self.paths.contains_key(path) {
            self.list_folders().await?;
        }
        self.paths.get(path).cloned().with_context(|| format!("no folder {path}"))
    }

    fn path_of(&self, folder_id: &str) -> Option<String> {
        self.paths.iter().find(|(_, id)| *id == folder_id).map(|(p, _)| p.clone())
    }

    /// Bring the folder's mirror up to date and describe it like SELECT does.
    pub async fn select(&mut self, path: &str) -> Result<MailboxStatus> {
        let folder = self.folder_id(path).await?;
        self.refresh(&folder).await.with_context(|| format!("listing {path}"))?;
        let db = self.store.db();
        let (next_uid, modseq): (i64, i64) = db.query_row(
            "SELECT next_uid, modseq FROM graph_folders WHERE account_id = ? AND folder_id = ?",
            params![self.account_id, folder],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let exists: i64 = db.query_row(
            "SELECT COUNT(*) FROM graph_messages WHERE account_id = ? AND folder_id = ?",
            params![self.account_id, folder],
            |r| r.get(0),
        )?;
        Ok(MailboxStatus { uid_validity: uid_validity(&folder), uid_next: next_uid as u32, exists: exists as u32, highest_modseq: Some(modseq as u64) })
    }

    /// One round of change tracking for a folder, applied to the mirror.
    async fn refresh(&mut self, folder: &str) -> Result<()> {
        let saved: Option<String> = {
            let db = self.store.db();
            db.execute("INSERT OR IGNORE INTO graph_folders(account_id, folder_id) VALUES(?, ?)", params![self.account_id, folder])?;
            db.query_row(
                "SELECT delta_link FROM graph_folders WHERE account_id = ? AND folder_id = ?",
                params![self.account_id, folder],
                |r| r.get(0),
            )?
        };
        let start = || -> Result<reqwest::Url> {
            let mut u = self.api.url(&["mailFolders", folder, "messages", "delta"])?;
            u.query_pairs_mut().append_pair("$select", DELTA_SELECT);
            Ok(u)
        };
        let mut full = saved.is_none();
        let mut url = match &saved {
            Some(l) => self.api.link(l)?,
            None => start()?,
        };
        let mut changes: Vec<(String, Option<Seen>)> = Vec::new();
        let mut pages = 0;
        let delta_link = loop {
            pages += 1;
            if pages > 5000 {
                bail!("Microsoft kept sending more changes");
            }
            let v = match self.api.get(url.clone()).await {
                Err(e) if !full && e.downcast_ref::<GraphError>().is_some_and(GraphError::sync_reset) => {
                    tracing::info!("Graph change tracking expired; listing the folder again");
                    full = true;
                    changes.clear();
                    url = start()?;
                    continue;
                }
                r => r?,
            };
            for e in v["value"].as_array().into_iter().flatten() {
                let Some(id) = e["id"].as_str() else { continue };
                let change = if e.get("@removed").is_some() { None } else { Some(Seen::of(e)) };
                changes.push((id.to_string(), change));
            }
            if let Some(n) = v["@odata.nextLink"].as_str() {
                url = self.api.link(n)?;
                continue;
            }
            break v["@odata.deltaLink"].as_str().map(String::from);
        };
        apply_changes(&self.store, &self.account_id, folder, changes, full, delta_link.as_deref())
    }

    fn rows(&self, folder: &str, uids: &[u32]) -> Result<Vec<Row>> {
        let db = self.store.db();
        let mut stmt = db.prepare_cached(
            "SELECT graph_id, uid, received, is_read, flagged, answered, is_draft FROM graph_messages
             WHERE account_id = ? AND folder_id = ? AND uid = ?",
        )?;
        let mut out = Vec::new();
        for uid in uids {
            if let Some(r) = stmt.query_row(params![self.account_id, folder, *uid as i64], row_from).optional()? {
                out.push(r);
            }
        }
        Ok(out)
    }

    fn all_rows(&self, folder: &str, changed_since: Option<u64>) -> Result<Vec<Row>> {
        let db = self.store.db();
        let mut stmt = db.prepare_cached(
            "SELECT graph_id, uid, received, is_read, flagged, answered, is_draft FROM graph_messages
             WHERE account_id = ? AND folder_id = ? AND modseq > ? ORDER BY uid",
        )?;
        let since = changed_since.map_or(-1, |m| m as i64);
        let rows = stmt.query_map(params![self.account_id, folder, since], row_from)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub async fn status_unseen(&mut self, path: &str) -> Result<u32> {
        let folder = self.folder_id(path).await?;
        Ok(self.all_rows(&folder, None)?.iter().filter(|r| !r.flags.seen && !r.flags.draft).count() as u32)
    }

    /// The searches sync makes: `ALL`, `UNSEEN`, `UID a:b` and `UID a:*`,
    /// alone or with `UNSEEN`.
    pub async fn search(&mut self, path: &str, criteria: &str) -> Result<Vec<u32>> {
        let folder = self.folder_id(path).await?;
        let (mut low, mut high, mut unseen) = (0u32, u32::MAX, false);
        let mut words = criteria.split_whitespace();
        while let Some(w) = words.next() {
            match w.to_ascii_uppercase().as_str() {
                "ALL" => {}
                "UNSEEN" => unseen = true,
                "UID" => {
                    let range = words.next().context("UID without a range")?;
                    let (a, b) = range.split_once(':').unwrap_or((range, range));
                    low = a.parse().context("bad UID range")?;
                    high = if b == "*" { u32::MAX } else { b.parse().context("bad UID range")? };
                }
                _ => bail!("this search isn't available for mail read through Microsoft Graph"),
            }
        }
        Ok(self
            .all_rows(&folder, None)?
            .into_iter()
            .filter(|r| r.uid >= low && r.uid <= high && (!unseen || !r.flags.seen))
            .map(|r| r.uid)
            .collect())
    }

    /// Messages by UID, downloaded whole; large ones are kept headers-only
    /// (`complete = false`) and fetched again on open.
    pub async fn fetch_messages(&mut self, path: &str, uids: &[u32]) -> Result<Vec<FetchedMessage>> {
        let folder = self.folder_id(path).await?;
        let rows = self.rows(&folder, uids)?;
        let api = self.api.clone();
        let fetched: Vec<Result<Option<FetchedMessage>>> = futures::stream::iter(rows)
            .map(|r| {
                let api = api.clone();
                async move {
                    let Some(mut raw) = api.mime(&r.graph_id).await? else { return Ok(None) };
                    let size = raw.len() as u32;
                    let complete = size <= FULL_FETCH_MAX_BYTES;
                    if !complete {
                        raw = headers_only(&raw);
                    }
                    Ok(Some(FetchedMessage { uid: r.uid, flags: r.flags, internal_date: Some(r.received).filter(|d| *d > 0), size: Some(size), raw, complete }))
                }
            })
            .buffer_unordered(PARALLEL)
            .collect()
            .await;
        let mut out = Vec::new();
        for f in fetched {
            if let Some(m) = f? {
                out.push(m);
            }
        }
        out.sort_by_key(|m| m.uid);
        Ok(out)
    }

    pub async fn fetch_raw(&mut self, path: &str, uid: u32) -> Result<Option<Vec<u8>>> {
        let folder = self.folder_id(path).await?;
        match self.rows(&folder, &[uid])?.into_iter().next() {
            Some(r) => self.api.mime(&r.graph_id).await,
            None => Ok(None),
        }
    }

    /// Flags for `uids`, or for every message changed since `modseq`.
    pub async fn fetch_flags(&mut self, path: &str, uids: &[u32], changed_since: Option<u64>) -> Result<Vec<(u32, MsgFlags)>> {
        let folder = self.folder_id(path).await?;
        let rows = match changed_since {
            Some(_) => self.all_rows(&folder, changed_since)?,
            None => self.rows(&folder, uids)?,
        };
        Ok(rows.into_iter().map(|r| (r.uid, r.flags)).collect())
    }

    pub async fn existing_uids(&mut self, path: &str, uids: &[u32]) -> Result<Vec<u32>> {
        let folder = self.folder_id(path).await?;
        let mut out: Vec<u32> = self.rows(&folder, uids)?.into_iter().map(|r| r.uid).collect();
        out.sort_unstable();
        Ok(out)
    }

    /// Set (`add`) or clear `\Seen`, `\Flagged` or `\Answered`.
    pub async fn store_flags(&mut self, path: &str, uids: &[u32], add: bool, flag: &str) -> Result<()> {
        let folder = self.folder_id(path).await?;
        let (body, column) = match flag {
            "\\Seen" => (json!({ "isRead": add }), "is_read"),
            "\\Flagged" => (json!({ "flag": { "flagStatus": if add { "flagged" } else { "notFlagged" } } }), "flagged"),
            "\\Answered" if add => (json!({ "singleValueExtendedProperties": [{ "id": LAST_VERB, "value": "102" }] }), "answered"),
            "\\Answered" => (Value::Null, "answered"),
            other => bail!("can't set {other} through Microsoft Graph"),
        };
        for r in self.rows(&folder, uids)? {
            if !body.is_null() {
                let u = self.api.url(&["messages", &r.graph_id])?;
                match self.api.json(reqwest::Method::PATCH, u, Body::Json(&body)).await {
                    Ok(_) => {}
                    Err(e) if not_found(&e) => continue,
                    // Outlook's reply arrow is a nicety; our own answered mark is what counts.
                    Err(e) if column == "answered" => tracing::debug!("marking replied in Outlook failed: {e:#}"),
                    Err(e) => return Err(e),
                }
            }
            self.store.db().execute(
                &format!("UPDATE graph_messages SET {column} = ? WHERE account_id = ? AND graph_id = ?"),
                params![add, self.account_id, r.graph_id],
            )?;
        }
        Ok(())
    }

    pub async fn move_uids(&mut self, path: &str, uids: &[u32], dest: &str) -> Result<()> {
        if path == dest {
            return Ok(());
        }
        let (from, to) = (self.folder_id(path).await?, self.folder_id(dest).await?);
        let body = json!({ "destinationId": to });
        for r in self.rows(&from, uids)? {
            let u = self.api.url(&["messages", &r.graph_id, "move"])?;
            let moved = match self.api.json(reqwest::Method::POST, u, Body::Json(&body)).await {
                Ok(v) => v,
                Err(e) if not_found(&e) => {
                    self.forget(&r.graph_id)?;
                    continue;
                }
                Err(e) => return Err(e).with_context(|| format!("moving to {dest}")),
            };
            let new_id = moved["id"].as_str().unwrap_or(&r.graph_id).to_string();
            let mut db = self.store.db();
            let tx = db.transaction()?;
            tx.execute("INSERT OR IGNORE INTO graph_folders(account_id, folder_id) VALUES(?, ?)", params![self.account_id, to])?;
            if new_id != r.graph_id {
                tx.execute("DELETE FROM graph_messages WHERE account_id = ? AND graph_id = ?", params![self.account_id, new_id])?;
            }
            let uid = take_uid(&tx, &self.account_id, &to)?;
            tx.execute(
                "UPDATE graph_messages SET graph_id = ?, folder_id = ?, uid = ?, modseq = 0 WHERE account_id = ? AND graph_id = ?",
                params![new_id, to, uid, self.account_id, r.graph_id],
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Delete for good (Trash/Spam "delete forever", replaced drafts).
    pub async fn delete_uids(&mut self, path: &str, uids: &[u32]) -> Result<()> {
        let folder = self.folder_id(path).await?;
        let api = self.api.clone();
        let rows = self.rows(&folder, uids)?;
        let done: Vec<Result<String>> = futures::stream::iter(rows)
            .map(|r| {
                let api = api.clone();
                async move {
                    let u = api.url(&["messages", &r.graph_id])?;
                    match api.json(reqwest::Method::DELETE, u, Body::None).await {
                        Err(e) if !not_found(&e) => Err(e),
                        _ => Ok(r.graph_id),
                    }
                }
            })
            .buffer_unordered(PARALLEL)
            .collect()
            .await;
        let mut first_error = None;
        for d in done {
            match d {
                Ok(id) => self.forget(&id)?,
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn forget(&self, graph_id: &str) -> Result<()> {
        self.store.db().execute("DELETE FROM graph_messages WHERE account_id = ? AND graph_id = ?", params![self.account_id, graph_id])?;
        Ok(())
    }

    /// Store a message in a folder (a draft, as Graph makes every message it's given).
    pub async fn append(&mut self, path: &str, flags: &str, raw: &[u8]) -> Result<()> {
        let folder = self.folder_id(path).await?;
        let u = self.api.url(&["mailFolders", &folder, "messages"])?;
        let made = self.api.json(reqwest::Method::POST, u, Body::Mime(raw)).await.with_context(|| format!("saving to {path}"))?;
        let id = made["id"].as_str().context("Microsoft didn't return the new message")?.to_string();
        let mut seen = Seen::of(&made);
        if flags.contains("\\Seen") && seen.is_read != Some(true) {
            let u = self.api.url(&["messages", &id])?;
            self.api.json(reqwest::Method::PATCH, u, Body::Json(&json!({ "isRead": true }))).await?;
            seen.is_read = Some(true);
        }
        if seen.message_id.is_none() {
            seen.message_id = crate::mime::parse(raw).and_then(|p| p.message_id);
        }
        let mut db = self.store.db();
        let tx = db.transaction()?;
        tx.execute("INSERT OR IGNORE INTO graph_folders(account_id, folder_id) VALUES(?, ?)", params![self.account_id, folder])?;
        let uid = take_uid(&tx, &self.account_id, &folder)?;
        insert_row(&tx, &self.account_id, &id, &folder, uid, &seen, 0)?;
        tx.commit()?;
        Ok(())
    }

    /// UIDs by Message-ID (without `<>`).
    pub async fn find_by_message_ids(&mut self, path: &str, message_ids: &[String]) -> Result<HashMap<String, u32>> {
        let folder = self.folder_id(path).await?;
        let db = self.store.db();
        let mut stmt = db.prepare_cached("SELECT MAX(uid) FROM graph_messages WHERE account_id = ? AND folder_id = ? AND message_id = ?")?;
        let mut found = HashMap::new();
        for mid in message_ids {
            let uid: Option<i64> = stmt.query_row(params![self.account_id, folder, mid], |r| r.get(0))?;
            if let Some(uid) = uid {
                found.insert(mid.clone(), uid as u32);
            }
        }
        Ok(found)
    }

    /// A top-level folder; one that exists already counts as made.
    pub async fn create_folder(&mut self, name: &str) -> Result<()> {
        let u = self.api.url(&["mailFolders"])?;
        match self.api.json(reqwest::Method::POST, u, Body::Json(&json!({ "displayName": name }))).await {
            Err(e) if e.downcast_ref::<GraphError>().is_some_and(|g| g.status == 409 || g.code.eq_ignore_ascii_case("ErrorFolderExists")) => {}
            r => {
                r?;
            }
        }
        self.list_folders().await?;
        Ok(())
    }

    /// Search the whole mailbox at Microsoft. Returns `(path, uid)` of the
    /// matches in folders this mirror tracks, newest first.
    pub async fn search_messages(&mut self, q: &SearchQuery, limit: usize) -> Result<Vec<(String, u32)>> {
        let kql = to_kql(q);
        if kql.is_empty() {
            return Ok(Vec::new());
        }
        if self.paths.is_empty() {
            self.list_folders().await?;
        }
        let mut u = self.api.url(&["messages"])?;
        u.query_pairs_mut()
            .append_pair("$search", &format!("\"{}\"", kql.replace('"', "")))
            .append_pair("$select", "id,parentFolderId")
            .append_pair("$top", &limit.to_string());
        let v = self.api.get(u).await?;
        let db = self.store.db();
        let mut stmt = db.prepare_cached("SELECT folder_id, uid FROM graph_messages WHERE account_id = ? AND graph_id = ?")?;
        let mut out = Vec::new();
        for m in v["value"].as_array().into_iter().flatten() {
            let Some(id) = m["id"].as_str() else { continue };
            let Some((folder, uid)) = stmt.query_row(params![self.account_id, id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).optional()? else {
                continue;
            };
            if let Some(path) = self.path_of(&folder) {
                out.push((path, uid as u32));
            }
        }
        Ok(out)
    }

    /// The UIDVALIDITY a folder has here.
    pub async fn uid_validity(&mut self, path: &str) -> Result<u32> {
        Ok(uid_validity(&self.folder_id(path).await?))
    }
}

/// A Graph `$search` for a query (KQL). Unread/starred can't be searched
/// for there; local search applies them to what comes back.
fn to_kql(q: &SearchQuery) -> String {
    let mut parts: Vec<String> = Vec::new();
    let term = |s: &str| if s.contains(char::is_whitespace) { format!("'{s}'") } else { s.to_string() };
    for (neg, t) in &q.text {
        if !neg {
            parts.push(term(t));
        }
    }
    for (field, list) in [("from", &q.from), ("to", &q.to), ("subject", &q.subject)] {
        for (neg, t) in list {
            if !neg {
                parts.push(format!("{field}:{}", term(t)));
            }
        }
    }
    if q.has_attachment == Some(true) {
        parts.push("hasattachments:true".into());
    }
    let day = |ms: i64| chrono::DateTime::from_timestamp_millis(ms).map(|d| d.format("%Y-%m-%d").to_string());
    if let Some(d) = q.after.and_then(day) {
        parts.push(format!("received>={d}"));
    }
    if let Some(d) = q.before.and_then(day) {
        parts.push(format!("received<{d}"));
    }
    parts.join(" ")
}

fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        graph_id: r.get(0)?,
        uid: r.get::<_, i64>(1)? as u32,
        received: r.get(2)?,
        flags: MsgFlags { seen: r.get(3)?, flagged: r.get(4)?, answered: r.get(5)?, draft: r.get(6)?, deleted: false },
    })
}

/// The header block of a message (with the blank line that ends it).
fn headers_only(raw: &[u8]) -> Vec<u8> {
    match raw.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => raw[..i + 4].to_vec(),
        None => match raw.windows(2).position(|w| w == b"\n\n") {
            Some(i) => raw[..i + 2].to_vec(),
            None => raw.to_vec(),
        },
    }
}

/// The folder's next UID, used up.
fn take_uid(tx: &rusqlite::Transaction<'_>, account_id: &str, folder: &str) -> Result<i64> {
    let uid: i64 = tx.query_row(
        "UPDATE graph_folders SET next_uid = next_uid + 1 WHERE account_id = ? AND folder_id = ? RETURNING next_uid - 1",
        params![account_id, folder],
        |r| r.get(0),
    )?;
    Ok(uid)
}

fn insert_row(tx: &rusqlite::Transaction<'_>, account_id: &str, graph_id: &str, folder: &str, uid: i64, s: &Seen, modseq: i64) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO graph_messages(account_id, graph_id, folder_id, uid, received, is_read, flagged, is_draft, message_id, modseq)
         VALUES(?,?,?,?,?,?,?,?,?,?)",
        params![
            account_id,
            graph_id,
            folder,
            uid,
            s.received.unwrap_or(0),
            s.is_read.unwrap_or(false),
            s.flagged.unwrap_or(false),
            s.is_draft.unwrap_or(false),
            s.message_id,
            modseq
        ],
    )?;
    Ok(())
}

/// Apply one round of change tracking to a folder's mirror. `full`: the
/// round listed the whole folder, so what it didn't mention is gone.
fn apply_changes(store: &Store, account_id: &str, folder: &str, changes: Vec<(String, Option<Seen>)>, full: bool, delta_link: Option<&str>) -> Result<()> {
    let mut db = store.db();
    let tx = db.transaction()?;
    tx.execute("INSERT OR IGNORE INTO graph_folders(account_id, folder_id) VALUES(?, ?)", params![account_id, folder])?;
    let modseq: i64 =
        tx.query_row("SELECT modseq FROM graph_folders WHERE account_id = ? AND folder_id = ?", params![account_id, folder], |r| r.get(0))?;
    let next = modseq + 1;
    let mut changed = false;
    // The latest word on each message wins.
    let mut latest: HashMap<String, Option<Seen>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for (id, c) in changes {
        if latest.insert(id.clone(), c).is_none() {
            order.push(id);
        }
    }
    let mut arrivals: Vec<(String, Seen)> = Vec::new();
    for id in &order {
        let current: Option<(String, bool, bool, bool)> = tx
            .query_row(
                "SELECT folder_id, is_read, flagged, is_draft FROM graph_messages WHERE account_id = ? AND graph_id = ?",
                params![account_id, id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        match (latest.remove(id).flatten(), current) {
            // Removed here; a message we already filed elsewhere stays there.
            (None, Some((f, ..))) if f == folder => {
                tx.execute("DELETE FROM graph_messages WHERE account_id = ? AND graph_id = ?", params![account_id, id])?;
                changed = true;
            }
            (None, _) => {}
            (Some(s), Some((f, read, flagged, draft))) if f == folder => {
                let now = (s.is_read.unwrap_or(read), s.flagged.unwrap_or(flagged), s.is_draft.unwrap_or(draft));
                if now != (read, flagged, draft) {
                    tx.execute(
                        "UPDATE graph_messages SET is_read = ?, flagged = ?, is_draft = ?, modseq = ? WHERE account_id = ? AND graph_id = ?",
                        params![now.0, now.1, now.2, next, account_id, id],
                    )?;
                    changed = true;
                }
            }
            // New here, or moved in from another folder: a new UID, as over IMAP.
            (Some(s), _) => arrivals.push((id.clone(), s)),
        }
    }
    arrivals.sort_by_key(|(_, s)| s.received.unwrap_or(0));
    for (id, s) in &arrivals {
        let uid = take_uid(&tx, account_id, folder)?;
        insert_row(&tx, account_id, id, folder, uid, s, next)?;
        changed = true;
    }
    if full {
        let mentioned: HashSet<&String> = order.iter().collect();
        let mut stmt = tx.prepare("SELECT graph_id FROM graph_messages WHERE account_id = ? AND folder_id = ?")?;
        let held: Vec<String> = stmt.query_map(params![account_id, folder], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        for id in held.iter().filter(|id| !mentioned.contains(id)) {
            tx.execute("DELETE FROM graph_messages WHERE account_id = ? AND graph_id = ?", params![account_id, id])?;
            changed = true;
        }
    }
    tx.execute(
        "UPDATE graph_folders SET delta_link = ?, modseq = ? WHERE account_id = ? AND folder_id = ?",
        params![delta_link, if changed { next } else { modseq }, account_id, folder],
    )?;
    tx.commit()?;
    Ok(())
}

/// Forget everything stored for an account's mail (its mirror and its
/// messages), before it switches between IMAP and Graph: UIDs and folder
/// paths don't carry over.
pub fn reset_account(store: &Store, account_id: &str) -> Result<()> {
    let mut db = store.db();
    let tx = db.transaction()?;
    tx.execute("DELETE FROM graph_messages WHERE account_id = ?", [account_id])?;
    tx.execute("DELETE FROM graph_folders WHERE account_id = ?", [account_id])?;
    tx.execute("DELETE FROM messages WHERE account_id = ?", [account_id])?;
    tx.execute("DELETE FROM folder_state WHERE account_id = ?", [account_id])?;
    tx.execute("DELETE FROM folders WHERE account_id = ?", [account_id])?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::account;

    fn seen(received: i64, read: bool) -> Option<Seen> {
        Some(Seen { received: Some(received), is_read: Some(read), flagged: Some(false), is_draft: Some(false), message_id: Some(format!("m{received}@x")) })
    }

    fn uids(store: &Store, acc: &str, folder: &str) -> Vec<(String, u32, bool, i64)> {
        let db = store.db();
        let mut stmt = db.prepare("SELECT graph_id, uid, is_read, modseq FROM graph_messages WHERE account_id = ? AND folder_id = ? ORDER BY uid").unwrap();
        stmt.query_map(params![acc, folder], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u32, r.get(2)?, r.get(3)?))).unwrap().map(Result::unwrap).collect()
    }

    fn folder_state(store: &Store, acc: &str, folder: &str) -> (Option<String>, i64, i64) {
        store
            .db()
            .query_row("SELECT delta_link, next_uid, modseq FROM graph_folders WHERE account_id = ? AND folder_id = ?", params![acc, folder], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap()
    }

    #[test]
    fn mirror_follows_change_tracking() {
        let store = Store::open_in_memory().unwrap();
        store.db().execute_batch(SCHEMA).unwrap();
        let acc = account(&store, "me@outlook.com").id;
        // First round: UIDs in arrival order, whatever order Graph listed them in.
        apply_changes(&store, &acc, "in", vec![("b".into(), seen(200, false)), ("a".into(), seen(100, true))], true, Some("d1")).unwrap();
        assert_eq!(uids(&store, &acc, "in"), vec![("a".into(), 1, true, 2), ("b".into(), 2, false, 2)]);
        assert_eq!(folder_state(&store, &acc, "in"), (Some("d1".into()), 3, 2));

        // Nothing changed: the change counter stays.
        apply_changes(&store, &acc, "in", vec![("a".into(), seen(100, true))], false, Some("d2")).unwrap();
        assert_eq!(folder_state(&store, &acc, "in"), (Some("d2".into()), 3, 2));

        // b read elsewhere, c arrives, a deleted.
        apply_changes(&store, &acc, "in", vec![("b".into(), seen(200, true)), ("c".into(), seen(300, false)), ("a".into(), None)], false, Some("d3")).unwrap();
        assert_eq!(uids(&store, &acc, "in"), vec![("b".into(), 2, true, 3), ("c".into(), 3, false, 3)]);

        // c moved to another folder by another app: a new UID there; the
        // old folder's removal notice, coming later, leaves it alone.
        apply_changes(&store, &acc, "arch", vec![("c".into(), seen(300, false))], true, Some("x1")).unwrap();
        apply_changes(&store, &acc, "in", vec![("c".into(), None)], false, Some("d4")).unwrap();
        assert_eq!(uids(&store, &acc, "arch"), vec![("c".into(), 1, false, 2)]);
        assert_eq!(uids(&store, &acc, "in"), vec![("b".into(), 2, true, 3)]);

        // A full listing (tracking expired) drops what it didn't mention.
        apply_changes(&store, &acc, "in", vec![("d".into(), seen(400, false))], true, Some("d5")).unwrap();
        assert_eq!(uids(&store, &acc, "in"), vec![("d".into(), 4, false, 4)]);

        reset_account(&store, &acc).unwrap();
        assert!(uids(&store, &acc, "arch").is_empty());
    }

    #[test]
    fn small_pieces() {
        assert_eq!(headers_only(b"Subject: a\r\n\r\nbody"), b"Subject: a\r\n\r\n");
        assert_eq!(headers_only(b"Subject: a\n\nbody"), b"Subject: a\n\n");
        assert_eq!(with_bcc(b"To: a@x\r\n\r\nhi", &["b@x".into(), " c@x ".into()]), b"Bcc: b@x, c@x\r\nTo: a@x\r\n\r\nhi");
        assert_eq!(with_bcc(b"To: a@x\r\n\r\nhi", &[]), b"To: a@x\r\n\r\nhi");
        assert_ne!(uid_validity("AAMk1"), uid_validity("AAMk2"));
        assert!(uid_validity("") > 0);
        assert_eq!(bare_message_id(" <a@b> ").as_deref(), Some("a@b"));
        let q = crate::search::parse("from:sara invoice has:attachment", 0);
        let k = to_kql(&q);
        assert!(k.contains("from:sara") && k.contains("invoice") && k.contains("hasattachments:true"), "{k}");
    }
}
