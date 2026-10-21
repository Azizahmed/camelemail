//! Calendar: CalDAV accounts and their calendars, cached in SQLite and
//! synced both ways, Outlook calendars over Microsoft Graph (connected by
//! themselves for Microsoft mail accounts), plus read-only iCal
//! subscriptions; expanded into event instances for the UI; weather from
//! Open-Meteo for the calendar's days; reminder notifications.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::caldav::client::{CalDavClient, DavCredentials, DavError};
use crate::caldav::event::{self, EditScope, EventInput, Occurrence};
use crate::caldav::feed::{self, FeedObject};
use crate::caldav::ical::Component;
use crate::caldav::Tz;
use crate::engine::Engine;
use crate::graph::{self, GraphClient, GraphError};
use crate::model::EngineEvent;
use crate::store::{new_id, now_ms, Store};
use remail_proto::sections;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS cal_accounts (
  id TEXT PRIMARY KEY,
  mail_account_id TEXT REFERENCES accounts(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  url TEXT NOT NULL,
  username TEXT NOT NULL,
  allow_invalid_certs INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'ok',
  last_error TEXT,
  last_sync INTEGER,
  created_at INTEGER NOT NULL,
  -- `caldav` | `ics` (an iCal subscription: one read-only calendar at `url`)
  -- | `graph` (a Microsoft account's Outlook calendars)
  kind TEXT NOT NULL DEFAULT 'caldav',
  -- Seconds between refreshes the feed asks for
  refresh_secs INTEGER
);
CREATE TABLE IF NOT EXISTS calendars (
  id TEXT PRIMARY KEY,
  cal_account_id TEXT NOT NULL REFERENCES cal_accounts(id) ON DELETE CASCADE,
  href TEXT NOT NULL,
  name TEXT NOT NULL,
  color TEXT NOT NULL,
  ctag TEXT,
  read_only INTEGER NOT NULL DEFAULT 0,
  enabled INTEGER NOT NULL DEFAULT 1,
  sort_order INTEGER NOT NULL DEFAULT 0,
  UNIQUE (cal_account_id, href)
);
CREATE TABLE IF NOT EXISTS cal_objects (
  calendar_id TEXT NOT NULL REFERENCES calendars(id) ON DELETE CASCADE,
  href TEXT NOT NULL,
  etag TEXT NOT NULL,
  uid TEXT,
  ics TEXT NOT NULL,
  -- First start / last end in ms for range queries (i64::MAX end = repeats
  -- forever; NULL span = nothing to show). DATE and floating times are padded by 14 h.
  span_start INTEGER,
  span_end INTEGER,
  summary TEXT NOT NULL DEFAULT '',
  -- Lower-cased SUMMARY, LOCATION, DESCRIPTION and people of its events, for search.
  search_text TEXT,
  PRIMARY KEY (calendar_id, href)
);
CREATE INDEX IF NOT EXISTS idx_cal_objects_span ON cal_objects(span_start, span_end);
"#;

/// Columns added after the first calendar release (run after `SCHEMA`).
pub(crate) fn upgrade(conn: &rusqlite::Connection) -> Result<()> {
    let has = |table: &str, col: &str| -> Result<bool> {
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM pragma_table_info(?) WHERE name = ?", [table, col], |r| r.get(0))?;
        Ok(n > 0)
    };
    if !has("cal_accounts", "kind")? {
        conn.execute_batch("ALTER TABLE cal_accounts ADD COLUMN kind TEXT NOT NULL DEFAULT 'caldav';")?;
    }
    if !has("cal_accounts", "refresh_secs")? {
        conn.execute_batch("ALTER TABLE cal_accounts ADD COLUMN refresh_secs INTEGER;")?;
    }
    if !has("cal_objects", "search_text")? {
        conn.execute_batch("ALTER TABLE cal_objects ADD COLUMN search_text TEXT;")?;
    }
    // Objects stored before search_text: fill it in, and their spans the current way.
    let tx = conn.unchecked_transaction()?;
    let rows: Vec<(String, String, String)> = {
        let mut stmt = tx.prepare("SELECT calendar_id, href, ics FROM cal_objects WHERE search_text IS NULL")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let tz = local_tz();
    for (calendar_id, href, ics) in rows {
        let d = derive(&ics, tz);
        tx.execute(
            "UPDATE cal_objects SET search_text = ?, span_start = ?, span_end = ? WHERE calendar_id = ? AND href = ?",
            params![d.search_text, d.span_start, d.span_end, calendar_id, href],
        )?;
    }
    // Open-ended spans were NULL once (which kept the range query off the index).
    tx.execute("UPDATE cal_objects SET span_end = ? WHERE span_end IS NULL AND span_start IS NOT NULL", [OPEN_END])?;
    tx.commit()?;
    Ok(())
}

pub const KIND_CALDAV: &str = "caldav";
pub const KIND_ICS: &str = "ics";
pub const KIND_GRAPH: &str = "graph";
/// Setting: Microsoft mail accounts whose Outlook calendars were disconnected
/// (so they aren't connected again by themselves).
pub(crate) const OUTLOOK_OFF: &str = "cal_outlook_off";
const DAY_MS: i64 = 86_400_000;
/// `span_end` of objects that repeat forever.
const OPEN_END: i64 = i64::MAX;
/// Largest iCal feed we download.
const MAX_FEED_BYTES: usize = 40 << 20;

/// Colours for calendars the server doesn't colour.
pub(crate) const PALETTE: &[&str] = &["#ff7c19", "#007aff", "#34c759", "#af52de", "#ff3b30", "#ffcc00", "#00c7be", "#ff2d55"];
const SYNC_EVERY: Duration = Duration::from_secs(5 * 60);

// ----------------------------------------------------------------------------
// Models
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarView {
    pub id: String,
    pub account_id: String,
    pub account_name: String,
    pub name: String,
    pub color: String,
    pub read_only: bool,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalAccountView {
    pub id: String,
    /// `caldav` | `ics` | `graph`
    pub kind: String,
    pub mail_account_id: Option<String>,
    pub name: String,
    pub url: String,
    pub username: String,
    /// `ok` | `error` | `signin` (the Microsoft account must be signed in again)
    pub status: String,
    pub last_error: Option<String>,
    pub last_sync: Option<i64>,
    pub calendars: Vec<CalendarView>,
}

/// A calendar account as it syncs between devices.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SyncedCalAccount {
    pub id: String,
    pub kind: String,
    pub mail_account_id: Option<String>,
    pub name: String,
    pub url: String,
    pub username: String,
    pub created_at: i64,
}

/// One event instance for the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalEvent {
    pub key: String,
    pub calendar_id: String,
    pub href: String,
    pub color: String,
    pub read_only: bool,
    #[serde(flatten)]
    pub occurrence: Occurrence,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalConnect {
    pub mail_account_id: Option<String>,
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(default)]
    pub allow_invalid_certs: bool,
}

/// Subscribe to an iCal (`.ics` / `webcal://`) link.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalSubscribe {
    pub url: String,
    pub name: Option<String>,
    /// For feeds behind a password (most aren't).
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Place {
    pub name: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub country: String,
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DayWeather {
    /// `YYYY-MM-DD` (local to the place).
    pub date: String,
    /// WMO weather code.
    pub code: i64,
    /// °C
    pub max: f64,
    pub min: f64,
}

/// Calendar preferences (part of the app settings).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CalendarPrefs {
    /// 0 = Sunday, 1 = Monday, 6 = Saturday.
    pub week_start: u8,
    pub week_numbers: bool,
    pub weather: Option<Place>,
    /// `c` | `f`
    pub units: String,
    pub default_calendar: Option<String>,
    /// `day` | `week` | `month` | `quarter` | `year`
    pub view: String,
}

impl Default for CalendarPrefs {
    fn default() -> Self {
        Self { week_start: 0, week_numbers: true, weather: None, units: "c".into(), default_calendar: None, view: "month".into() }
    }
}

/// The computer's time zone (for all-day and floating times).
pub fn local_tz() -> Tz {
    iana_time_zone::get_timezone().ok().and_then(|n| Tz::from_str(&n).ok()).unwrap_or(Tz::UTC)
}

// ----------------------------------------------------------------------------
// Storage
// ----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct AccountRow {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) refresh_secs: Option<i64>,
    pub(crate) mail_account_id: Option<String>,
    pub(crate) name: String,
    pub(crate) url: String,
    pub(crate) username: String,
    pub(crate) allow_invalid_certs: bool,
    pub(crate) status: String,
    pub(crate) last_error: Option<String>,
    pub(crate) last_sync: Option<i64>,
}

#[derive(Debug, Clone)]
struct CalendarRow {
    id: String,
    account_id: String,
    href: String,
    ctag: Option<String>,
    read_only: bool,
}

/// An object to show (its text is read only when it isn't parsed already).
struct ObjectRow {
    calendar_id: String,
    href: String,
    etag: String,
    color: String,
    read_only: bool,
    /// From an Outlook calendar (instances that belong to a series).
    outlook: bool,
}

impl Store {
    pub(crate) fn cal_account_rows(&self) -> Result<Vec<AccountRow>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT id, mail_account_id, name, url, username, allow_invalid_certs, status, last_error, last_sync, kind, refresh_secs
             FROM cal_accounts ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(AccountRow {
                id: r.get(0)?,
                kind: r.get(9)?,
                refresh_secs: r.get(10)?,
                mail_account_id: r.get(1)?,
                name: r.get(2)?,
                url: r.get(3)?,
                username: r.get(4)?,
                allow_invalid_certs: r.get(5)?,
                status: r.get(6)?,
                last_error: r.get(7)?,
                last_sync: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn cal_account_row(&self, id: &str) -> Result<Option<AccountRow>> {
        Ok(self.cal_account_rows()?.into_iter().find(|a| a.id == id))
    }

    /// Every calendar: `(id, calendar account, href)`.
    fn calendar_hrefs(&self) -> Result<Vec<(String, String, String)>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT id, cal_account_id, href FROM calendars")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Calendar accounts as they sync to the Remail server (not Outlook
    /// calendars: each device connects those by itself).
    pub(crate) fn synced_cal_accounts(&self) -> Result<Vec<SyncedCalAccount>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT id, kind, mail_account_id, name, url, username, created_at FROM cal_accounts WHERE kind != ? ORDER BY created_at")?;
        let rows = stmt.query_map([KIND_GRAPH], |r| {
            Ok(SyncedCalAccount {
                id: r.get(0)?,
                kind: r.get(1)?,
                mail_account_id: r.get(2)?,
                name: r.get(3)?,
                url: r.get(4)?,
                username: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Colour and visibility of every calendar, by `override_key`.
    pub(crate) fn calendar_looks(&self) -> Result<Vec<(String, String, bool)>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT a.kind, a.id, a.mail_account_id, c.href, c.color, c.enabled FROM calendars c JOIN cal_accounts a ON a.id = c.cal_account_id",
        )?;
        let rows = stmt.query_map([], |r| {
            let (kind, id, mail): (String, String, Option<String>) = (r.get(0)?, r.get(1)?, r.get(2)?);
            let href: String = r.get(3)?;
            Ok((crate::cloud::override_key(&cal_key(&kind, &id, mail.as_deref()), &href), r.get(4)?, r.get(5)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The portable reference (`(calendar account key, href)`) of a calendar.
    pub(crate) fn calendar_ref(&self, calendar_id: &str) -> Result<Option<(String, String)>> {
        Ok(self
            .db()
            .query_row(
                "SELECT a.kind, a.id, a.mail_account_id, c.href FROM calendars c JOIN cal_accounts a ON a.id = c.cal_account_id WHERE c.id = ?",
                [calendar_id],
                |r| {
                    let (kind, id, mail): (String, String, Option<String>) = (r.get(0)?, r.get(1)?, r.get(2)?);
                    Ok((cal_key(&kind, &id, mail.as_deref()), r.get::<_, String>(3)?))
                },
            )
            .optional()?)
    }

    /// The local calendar a portable reference points at, if it's here.
    pub(crate) fn calendar_by_ref(&self, key: &str, href: &str) -> Result<Option<String>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT c.id, a.kind, a.id, a.mail_account_id FROM calendars c JOIN cal_accounts a ON a.id = c.cal_account_id WHERE c.href = ?",
        )?;
        let rows = stmt.query_map([href], |r| {
            let (kind, id, mail): (String, String, Option<String>) = (r.get(1)?, r.get(2)?, r.get(3)?);
            Ok((r.get::<_, String>(0)?, cal_key(&kind, &id, mail.as_deref())))
        })?;
        for row in rows {
            let (cal, k) = row?;
            if k == key {
                return Ok(Some(cal));
            }
        }
        Ok(None)
    }

    fn calendar_views(&self) -> Result<Vec<CalendarView>> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT c.id, c.cal_account_id, a.name, c.name, c.color, c.read_only, c.enabled
             FROM calendars c JOIN cal_accounts a ON a.id = c.cal_account_id
             ORDER BY a.created_at, c.sort_order, c.name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(CalendarView {
                id: r.get(0)?,
                account_id: r.get(1)?,
                account_name: r.get(2)?,
                name: r.get(3)?,
                color: r.get(4)?,
                read_only: r.get(5)?,
                enabled: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn calendar_row(&self, id: &str) -> Result<Option<CalendarRow>> {
        Ok(self
            .db()
            .query_row(
                "SELECT id, cal_account_id, href, ctag, read_only FROM calendars WHERE id = ?",
                [id],
                |r| Ok(CalendarRow { id: r.get(0)?, account_id: r.get(1)?, href: r.get(2)?, ctag: r.get(3)?, read_only: r.get(4)? }),
            )
            .optional()?)
    }

    fn calendar_rows(&self, account_id: &str) -> Result<Vec<CalendarRow>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT id, cal_account_id, href, ctag, read_only FROM calendars WHERE cal_account_id = ?")?;
        let rows = stmt.query_map([account_id], |r| {
            Ok(CalendarRow { id: r.get(0)?, account_id: r.get(1)?, href: r.get(2)?, ctag: r.get(3)?, read_only: r.get(4)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn object_etags(&self, calendar_id: &str) -> Result<HashMap<String, String>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT href, etag FROM cal_objects WHERE calendar_id = ?")?;
        let rows = stmt.query_map([calendar_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn object(&self, calendar_id: &str, href: &str) -> Result<Option<(String, String)>> {
        Ok(self
            .db()
            .query_row("SELECT etag, ics FROM cal_objects WHERE calendar_id = ? AND href = ?", params![calendar_id, href], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?)
    }

    fn put_object(&self, calendar_id: &str, href: &str, etag: &str, ics: &str, tz: Tz) -> Result<()> {
        let d = derive(ics, tz);
        put_object_on(&self.db(), calendar_id, href, etag, ics, &d)
    }

    /// Make a calendar hold exactly `objects` (an iCal feed's), in one transaction.
    fn replace_objects(&self, calendar_id: &str, objects: &[FeedObject], tz: Tz) -> Result<Applied> {
        let known = self.object_etags(calendar_id)?;
        let keep: HashSet<&str> = objects.iter().map(|o| o.href.as_str()).collect();
        let removed: Vec<String> = known.keys().filter(|h| !keep.contains(h.as_str())).cloned().collect();
        let changed: Vec<&FeedObject> = objects.iter().filter(|o| known.get(&o.href) != Some(&o.etag)).collect();
        self.write_objects(calendar_id, &changed, &removed, tz)
    }

    /// Store and remove objects in one transaction. Parsing and expanding
    /// them happens first, without holding the database.
    fn write_objects(&self, calendar_id: &str, changed: &[&FeedObject], removed: &[String], tz: Tz) -> Result<Applied> {
        let derived: Vec<Derived> = changed.iter().map(|o| derive(&o.ics, tz)).collect();
        let mut db = self.db();
        let tx = db.transaction()?;
        let mut applied = Applied::default();
        for href in removed {
            if tx.execute("DELETE FROM cal_objects WHERE calendar_id = ? AND href = ?", params![calendar_id, href])? > 0 {
                applied.removed.push(href.clone());
            }
        }
        for (o, d) in changed.iter().zip(&derived) {
            put_object_on(&tx, calendar_id, &o.href, &o.etag, &o.ics, d)?;
        }
        tx.commit()?;
        applied.written = changed.len();
        Ok(applied)
    }
}

/// What a batch of writes changed.
#[derive(Debug, Default)]
struct Applied {
    removed: Vec<String>,
    written: usize,
}

impl Store {
    fn object_texts(&self, calendar_id: &str) -> Result<Vec<String>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT ics FROM cal_objects WHERE calendar_id = ?")?;
        let rows = stmt.query_map([calendar_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Apply a round of changes (a CalDAV or Outlook calendar's), in one transaction.
    fn apply_changes(&self, calendar_id: &str, changed: &[FeedObject], removed: &[String], tz: Tz) -> Result<Applied> {
        self.write_objects(calendar_id, &changed.iter().collect::<Vec<_>>(), removed, tz)
    }
}

/// What is stored next to an object's text: its span (for range queries),
/// UID, titles and search text.
struct Derived {
    uid: Option<String>,
    span_start: Option<i64>,
    span_end: Option<i64>,
    summary: String,
    search_text: String,
}

/// Parse and expand an object (done before the database is locked).
fn derive(ics: &str, tz: Tz) -> Derived {
    let parsed = Component::parse(ics).ok();
    let (span_start, span_end) = match parsed.as_ref().and_then(|c| event::object_span(c, tz)) {
        Some((s, e)) => (Some(s), Some(e.unwrap_or(OPEN_END))),
        None => (None, None),
    };
    let events: Vec<&Component> = parsed.as_ref().map(|c| c.children_named("VEVENT").collect()).unwrap_or_default();
    let texts = |names: &[&str]| events.iter().flat_map(|v| names.iter().filter_map(|n| v.prop(n).map(|p| p.text()))).collect::<Vec<_>>();
    // People are found by name or address (`cal_search` checks each occurrence again).
    let people = events.iter().flat_map(|v| v.props("ORGANIZER").chain(v.props("ATTENDEE"))).map(|p| {
        let email = p.value.get(..7).filter(|s| s.eq_ignore_ascii_case("mailto:")).map_or(p.value.as_str(), |_| &p.value[7..]);
        format!("{} {email}", p.param("CN").unwrap_or_default())
    });
    let mut search = texts(&["SUMMARY", "LOCATION", "DESCRIPTION"]);
    search.extend(people);
    Derived {
        uid: parsed.as_ref().and_then(event::object_uid),
        span_start,
        span_end,
        summary: texts(&["SUMMARY"]).join(" "),
        search_text: search.join("\n").to_lowercase(),
    }
}

fn put_object_on(conn: &rusqlite::Connection, calendar_id: &str, href: &str, etag: &str, ics: &str, d: &Derived) -> Result<()> {
    conn.execute(
        "INSERT INTO cal_objects(calendar_id, href, etag, uid, ics, span_start, span_end, summary, search_text) VALUES(?,?,?,?,?,?,?,?,?)
         ON CONFLICT(calendar_id, href) DO UPDATE SET etag = excluded.etag, uid = excluded.uid, ics = excluded.ics,
           span_start = excluded.span_start, span_end = excluded.span_end, summary = excluded.summary, search_text = excluded.search_text",
        params![calendar_id, href, etag, d.uid, ics, d.span_start, d.span_end, d.summary, d.search_text],
    )?;
    Ok(())
}

impl Store {

    fn delete_object(&self, calendar_id: &str, href: &str) -> Result<()> {
        self.db().execute("DELETE FROM cal_objects WHERE calendar_id = ? AND href = ?", params![calendar_id, href])?;
        Ok(())
    }

    fn objects_in_range(&self, from: i64, to: i64) -> Result<Vec<ObjectRow>> {
        // `>=`: an event of no length starting at `from` is in range.
        self.object_rows("o.span_start < ?2 AND o.span_end >= ?1", params![from, to])
    }

    /// Objects whose title, place or notes contain `query` (any case).
    fn objects_matching(&self, query: &str) -> Result<Vec<ObjectRow>> {
        let q = query.to_lowercase().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        self.object_rows("o.search_text LIKE ?1 ESCAPE '\\' LIMIT 300", params![format!("%{q}%")])
    }

    fn object_rows(&self, filter: &str, args: impl rusqlite::Params) -> Result<Vec<ObjectRow>> {
        let db = self.db();
        let mut stmt = db.prepare(&format!(
            "SELECT o.calendar_id, o.href, o.etag, c.color, c.read_only, a.kind = 'graph'
             FROM cal_objects o JOIN calendars c ON c.id = o.calendar_id JOIN cal_accounts a ON a.id = c.cal_account_id
             WHERE c.enabled = 1 AND {filter}"
        ))?;
        let rows = stmt.query_map(args, |r| {
            Ok(ObjectRow { calendar_id: r.get(0)?, href: r.get(1)?, etag: r.get(2)?, color: r.get(3)?, read_only: r.get(4)?, outlook: r.get(5)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

// ----------------------------------------------------------------------------
// Engine API
// ----------------------------------------------------------------------------

/// `(calendar id, href)` → `(etag, parsed object)`.
type ParsedCache = HashMap<(String, String), (String, Arc<Component>)>;

/// Parsed objects (reused while their etag is unchanged), the weather cache
/// and the sync loop's wake-up.
#[derive(Default)]
pub(crate) struct CalState {
    parsed: Mutex<ParsedCache>,
    /// `(place, fetched at, days)`
    weather: Mutex<Option<(Place, i64, Vec<DayWeather>)>>,
    pub(crate) wake: tokio::sync::Notify,
    /// CalDAV syncs and writes take turns.
    syncing: tokio::sync::Mutex<()>,
    /// Account id → when its last sync started (so failing feeds aren't hammered).
    attempts: Mutex<HashMap<String, i64>>,
    /// iCal subscriptions being refreshed right now (one refresh per feed at a time).
    feeds_in_flight: Mutex<HashSet<String>>,
    /// Outlook accounts: syncs and writes take turns per account (not with CalDAV).
    graph_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// A background sync is running (the next waits for it).
    sync_running: AtomicBool,
    /// A sync changed what's shown since the UI was last told.
    changed: AtomicBool,
}

impl CalState {
    /// Forget cached events, weather and sync times (signed out).
    pub(crate) fn forget_all(&self) {
        self.parsed.lock().unwrap().clear();
        *self.weather.lock().unwrap() = None;
        self.attempts.lock().unwrap().clear();
    }
}

/// What to do locally once Microsoft has taken a write.
enum AfterWrite {
    Nothing,
    /// The event as Microsoft returned it.
    Store(FeedObject),
    /// Fetch the calendar's changes (a series was laid out again).
    Refresh,
}

/// What an iCal feed's server answered.
enum Fetched {
    NotModified,
    Body { text: String, tag: FeedTag },
}

/// Remembered per feed (in `calendars.ctag`) for conditional requests and change detection.
#[derive(Debug, Default, Clone, PartialEq)]
struct FeedTag {
    etag: Option<String>,
    modified: Option<String>,
    hash: String,
}

impl FeedTag {
    fn encode(&self) -> String {
        format!("{}\n{}\n{}", self.etag.as_deref().unwrap_or(""), self.modified.as_deref().unwrap_or(""), self.hash)
    }

    fn decode(s: &str) -> FeedTag {
        let mut it = s.splitn(3, '\n').map(|p| Some(p.to_string()).filter(|p| !p.is_empty()));
        FeedTag { etag: it.next().flatten(), modified: it.next().flatten(), hash: it.next().flatten().unwrap_or_default() }
    }
}

impl Engine {
    fn cal(&self) -> &CalState {
        &self.inner_ref().cal
    }

    /// Note that a sync changed what's shown.
    fn cal_changed(&self) {
        self.cal().changed.store(true, Ordering::Release);
    }

    /// After a batch of writes: forget parsed copies of what's gone.
    fn applied(&self, calendar_id: &str, a: &Applied) {
        if !a.removed.is_empty() {
            let gone: HashSet<&str> = a.removed.iter().map(String::as_str).collect();
            self.cal().parsed.lock().unwrap().retain(|(c, h), _| c != calendar_id || !gone.contains(h.as_str()));
        }
        if a.written > 0 || !a.removed.is_empty() {
            self.cal_changed();
        }
    }

    /// Forget parsed copies of the objects of deleted calendars.
    fn forget_calendars(&self, ids: &[String]) {
        self.cal().parsed.lock().unwrap().retain(|(c, _), _| !ids.contains(c));
    }

    fn delete_stored(&self, calendar_id: &str, href: &str) -> Result<()> {
        self.store().delete_object(calendar_id, href)?;
        self.applied(calendar_id, &Applied { removed: vec![href.to_string()], written: 0 });
        Ok(())
    }

    async fn dav_client(&self, a: &AccountRow) -> Result<CalDavClient> {
        // A Google account's calendars sign in with its Google token.
        if let Some(mail) = a.mail_account_id.as_deref() {
            if self.store().account(mail)?.is_some_and(|m| m.auth == crate::oauth::GOOGLE) {
                return CalDavClient::with_bearer(self.calendar_token(mail).await?, a.allow_invalid_certs);
            }
        }
        let secret_id = a.mail_account_id.clone().unwrap_or_else(|| format!("cal:{}", a.id));
        let password = self.secrets().get(&secret_id)?.context("no saved password for this calendar account")?;
        CalDavClient::new(DavCredentials { username: a.username.clone(), password, allow_invalid_certs: a.allow_invalid_certs })
    }

    pub fn cal_accounts(&self) -> Result<Vec<CalAccountView>> {
        let cals = self.store().calendar_views()?;
        Ok(self
            .store()
            .cal_account_rows()?
            .into_iter()
            .map(|a| CalAccountView {
                calendars: cals.iter().filter(|c| c.account_id == a.id).cloned().collect(),
                id: a.id,
                kind: a.kind,
                mail_account_id: a.mail_account_id,
                name: a.name,
                url: a.url,
                username: a.username,
                status: a.status,
                last_error: a.last_error,
                last_sync: a.last_sync,
            })
            .collect())
    }

    pub fn cal_calendars(&self) -> Result<Vec<CalendarView>> {
        self.store().calendar_views()
    }

    /// Find the CalDAV server for a mail account (or the given URL), store it and sync.
    pub async fn cal_connect(&self, req: CalConnect) -> Result<CalAccountView> {
        let (name, username, password, allow_invalid, urls, mail_id) = match &req.mail_account_id {
            Some(mid) => {
                let acc = self.store().account(mid)?.context("account not found")?;
                let host = acc.imap.host.to_lowercase();
                if acc.auth == crate::oauth::MICROSOFT {
                    return self.connect_outlook_calendar(mid, true).await;
                }
                if acc.auth == crate::oauth::GOOGLE {
                    return self.connect_google_calendar(&acc).await;
                }
                if host.ends_with("office365.com") || host.ends_with("outlook.com") {
                    bail!("Outlook calendars come with Microsoft sign-in: in Settings → Accounts, choose \"Sign in again\" for this account, and its calendars connect by themselves.");
                }
                if acc.is_gmail || host.contains("gmail") || host.contains("google") {
                    bail!("Google Calendar comes with Google sign-in: in Settings → Accounts, choose \"Sign in again\" for this account, then connect its calendar.");
                }
                let password = match req.password.clone().filter(|p| !p.is_empty()) {
                    Some(p) => p,
                    None => self.secrets().get(mid)?.context("no saved password for this account")?,
                };
                let urls = match req.url.clone().filter(|u| !u.trim().is_empty()) {
                    Some(u) => vec![u],
                    None => CalDavClient::guess_start_urls(&acc.email, &acc.imap.host),
                };
                let user = req.username.clone().filter(|u| !u.trim().is_empty()).unwrap_or(acc.username.clone());
                (acc.email.clone(), user, password, req.allow_invalid_certs || acc.allow_invalid_certs, urls, Some(mid.clone()))
            }
            None => {
                let url = req.url.clone().filter(|u| !u.trim().is_empty()).context("Enter the CalDAV server address")?;
                let user = req.username.clone().filter(|u| !u.trim().is_empty()).context("Enter the username")?;
                let pass = req.password.clone().filter(|p| !p.is_empty()).context("Enter the password")?;
                let host = reqwest::Url::parse(url.trim()).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_else(|| url.clone());
                (format!("{user} · {host}"), user, pass, req.allow_invalid_certs, vec![url.trim().to_string()], None)
            }
        };
        let client = CalDavClient::new(DavCredentials { username: username.clone(), password: password.clone(), allow_invalid_certs: allow_invalid })?;
        let mut home = None;
        let mut last_err: Option<anyhow::Error> = None;
        for u in &urls {
            match client.discover(u).await {
                Ok(h) => {
                    home = Some(h);
                    break;
                }
                Err(e) if matches!(e.downcast_ref::<DavError>(), Some(DavError::Auth)) => {
                    bail!("The calendar server rejected the username or password ({u}).")
                }
                Err(e) => {
                    tracing::debug!("CalDAV discovery at {u} failed: {e:#}");
                    last_err = Some(e);
                }
            }
        }
        let home = home.ok_or_else(|| {
            anyhow!(
                "Couldn't find a calendar server for this account{}. Use \"Other CalDAV account\" with your provider's CalDAV address.",
                last_err.map(|e| format!(" ({e:#})")).unwrap_or_default()
            )
        })?;
        let calendars = client.calendars(&home).await?;
        if !calendars.iter().any(|c| c.supports_events) {
            bail!("The server has no calendars with events for this account.");
        }
        let id = new_id("cal");
        if mail_id.is_none() {
            self.set_credential(&id, &password)?;
        }
        self.store().db().execute(
            "INSERT INTO cal_accounts(id, mail_account_id, name, url, username, allow_invalid_certs, created_at) VALUES(?,?,?,?,?,?,?)",
            params![id, mail_id, name, home, username, allow_invalid, now_ms()],
        )?;
        if let Err(e) = self.cal_sync_account(&id).await {
            // Don't leave a half-connected account behind (a retry would duplicate it).
            self.store().db().execute("DELETE FROM cal_accounts WHERE id = ?", [&id])?;
            if mail_id.is_none() {
                let _ = self.secrets().delete(&format!("cal:{id}"));
            }
            return Err(e.context("the first sync failed"));
        }
        self.calendars_changed(mail_id.is_none().then_some(id.as_str()));
        self.cal_accounts()?.into_iter().find(|a| a.id == id).context("account vanished")
    }

    /// Calendar setup changed here: sync it to the Remail server (with the
    /// calendar account's own credential, if it has one) and tell the UI.
    fn calendars_changed(&self, credential_of: Option<&str>) {
        self.mark_dirty(sections::CALENDARS);
        if let Some(id) = credential_of {
            self.mark_dirty(&sections::cred_section(id));
        }
        self.emit_event(EngineEvent::CalendarChanged);
    }

    /// Connect a Google account's calendars (CalDAV with its Google token).
    async fn connect_google_calendar(&self, acc: &crate::model::Account) -> Result<CalAccountView> {
        let token = self.calendar_token(&acc.id).await?;
        let client = CalDavClient::with_bearer(token, acc.allow_invalid_certs)?;
        let base = crate::engine::google_caldav_base();
        let mut home = None;
        let mut last_err = None;
        for start in [format!("{base}/{}/", acc.email), format!("{base}/{}/user", acc.email)] {
            match client.discover(&start).await {
                Ok(h) => {
                    home = Some(h);
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        let home = match (home, last_err) {
            (Some(h), _) => h,
            (None, Some(e)) => return Err(e.context("Couldn't find this Google account's calendars")),
            (None, None) => bail!("Couldn't find this Google account's calendars"),
        };
        let id = new_id("cal");
        self.store().db().execute(
            "INSERT INTO cal_accounts(id, mail_account_id, name, url, username, created_at) VALUES(?,?,?,?,?,?)",
            params![id, acc.id, acc.email, home, acc.email, now_ms()],
        )?;
        if let Err(e) = self.cal_sync_account(&id).await {
            self.store().db().execute("DELETE FROM cal_accounts WHERE id = ?", [&id])?;
            return Err(e.context("the first sync failed"));
        }
        self.calendars_changed(None);
        self.cal_accounts()?.into_iter().find(|a| a.id == id).context("account vanished")
    }

    /// The colour and visibility the user chose for a calendar on another
    /// device, for a calendar this device is about to show for the first time.
    fn calendar_override(&self, acc: &AccountRow, href: &str) -> (Option<String>, Option<bool>) {
        let all: HashMap<String, crate::cloud::CalOverride> = self.store().setting(crate::cloud::CAL_OVERRIDES).unwrap_or_default();
        match all.get(&crate::cloud::override_key(&cal_key(&acc.kind, &acc.id, acc.mail_account_id.as_deref()), href)) {
            Some(o) => (o.color.clone(), o.enabled),
            None => (None, None),
        }
    }

    /// Subscribe to an iCal link: a read-only calendar refreshed in the background.
    pub async fn cal_subscribe(&self, req: CalSubscribe) -> Result<CalAccountView> {
        let url = feed::normalize_url(&req.url)?;
        let username = req.username.unwrap_or_default().trim().to_string();
        let password = req.password.filter(|p| !p.is_empty() && !username.is_empty());
        let subscribed = |db: &rusqlite::Connection| -> Result<bool> {
            let n: i64 = db.query_row("SELECT COUNT(*) FROM cal_accounts WHERE kind = ? AND url = ?", params![KIND_ICS, url.as_str()], |r| r.get(0))?;
            Ok(n > 0)
        };
        if subscribed(&self.store().db())? {
            bail!("You're already subscribed to this calendar.");
        }
        let fetched = match self.fetch_feed(&url, &username, password.as_deref(), None).await {
            // webcal:// links don't say whether the server speaks https. Never
            // fall back to plain http by ourselves: someone on the network could
            // force that and read the link's secret token.
            Err(e) if req.url.trim().to_ascii_lowercase().starts_with("webcal://") && e.downcast_ref::<reqwest::Error>().is_some_and(|r| r.is_connect()) => {
                return Err(e.context(format!(
                    "Couldn't connect securely to {}. If this calendar is only offered over plain http, paste the link starting with http:// instead",
                    url.host_str().unwrap_or_default()
                )));
            }
            r => r?,
        };
        let Fetched::Body { text, tag } = fetched else { bail!("The calendar server sent no calendar") };
        let parsed = feed::parse_feed(&text)?;
        let host = url.host_str().unwrap_or_default().to_string();
        let name = req.name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).or(parsed.name.clone()).unwrap_or(host);
        let n_cals = self.store().calendar_views()?.len();
        let color = parsed.color.clone().unwrap_or_else(|| PALETTE[n_cals % PALETTE.len()].to_string());
        let id = new_id("cal");
        let cal_id = new_id("calendar");
        if let Some(p) = &password {
            self.set_credential(&id, p)?;
        }
        let stored = (|| -> Result<()> {
            let now = now_ms();
            let db = self.store().db();
            // Again, under the lock: two subscribes to the same link can race.
            if subscribed(&db)? {
                bail!("You're already subscribed to this calendar.");
            }
            db.execute(
                "INSERT INTO cal_accounts(id, mail_account_id, name, url, username, kind, refresh_secs, last_sync, created_at) VALUES(?,NULL,?,?,?,?,?,?,?)",
                params![id, name, url.as_str(), username, KIND_ICS, parsed.refresh_secs, now, now],
            )?;
            db.execute(
                "INSERT INTO calendars(id, cal_account_id, href, name, color, ctag, read_only, sort_order) VALUES(?,?,?,?,?,?,1,0)",
                params![cal_id, id, url.as_str(), name, color, tag.encode()],
            )?;
            Ok(())
        })()
        .and_then(|()| self.store().replace_objects(&cal_id, &parsed.objects, local_tz()).map(|_| ()));
        if let Err(e) = stored {
            self.store().db().execute("DELETE FROM cal_accounts WHERE id = ?", [&id])?; // no-op if it was never inserted
            let _ = self.secrets().delete(&format!("cal:{id}"));
            return Err(e);
        }
        self.cal().attempts.lock().unwrap().insert(id.clone(), now_ms());
        self.calendars_changed(password.is_some().then_some(id.as_str()));
        self.cal_accounts()?.into_iter().find(|a| a.id == id).context("subscription vanished")
    }

    /// Download a feed (conditionally, when we know its validators).
    async fn fetch_feed(&self, url: &reqwest::Url, username: &str, password: Option<&str>, prev: Option<&FeedTag>) -> Result<Fetched> {
        use reqwest::header::{ACCEPT, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED};
        let mut req = self.http().get(url.clone()).timeout(Duration::from_secs(60)).header(ACCEPT, "text/calendar, text/plain;q=0.8, */*;q=0.5");
        if !username.is_empty() {
            req = req.basic_auth(username, password);
        }
        if let Some(p) = prev {
            if let Some(e) = &p.etag {
                req = req.header(IF_NONE_MATCH, e);
            }
            if let Some(m) = &p.modified {
                req = req.header(IF_MODIFIED_SINCE, m);
            }
        }
        let host = url.host_str().unwrap_or_default().to_string();
        // reqwest errors name the URL; feed links carry secret tokens.
        let mut resp = req.send().await.map_err(|e| e.without_url()).with_context(|| format!("couldn't reach {host}"))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_MODIFIED && prev.is_some() {
            return Ok(Fetched::NotModified);
        }
        match status.as_u16() {
            401 | 403 => bail!("{host} refused access (HTTP {}). If the link needs a username and password, add them; otherwise copy the link again.", status.as_u16()),
            404 | 410 => bail!("This calendar link doesn't exist (any more) (HTTP {}). Copy the iCal link again from the calendar's sharing settings.", status.as_u16()),
            _ if !status.is_success() => bail!("{host} answered HTTP {status}"),
            _ => {}
        }
        let header = |h| resp.headers().get(h).and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok()).map(String::from);
        let (etag, modified) = (header(ETAG), header(LAST_MODIFIED));
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| e.without_url()).with_context(|| format!("downloading from {host}"))? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_FEED_BYTES {
                bail!("This calendar is too large (over {} MB)", MAX_FEED_BYTES >> 20);
            }
        }
        let text = String::from_utf8_lossy(&body).into_owned();
        let hash = feed::stable_hash(&text);
        Ok(Fetched::Body { text, tag: FeedTag { etag, modified, hash } })
    }

    /// Refresh an iCal subscription (skipped if it is being refreshed already).
    async fn ics_sync(&self, acc: &AccountRow) -> Result<()> {
        struct InFlight<'a>(&'a Mutex<HashSet<String>>, String);
        impl Drop for InFlight<'_> {
            fn drop(&mut self) {
                self.0.lock().unwrap().remove(&self.1);
            }
        }
        if !self.cal().feeds_in_flight.lock().unwrap().insert(acc.id.clone()) {
            return Ok(());
        }
        let _done = InFlight(&self.cal().feeds_in_flight, acc.id.clone());
        self.ics_refresh(acc).await
    }

    async fn ics_refresh(&self, acc: &AccountRow) -> Result<()> {
        let cal = self.store().calendar_rows(&acc.id)?.into_iter().next().context("this subscription has no calendar")?;
        let url = reqwest::Url::parse(&acc.url)?;
        let password = if acc.username.is_empty() { None } else { self.secrets().get(&format!("cal:{}", acc.id))? };
        let prev = cal.ctag.as_deref().map(FeedTag::decode);
        let (text, tag) = match self.fetch_feed(&url, &acc.username, password.as_deref(), prev.as_ref()).await? {
            Fetched::NotModified => return Ok(()),
            Fetched::Body { text, tag } => (text, tag),
        };
        if prev.as_ref().is_some_and(|p| p.hash == tag.hash) {
            self.store().db().execute("UPDATE calendars SET ctag = ? WHERE id = ?", params![tag.encode(), cal.id])?;
            return Ok(());
        }
        let parsed = feed::parse_feed(&text)?;
        let applied = self.store().replace_objects(&cal.id, &parsed.objects, local_tz())?;
        self.applied(&cal.id, &applied);
        let db = self.store().db();
        db.execute("UPDATE calendars SET ctag = ? WHERE id = ?", params![tag.encode(), cal.id])?;
        db.execute("UPDATE cal_accounts SET refresh_secs = ? WHERE id = ?", params![parsed.refresh_secs, acc.id])?;
        Ok(())
    }

    /// Background sync: CalDAV and Outlook accounts every time, iCal feeds
    /// when their interval is up. Accounts sync side by side (CalDAV ones
    /// still take turns). Microsoft mail accounts get their Outlook calendars here.
    async fn cal_sync_scheduled(&self) {
        match self.ensure_outlook_calendars() {
            Ok(added) if !added.is_empty() => self.cal_changed(),
            Ok(_) => {}
            Err(e) => tracing::debug!("connecting Outlook calendars: {e:#}"),
        }
        let now = now_ms();
        let due: Vec<AccountRow> = self
            .store()
            .cal_account_rows()
            .unwrap_or_default()
            .into_iter()
            .filter(|a| {
                if a.kind != KIND_ICS {
                    return true;
                }
                let every = a.refresh_secs.unwrap_or(feed::DEFAULT_REFRESH_SECS).clamp(feed::MIN_REFRESH_SECS, feed::MAX_REFRESH_SECS) * 1000;
                let last = self.cal().attempts.lock().unwrap().get(&a.id).copied().or(a.last_sync).unwrap_or(0);
                now - last >= every
            })
            .collect();
        if due.is_empty() {
            return;
        }
        futures::future::join_all(due.iter().map(|a| async move {
            if let Err(e) = self.cal_sync_account(&a.id).await {
                tracing::debug!(account = %a.name, "calendar sync: {e:#}");
            }
        }))
        .await;
        // Only when something changed: the UI expands every event again for it.
        if self.cal().changed.swap(false, Ordering::AcqRel) {
            self.emit_event(EngineEvent::CalendarChanged);
        }
    }

    // ------------------------------------------------------------------
    // Calendar setup from another device (cloud::mapping)
    // ------------------------------------------------------------------

    /// A CalDAV account or iCal subscription that arrived from another
    /// device: stored under its id (calendars are found by the next sync).
    /// Returns whether anything changed.
    pub(crate) fn cal_apply_account(&self, a: &SyncedCalAccount) -> Result<bool> {
        if a.kind != KIND_CALDAV && a.kind != KIND_ICS {
            return Ok(false);
        }
        if let Some(m) = &a.mail_account_id {
            if self.store().account(m)?.is_none() {
                return Ok(false); // its mail account hasn't arrived (yet)
            }
        }
        match self.store().cal_account_row(&a.id)? {
            Some(cur) => {
                if (cur.name.as_str(), cur.url.as_str(), cur.username.as_str()) == (a.name.as_str(), a.url.as_str(), a.username.as_str()) {
                    return Ok(false);
                }
                self.store().db().execute(
                    "UPDATE cal_accounts SET name = ?, url = ?, username = ? WHERE id = ?",
                    params![a.name, a.url, a.username, a.id],
                )?;
                if a.kind == KIND_ICS {
                    self.store().db().execute("UPDATE calendars SET href = ?, name = ? WHERE cal_account_id = ?", params![a.url, a.name, a.id])?;
                }
            }
            None => {
                let db = self.store().db();
                db.execute(
                    "INSERT INTO cal_accounts(id, mail_account_id, name, url, username, kind, created_at) VALUES(?,?,?,?,?,?,?)",
                    params![a.id, a.mail_account_id, a.name, a.url, a.username, a.kind, a.created_at],
                )?;
                if a.kind == KIND_ICS {
                    // A subscription is one calendar at its link.
                    let n: i64 = db.query_row("SELECT COUNT(*) FROM calendars", [], |r| r.get(0))?;
                    drop(db);
                    let row = self.store().cal_account_row(&a.id)?.context("calendar account vanished")?;
                    let (color, enabled) = self.calendar_override(&row, &a.url);
                    let color = color.unwrap_or_else(|| PALETTE[n as usize % PALETTE.len()].to_string());
                    self.store().db().execute(
                        "INSERT INTO calendars(id, cal_account_id, href, name, color, read_only, sort_order, enabled) VALUES(?,?,?,?,?,1,0,?)",
                        params![new_id("calendar"), a.id, a.url, a.name, color, enabled.unwrap_or(true)],
                    )?;
                }
            }
        }
        self.cal().attempts.lock().unwrap().remove(&a.id);
        self.cal_changed();
        Ok(true)
    }

    /// Remove a calendar account from this device only.
    pub(crate) fn cal_remove_here(&self, id: &str) -> Result<bool> {
        let Some(row) = self.store().cal_account_row(id)? else { return Ok(false) };
        let calendars: Vec<String> = self.store().calendar_rows(id)?.into_iter().map(|c| c.id).collect();
        self.store().db().execute("DELETE FROM cal_accounts WHERE id = ?", [id])?;
        self.forget_calendars(&calendars);
        if row.mail_account_id.is_none() {
            let _ = self.secrets().delete(&format!("cal:{id}"));
        }
        self.cal_changed();
        Ok(true)
    }

    /// Outlook calendars of these mail accounts were disconnected elsewhere.
    pub(crate) fn cal_remove_outlook(&self, mail_account_ids: &[String]) -> Result<bool> {
        let mut any = false;
        for a in self.store().cal_account_rows()? {
            if a.kind == KIND_GRAPH && a.mail_account_id.as_ref().is_some_and(|m| mail_account_ids.contains(m)) {
                any |= self.cal_remove_here(&a.id)?;
            }
        }
        Ok(any)
    }

    /// Apply calendar colours and visibility chosen on another device to the
    /// calendars shown here. Returns whether anything changed.
    pub(crate) fn cal_apply_overrides(&self, overrides: &HashMap<String, crate::cloud::CalOverride>) -> Result<bool> {
        let accounts: HashMap<String, AccountRow> = self.store().cal_account_rows()?.into_iter().map(|a| (a.id.clone(), a)).collect();
        let mut changed = false;
        for (id, acc_id, href) in self.store().calendar_hrefs()? {
            let Some(acc) = accounts.get(&acc_id) else { continue };
            let key = crate::cloud::override_key(&cal_key(&acc.kind, &acc.id, acc.mail_account_id.as_deref()), &href);
            let Some(o) = overrides.get(&key) else { continue };
            let db = self.store().db();
            if let Some(c) = &o.color {
                changed |= db.execute("UPDATE calendars SET color = ?1 WHERE id = ?2 AND color IS NOT ?1", params![c, id])? > 0;
            }
            if let Some(e) = o.enabled {
                changed |= db.execute("UPDATE calendars SET enabled = ?1 WHERE id = ?2 AND enabled IS NOT ?1", params![e, id])? > 0;
            }
        }
        if changed {
            self.cal_changed();
        }
        Ok(changed)
    }

    /// Calendar setup changed from another device: tell the UI, and sync new accounts now.
    pub(crate) fn cal_applied(&self, new_accounts: Vec<String>) {
        if self.cal().changed.swap(false, Ordering::AcqRel) {
            self.emit_event(EngineEvent::CalendarChanged);
        }
        if new_accounts.is_empty() || !self.gate_open() {
            return;
        }
        let engine = self.clone();
        tokio::spawn(async move {
            for id in new_accounts {
                if let Err(e) = engine.cal_sync(Some(&id)).await {
                    tracing::debug!("calendar sync: {e:#}");
                }
            }
        });
    }

    pub fn cal_remove_account(&self, id: &str) -> Result<()> {
        let row = self.store().cal_account_row(id)?;
        if let Some(mail) = row.as_ref().filter(|r| r.kind == KIND_GRAPH).and_then(|r| r.mail_account_id.clone()) {
            // Don't connect it again by itself.
            let mut off: Vec<String> = self.store().setting(OUTLOOK_OFF).unwrap_or_default();
            if !off.contains(&mail) {
                off.push(mail);
                self.store().set_setting(OUTLOOK_OFF, &off)?;
            }
        }
        let calendars: Vec<String> = self.store().calendar_rows(id)?.into_iter().map(|c| c.id).collect();
        self.store().db().execute("DELETE FROM cal_accounts WHERE id = ?", [id])?;
        self.forget_calendars(&calendars);
        if let Some(r) = row {
            if r.mail_account_id.is_none() {
                let _ = self.secrets().delete(&format!("cal:{id}"));
            }
        }
        self.calendars_changed(Some(id));
        Ok(())
    }

    pub fn cal_set_calendar(&self, id: &str, enabled: Option<bool>, color: Option<String>) -> Result<()> {
        if let Some(e) = enabled {
            self.store().db().execute("UPDATE calendars SET enabled = ? WHERE id = ?", params![e, id])?;
        }
        if let Some(c) = color.filter(|c| c.starts_with('#') && (c.len() == 7 || c.len() == 4)) {
            self.store().db().execute("UPDATE calendars SET color = ? WHERE id = ?", params![c.to_lowercase(), id])?;
        }
        self.calendars_changed(None);
        Ok(())
    }

    /// Sync one account or all of them.
    pub async fn cal_sync(&self, account_id: Option<&str>) -> Result<()> {
        let ids: Vec<String> = match account_id {
            Some(a) => vec![a.to_string()],
            None => {
                self.ensure_outlook_calendars()?;
                self.store().cal_account_rows()?.into_iter().map(|a| a.id).collect()
            }
        };
        let mut first_err = None;
        for id in ids {
            if let Err(e) = self.cal_sync_account(&id).await {
                first_err.get_or_insert(e);
            }
        }
        self.emit_event(EngineEvent::CalendarChanged);
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn cal_sync_account(&self, id: &str) -> Result<()> {
        let acc = self.store().cal_account_row(id)?.context("calendar account not found")?;
        self.cal().attempts.lock().unwrap().insert(id.to_string(), now_ms());
        let result = if acc.kind == KIND_ICS {
            // Read-only, so it needn't wait for (or hold up) CalDAV writes.
            self.ics_sync(&acc).await
        } else if acc.kind == KIND_GRAPH {
            let lock = self.graph_lock(&acc.id);
            let _turn = lock.lock().await;
            self.graph_sync(&acc).await
        } else {
            let _guard = self.cal().syncing.lock().await;
            self.cal_sync_inner(&acc).await
        };
        let (status, err) = match &result {
            Ok(()) => ("ok", None),
            // Only signing in again helps (Settings shows the button for it).
            Err(e) if e.chain().any(|c| c.downcast_ref::<crate::oauth::SignInRequired>().is_some()) => ("signin", Some(format!("{e:#}"))),
            Err(e) => ("error", Some(format!("{e:#}"))),
        };
        if status != acc.status || err != acc.last_error {
            self.cal_changed();
        }
        self.store().db().execute(
            "UPDATE cal_accounts SET status = ?, last_error = ?, last_sync = CASE WHEN ? = 'ok' THEN ? ELSE last_sync END WHERE id = ?",
            params![status, err, status, now_ms(), id],
        )?;
        result
    }

    async fn cal_sync_inner(&self, acc: &AccountRow) -> Result<()> {
        let client = self.dav_client(acc).await?;
        let remote: Vec<_> = client.calendars(&acc.url).await?.into_iter().filter(|c| c.supports_events).collect();
        let local = self.store().calendar_rows(&acc.id)?;
        // Calendars removed on the server.
        for l in &local {
            if !remote.iter().any(|r| r.href == l.href) {
                self.store().db().execute("DELETE FROM calendars WHERE id = ?", [&l.id])?;
                self.forget_calendars(std::slice::from_ref(&l.id));
                self.cal_changed();
            }
        }
        let tz = local_tz();
        let mut first_err: Option<anyhow::Error> = None;
        for (i, r) in remote.iter().enumerate() {
            // One broken calendar (e.g. a shared one that now returns 403) mustn't stop the others.
            if let Err(e) = self.cal_sync_calendar(&client, acc, &local, i, r, tz).await {
                tracing::warn!(calendar = %r.name, "calendar sync failed: {e:#}");
                first_err.get_or_insert(e.context(format!("calendar \"{}\"", r.name)));
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn cal_sync_calendar(
        &self,
        client: &CalDavClient,
        acc: &AccountRow,
        local: &[CalendarRow],
        i: usize,
        r: &crate::caldav::client::DavCalendar,
        tz: Tz,
    ) -> Result<()> {
        {
            let existing = local.iter().find(|l| l.href == r.href);
            let cal_id = match existing {
                Some(l) => {
                    let updated = self.store().db().execute(
                        "UPDATE calendars SET name = ?1, read_only = ?2, sort_order = ?3
                         WHERE id = ?4 AND (name IS NOT ?1 OR read_only IS NOT ?2 OR sort_order IS NOT ?3)",
                        params![r.name, r.read_only, i as i64, l.id],
                    )?;
                    if updated > 0 {
                        self.cal_changed();
                    }
                    l.id.clone()
                }
                None => {
                    let id = new_id("calendar");
                    let (chosen, enabled) = self.calendar_override(acc, &r.href);
                    let color = chosen.or_else(|| r.color.as_deref().map(str::to_lowercase)).unwrap_or_else(|| PALETTE[i % PALETTE.len()].to_string());
                    self.store().db().execute(
                        "INSERT INTO calendars(id, cal_account_id, href, name, color, read_only, sort_order, enabled) VALUES(?,?,?,?,?,?,?,?)",
                        params![id, acc.id, r.href, r.name, color, r.read_only, i as i64, enabled.unwrap_or(true)],
                    )?;
                    self.cal_changed();
                    id
                }
            };
            let tag = r.sync_token.clone().or_else(|| r.ctag.clone());
            let stored = existing.and_then(|l| l.ctag.clone());
            if tag.is_some() && stored == tag {
                return Ok(());
            }
            let local_etags = self.store().object_etags(&cal_id)?;
            // Just what changed since the last sync, when the server keeps track (RFC 6578).
            let delta = match (&r.sync_token, &stored) {
                (Some(_), Some(token)) => match client.sync_changes(&r.href, token).await {
                    Ok(d) => d,
                    Err(e) if matches!(e.downcast_ref::<DavError>(), Some(DavError::Auth)) => return Err(e),
                    Err(e) => {
                        tracing::debug!(calendar = %r.name, "sync-collection failed, listing the calendar: {e:#}");
                        None
                    }
                },
                _ => None,
            };
            let (changed, removed, tag): (Vec<String>, Vec<String>, Option<String>) = match delta {
                Some(d) => (
                    d.changed.into_iter().filter(|(h, e)| local_etags.get(h) != Some(e)).map(|(h, _)| h).collect(),
                    d.deleted.into_iter().filter(|h| local_etags.contains_key(h)).collect(),
                    d.sync_token.or(tag),
                ),
                // No token, or it expired: compare every member.
                None => {
                    let remote_etags = client.list_etags(&r.href).await?;
                    let remote_set: HashSet<&String> = remote_etags.iter().map(|(h, _)| h).collect();
                    let removed = local_etags.keys().filter(|h| !remote_set.contains(h)).cloned().collect();
                    let changed = remote_etags.iter().filter(|(h, e)| local_etags.get(h) != Some(e)).map(|(h, _)| h.clone()).collect();
                    (changed, removed, tag)
                }
            };
            let fetched: Vec<FeedObject> = if changed.is_empty() {
                Vec::new()
            } else {
                client.multiget(&r.href, &changed).await?.into_iter().map(|o| FeedObject { href: o.href, etag: o.etag, ics: o.ics }).collect()
            };
            let applied = self.store().apply_changes(&cal_id, &fetched, &removed, tz)?;
            self.applied(&cal_id, &applied);
            self.store().db().execute("UPDATE calendars SET ctag = ? WHERE id = ?", params![tag, cal_id])?;
        }
        Ok(())
    }

    fn parsed_object(&self, o: &ObjectRow) -> Option<Arc<Component>> {
        let key = (o.calendar_id.clone(), o.href.clone());
        if let Some((etag, c)) = self.cal().parsed.lock().unwrap().get(&key) {
            if *etag == o.etag {
                return Some(c.clone());
            }
        }
        // New or changed: only now read its text.
        let (etag, ics) = self.store().object(&o.calendar_id, &o.href).ok()??;
        let c = Arc::new(Component::parse(&ics).ok()?);
        self.cal().parsed.lock().unwrap().insert(key, (etag, c.clone()));
        Some(c)
    }

    fn expand(&self, rows: Vec<ObjectRow>, from: i64, to: i64) -> Vec<CalEvent> {
        let tz = local_tz();
        let mut out = Vec::new();
        for o in rows {
            let Some(comp) = self.parsed_object(&o) else { continue };
            // An Outlook occurrence is stored on its own; it still belongs to a series.
            let series = if o.outlook { graph::series_of(&comp) } else { None };
            match event::occurrences(&comp, from, to, tz) {
                Ok(list) => {
                    for mut occ in list {
                        if let Some((_, original, rule)) = &series {
                            occ.recurring = true;
                            occ.recurrence_id = Some(original.unwrap_or(occ.start_ms));
                            occ.rrule = rule.clone();
                        }
                        out.push(CalEvent {
                            key: format!("{}|{}|{}", o.calendar_id, o.href, occ.recurrence_id.unwrap_or(occ.start_ms)),
                            calendar_id: o.calendar_id.clone(),
                            href: o.href.clone(),
                            color: o.color.clone(),
                            read_only: o.read_only,
                            occurrence: occ,
                        });
                    }
                }
                Err(e) => tracing::debug!(href = %o.href, "skipping unreadable event: {e:#}"),
            }
        }
        out.sort_by_key(|e| (e.occurrence.start_ms, e.occurrence.end_ms));
        out
    }

    pub fn cal_events(&self, from: i64, to: i64) -> Result<Vec<CalEvent>> {
        if to <= from || to - from > 800 * 86_400_000 {
            bail!("invalid date range");
        }
        Ok(self.expand(self.store().objects_in_range(from, to)?, from, to))
    }

    pub fn cal_search(&self, query: &str) -> Result<Vec<CalEvent>> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Ok(vec![]);
        }
        let now = now_ms();
        let year = 366 * 86_400_000;
        let mut found: Vec<CalEvent> = self
            .expand(self.store().objects_matching(query.trim())?, now - year, now + year)
            .into_iter()
            .filter(|e| {
                let o = &e.occurrence;
                let has = |s: &str| s.to_lowercase().contains(&q);
                has(&o.summary)
                    || has(&o.location)
                    || has(&o.description)
                    || o.organizer.as_deref().is_some_and(has)
                    || o.attendees.iter().any(|a| has(&a.name) || has(&a.email))
            })
            .collect();
        // Upcoming first, then the past (most recent first).
        found.sort_by_key(|e| if e.occurrence.end_ms >= now { (0, e.occurrence.start_ms) } else { (1, -e.occurrence.start_ms) });
        found.truncate(200);
        Ok(found)
    }

    fn writable_calendar(&self, calendar_id: &str) -> Result<(CalendarRow, AccountRow)> {
        let cal = self.store().calendar_row(calendar_id)?.context("calendar not found")?;
        if cal.read_only {
            bail!("This calendar is read-only");
        }
        let acc = self.store().cal_account_row(&cal.account_id)?.context("calendar account not found")?;
        Ok((cal, acc))
    }

    /// The etag a write of our copy (`stored_ics`) must match. When we never
    /// learned it, the server's current one only fits if the server still
    /// holds that copy; otherwise its version is stored and the write refused
    /// as a conflict (rather than overwriting changes we haven't seen).
    async fn known_etag(&self, client: &CalDavClient, cal: &CalendarRow, href: &str, stored_etag: &str, stored_ics: &str) -> Result<Option<String>> {
        if !stored_etag.is_empty() {
            return Ok(Some(stored_etag.to_string()));
        }
        let Some(o) = client.multiget(&cal.href, &[href.to_string()]).await?.into_iter().next() else {
            return Ok(None); // gone from the server
        };
        if o.ics != stored_ics {
            self.store().put_object(&cal.id, href, &o.etag, &o.ics, local_tz())?;
            return Err(DavError::Conflict.into());
        }
        if o.etag.is_empty() {
            return Ok(None);
        }
        self.store().put_object(&cal.id, href, &o.etag, &o.ics, local_tz())?;
        Ok(Some(o.etag))
    }

    /// PUT, then store what the server holds. A reply without an ETag means
    /// the server changed the object as it stored it (RFC 4791 §5.3.4), so its
    /// copy is fetched; if that fails ours is kept without an etag (the next
    /// sync fetches the server's, and writes before that check it first).
    async fn put_and_store(&self, client: &CalDavClient, cal: &CalendarRow, href: &str, ics: &str, etag: Option<&str>, create: bool) -> Result<()> {
        let tz = local_tz();
        if let Some(e) = client.put(href, ics, etag, create).await? {
            return self.store().put_object(&cal.id, href, &e, ics, tz);
        }
        match client.multiget(&cal.href, &[href.to_string()]).await {
            Ok(found) => {
                if let Some(o) = found.into_iter().next() {
                    return self.store().put_object(&cal.id, href, &o.etag, &o.ics, tz);
                }
            }
            Err(e) => tracing::debug!(%href, "fetching an event the server changed: {e:#}"),
        }
        self.store().put_object(&cal.id, href, "", ics, tz)
    }

    pub async fn cal_create(&self, calendar_id: &str, input: EventInput) -> Result<()> {
        self.create_event(calendar_id, &input).await.map(|_| ())
    }

    /// Create an event; returns its href.
    async fn create_event(&self, calendar_id: &str, input: &EventInput) -> Result<String> {
        let (cal, acc) = self.writable_calendar(calendar_id)?;
        let href = if acc.kind == KIND_GRAPH {
            let lock = self.graph_lock(&acc.id);
            let _turn = lock.lock().await;
            self.graph_create(&cal, &acc, input).await?
        } else {
            let client = self.dav_client(&acc).await?;
            let ics = event::new_object(input).to_ics();
            let href = CalDavClient::new_href(&cal.href);
            // A sync listing taken before this PUT must not delete the new event locally.
            let _sync = self.cal().syncing.lock().await;
            self.put_and_store(&client, &cal, &href, &ics, None, true).await?;
            href
        };
        self.emit_event(EngineEvent::CalendarChanged);
        self.cal().wake.notify_one();
        Ok(href)
    }

    /// Edit an event. `occurrence_start` is where the edited occurrence was
    /// shown: editing "all events" from it moves the whole series by the same amount.
    pub async fn cal_update(
        &self,
        calendar_id: &str,
        href: &str,
        input: EventInput,
        scope: EditScope,
        new_calendar: Option<String>,
        occurrence_start: Option<i64>,
    ) -> Result<()> {
        let (cal, acc) = self.writable_calendar(calendar_id)?;
        let target = new_calendar.clone().filter(|t| *t != cal.id && matches!(scope, EditScope::All));
        let target_is_graph = match &target {
            Some(t) => self.writable_calendar(t)?.1.kind == KIND_GRAPH,
            None => false,
        };
        if let Some(t) = target.filter(|_| acc.kind == KIND_GRAPH || target_is_graph) {
            // To or from Outlook: a copy in the other calendar, then the original
            // goes (only if it's unchanged; otherwise the copy is taken back).
            self.movable(&cal, href)?;
            let copy = self.create_event(&t, &input).await?;
            if let Err(e) = self.cal_delete(&cal.id, href, EditScope::All).await {
                if let Err(undo) = self.cal_delete(&t, &copy, EditScope::All).await {
                    tracing::warn!("taking back a copied event: {undo:#}");
                }
                return Err(e.context("The event couldn't be moved"));
            }
            return Ok(());
        }
        let result = if acc.kind == KIND_GRAPH {
            let lock = self.graph_lock(&acc.id);
            let _turn = lock.lock().await;
            self.graph_update(&cal, &acc, href, input, scope, occurrence_start).await
        } else {
            let _sync = self.cal().syncing.lock().await;
            self.cal_update_locked(&cal, &acc, href, input, scope, new_calendar, occurrence_start).await
        };
        if let Err(e) = result {
            return Err(self.conflict(e, &acc.id).await);
        }
        self.emit_event(EngineEvent::CalendarChanged);
        self.cal().wake.notify_one();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn cal_update_locked(
        &self,
        cal: &CalendarRow,
        acc: &AccountRow,
        href: &str,
        mut input: EventInput,
        scope: EditScope,
        new_calendar: Option<String>,
        occurrence_start: Option<i64>,
    ) -> Result<()> {
        let (stored_etag, ics) = self.store().object(&cal.id, href)?.context("event not found; it may have been deleted")?;
        let mut comp = Component::parse(&ics)?;
        let tz = local_tz();
        if let (EditScope::All, Some(shown)) = (scope, occurrence_start) {
            if let Some(master) = event::master_start(&comp, tz) {
                shift_series(&mut input, master, shown, tz);
            }
        }
        event::update_object(&mut comp, &input, scope)?;
        let new_ics = comp.to_ics();
        let client = self.dav_client(acc).await?;
        let etag = self.known_etag(&client, cal, href, &stored_etag, &ics).await?;
        if let Some(target) = new_calendar.filter(|t| *t != cal.id && matches!(scope, EditScope::All)) {
            // Moving calendars: only if the source is unchanged on the server, then
            // create the copy, then delete the original (undoing the copy if that fails).
            if client.etag_of(href).await? != etag {
                return Err(DavError::Conflict.into());
            }
            let (tcal, tacc) = self.writable_calendar(&target)?;
            let tclient = self.dav_client(&tacc).await?;
            let thref = CalDavClient::new_href(&tcal.href);
            self.put_and_store(&tclient, &tcal, &thref, &new_ics, None, true).await?;
            if let Err(e) = client.delete(href, etag.as_deref()).await {
                let _ = tclient.delete(&thref, None).await;
                self.delete_stored(&tcal.id, &thref)?;
                return Err(e);
            }
            self.delete_stored(&cal.id, href)?;
        } else {
            self.put_and_store(&client, cal, href, &new_ics, etag.as_deref(), false).await?;
        }
        Ok(())
    }

    pub async fn cal_delete(&self, calendar_id: &str, href: &str, scope: EditScope) -> Result<()> {
        let (cal, acc) = self.writable_calendar(calendar_id)?;
        if acc.kind == KIND_GRAPH {
            let result = {
                let lock = self.graph_lock(&acc.id);
                let _turn = lock.lock().await;
                self.graph_delete(&cal, &acc, href, scope).await
            };
            if let Err(e) = result {
                return Err(self.conflict(e, &acc.id).await);
            }
            self.emit_event(EngineEvent::CalendarChanged);
            return Ok(());
        }
        let result: Result<()> = async {
            let _sync = self.cal().syncing.lock().await;
            let (stored_etag, ics) = self.store().object(&cal.id, href)?.context("event not found; it may have been deleted")?;
            let client = self.dav_client(&acc).await?;
            let etag = self.known_etag(&client, &cal, href, &stored_etag, &ics).await?;
            match scope {
                EditScope::All => {
                    client.delete(href, etag.as_deref()).await?;
                    self.delete_stored(&cal.id, href)?;
                }
                EditScope::This { recurrence_id } => {
                    let mut comp = Component::parse(&ics)?;
                    event::delete_occurrence_in(&mut comp, recurrence_id, local_tz())?;
                    if comp.children_named("VEVENT").next().is_none() {
                        // Its only instance (an invitation to one occurrence): an
                        // empty calendar object isn't valid, so the resource goes.
                        client.delete(href, etag.as_deref()).await?;
                        self.delete_stored(&cal.id, href)?;
                    } else {
                        self.put_and_store(&client, &cal, href, &comp.to_ics(), etag.as_deref(), false).await?;
                    }
                }
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            return Err(self.conflict(e, &acc.id).await);
        }
        self.emit_event(EngineEvent::CalendarChanged);
        Ok(())
    }

    /// A rejected write: on a conflict, refresh so the user sees the server's version.
    async fn conflict(&self, e: anyhow::Error, account_id: &str) -> anyhow::Error {
        if matches!(e.downcast_ref::<DavError>(), Some(DavError::Conflict)) {
            let _ = self.cal_sync_account(account_id).await;
            self.emit_event(EngineEvent::CalendarChanged);
            return anyhow!("This event was changed on another device. Remail has loaded the latest version; please make your change again.");
        }
        e
    }

    // ------------------------------------------------------------------
    // Outlook calendars (Microsoft Graph)
    // ------------------------------------------------------------------

    fn graph_lock(&self, account_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.cal().graph_locks.lock().unwrap().entry(account_id.to_string()).or_default().clone()
    }

    /// The Outlook calendar account of a Microsoft mail account, created if
    /// missing (unless it was disconnected, checked under the same lock).
    fn outlook_account(&self, mail: &crate::model::Account) -> Result<Option<String>> {
        let db = self.store().db();
        let existing: Option<String> = db
            .query_row("SELECT id FROM cal_accounts WHERE kind = ? AND mail_account_id = ?", params![KIND_GRAPH, mail.id], |r| r.get(0))
            .optional()?;
        if existing.is_some() {
            return Ok(existing);
        }
        let off: Option<String> = db.query_row("SELECT value FROM settings WHERE key = ?", [OUTLOOK_OFF], |r| r.get(0)).optional()?;
        if off.and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok()).unwrap_or_default().contains(&mail.id) {
            return Ok(None);
        }
        let id = new_id("cal");
        db.execute(
            "INSERT INTO cal_accounts(id, mail_account_id, name, url, username, kind, created_at) VALUES(?,?,?,?,?,?,?)",
            params![id, mail.id, mail.email, crate::engine::graph_base(), mail.email, KIND_GRAPH, now_ms()],
        )?;
        Ok(Some(id))
    }

    /// Every Microsoft mail account shows its Outlook calendars, unless they
    /// were disconnected. Returns the calendar accounts it added.
    fn ensure_outlook_calendars(&self) -> Result<Vec<String>> {
        let have: HashSet<String> = self.store().cal_account_rows()?.into_iter().filter(|a| a.kind == KIND_GRAPH).filter_map(|a| a.mail_account_id).collect();
        let mut added = Vec::new();
        for m in self.store().accounts()? {
            if m.auth == crate::oauth::MICROSOFT && !have.contains(&m.id) {
                added.extend(self.outlook_account(&m)?);
            }
        }
        Ok(added)
    }

    /// Connect a Microsoft account's Outlook calendars and sync them now.
    /// `asked`: the user asked for it (connects again after a disconnect).
    pub(crate) async fn connect_outlook_calendar(&self, mail_account_id: &str, asked: bool) -> Result<CalAccountView> {
        let mail = self.store().account(mail_account_id)?.context("account not found")?;
        if mail.auth != crate::oauth::MICROSOFT {
            bail!("Outlook calendars need Microsoft sign-in for this account");
        }
        if asked {
            let mut off: Vec<String> = self.store().setting(OUTLOOK_OFF).unwrap_or_default();
            if off.contains(&mail.id) {
                off.retain(|m| *m != mail.id);
                self.store().set_setting(OUTLOOK_OFF, &off)?;
                self.mark_dirty(sections::CALENDARS);
            }
        }
        let id = self.outlook_account(&mail)?.context("its calendars were disconnected")?;
        let synced = self.cal_sync_account(&id).await;
        self.emit_event(EngineEvent::CalendarChanged);
        synced?;
        self.cal_accounts()?.into_iter().find(|a| a.id == id).context("calendar account vanished")
    }

    async fn graph_client(&self, acc: &AccountRow) -> Result<GraphClient> {
        let mail = acc.mail_account_id.as_deref().context("this Outlook calendar has no mail account")?;
        let token = self.calendar_token(mail).await?;
        Ok(GraphClient::new(self.http().clone(), &crate::engine::graph_base(), token))
    }

    /// After a refusal (HTTP 401) the next request gets a new token.
    fn graph_refused(&self, acc: &AccountRow, client: &GraphClient, e: &anyhow::Error) {
        if e.chain().any(|c| c.downcast_ref::<GraphError>().is_some_and(|g| g.status == 401)) {
            if let Some(mail) = &acc.mail_account_id {
                self.forget_calendar_token(mail, client.token());
            }
        }
    }

    /// The account's calendar list, then each calendar's changes. Graph
    /// takes four requests at a time per mailbox: two calendars at once, each
    /// with up to two series lookups.
    async fn graph_sync(&self, acc: &AccountRow) -> Result<()> {
        let client = self.graph_client(acc).await?;
        let result = self.graph_sync_with(&client, acc).await;
        if let Err(e) = &result {
            self.graph_refused(acc, &client, e);
        }
        result
    }

    async fn graph_sync_with(&self, client: &GraphClient, acc: &AccountRow) -> Result<()> {
        let mut remote = client.calendars(&acc.username).await?;
        // The main calendar first, then Outlook's order.
        remote.sort_by_key(|c| !c.is_default);
        let local = self.store().calendar_rows(&acc.id)?;
        for l in &local {
            if !remote.iter().any(|r| r.id == l.href) {
                self.store().db().execute("DELETE FROM calendars WHERE id = ?", [&l.id])?;
                self.forget_calendars(std::slice::from_ref(&l.id));
                self.cal_changed();
            }
        }
        let shown = self.store().calendar_views()?.len();
        let mut rows = Vec::new();
        for (i, r) in remote.iter().enumerate() {
            let row = match local.iter().find(|l| l.href == r.id) {
                Some(l) => {
                    let updated = self.store().db().execute(
                        "UPDATE calendars SET name = ?1, read_only = ?2, sort_order = ?3
                         WHERE id = ?4 AND (name IS NOT ?1 OR read_only IS NOT ?2 OR sort_order IS NOT ?3)",
                        params![r.name, !r.writable, i as i64, l.id],
                    )?;
                    if updated > 0 {
                        self.cal_changed();
                    }
                    CalendarRow { read_only: !r.writable, ..l.clone() }
                }
                None => {
                    let id = new_id("calendar");
                    let (chosen, enabled) = self.calendar_override(acc, &r.id);
                    let color = chosen.or_else(|| r.color.clone()).unwrap_or_else(|| PALETTE[(shown + i) % PALETTE.len()].to_string());
                    self.store().db().execute(
                        "INSERT INTO calendars(id, cal_account_id, href, name, color, read_only, sort_order, enabled) VALUES(?,?,?,?,?,?,?,?)",
                        params![id, acc.id, r.id, r.name, color, !r.writable, i as i64, enabled.unwrap_or(true)],
                    )?;
                    self.cal_changed();
                    CalendarRow { id, account_id: acc.id.clone(), href: r.id.clone(), ctag: None, read_only: !r.writable }
                }
            };
            rows.push((row, r.name.clone()));
        }
        let tz = local_tz();
        let results: Vec<Result<()>> = futures::stream::iter(rows)
            .map(|(row, name)| async move {
                self.graph_sync_calendar(client, &row, tz).await.map_err(|e| e.context(format!("calendar \"{name}\"")))
            })
            .buffer_unordered(2)
            .collect()
            .await;
        // One broken calendar (say, a shared one we may no longer read) doesn't stop the others.
        match results.into_iter().find_map(Result::err) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// One change-tracking round for a calendar: everything in the window the
    /// first time (or when the window has moved on a month, or the saved round
    /// is no longer accepted), then only changes. Calendars that offer no
    /// change tracking are listed in full instead.
    async fn graph_sync_calendar(&self, client: &GraphClient, cal: &CalendarRow, tz: Tz) -> Result<()> {
        #[derive(Serialize, Deserialize)]
        struct Round {
            link: String,
            from: i64,
            to: i64,
        }
        let (from, to) = graph::window(now_ms());
        let saved: Option<Round> = cal.ctag.as_deref().and_then(|c| serde_json::from_str(c).ok());
        let (mut url, mut full, mut window) = match saved {
            Some(r) if r.from >= from - 31 * DAY_MS => (r.link, false, (r.from, r.to)),
            _ => (client.delta_start(&cal.href, from, to)?, true, (from, to)),
        };
        let mut changes: Vec<(String, Option<serde_json::Value>)> = Vec::new();
        let mut pages = 0;
        let link = loop {
            pages += 1;
            if pages > 2000 {
                bail!("Microsoft kept sending more changes");
            }
            match client.delta_page(&url).await {
                Ok(page) => {
                    changes.extend(page.changes);
                    match (page.next, page.delta) {
                        (Some(next), _) => url = next,
                        (None, Some(delta)) => break Some(delta),
                        (None, None) => bail!("Microsoft didn't finish the list of events"),
                    }
                }
                // The saved round expired or isn't accepted any more: start the window over.
                Err(e) if !full && e.downcast_ref::<GraphError>().is_some_and(|g| g.sync_reset() || g.status == 400) => {
                    full = true;
                    window = (from, to);
                    url = client.delta_start(&cal.href, from, to)?;
                    changes.clear();
                }
                // No change tracking for this calendar: list it.
                Err(e) if full && pages == 1 && e.downcast_ref::<GraphError>().is_some_and(|g| matches!(g.status, 400 | 405 | 501)) => {
                    tracing::debug!("listing a calendar without change tracking: {e:#}");
                    changes = client.list_view(&cal.href, from, to).await?.into_iter().filter_map(|v| Some((v["id"].as_str()?.to_string(), Some(v)))).collect();
                    break None;
                }
                Err(e) => return Err(e),
            }
        };
        // The last word on each event.
        let mut latest: HashMap<String, Option<serde_json::Value>> = HashMap::new();
        let mut order = Vec::new();
        for (id, ev) in changes {
            if !latest.contains_key(&id) {
                order.push(id.clone());
            }
            latest.insert(id, ev);
        }
        // Occurrences carry their series' rule (for the editor); one lookup per series.
        let series: HashSet<String> =
            latest.values().flatten().filter_map(|e| e["seriesMasterId"].as_str().filter(|s| !s.is_empty()).map(String::from)).collect();
        let mut rules: HashMap<String, Option<String>> = futures::stream::iter(series)
            .map(|id| async move {
                let rule = match client.series(&cal.href, &id).await {
                    Ok(m) => Some(graph::rrule_from(&m["recurrence"])),
                    Err(e) => {
                        tracing::debug!("series {id}: {e:#}");
                        None
                    }
                };
                (id, rule)
            })
            .buffer_unordered(2)
            .filter_map(|(id, rule)| async move { rule.map(|r| (id, r)) })
            .collect()
            .await;
        // A lookup that failed keeps the rule stored before.
        let missing: Vec<String> = latest
            .values()
            .flatten()
            .filter_map(|e| e["seriesMasterId"].as_str())
            .filter(|s| !s.is_empty() && !rules.contains_key(*s))
            .map(String::from)
            .collect();
        if !missing.is_empty() {
            for ics in self.store().object_texts(&cal.id)? {
                if let Some((series, _, rule)) = Component::parse(&ics).ok().as_ref().and_then(graph::series_of) {
                    if missing.contains(&series) && rule.is_some() {
                        rules.entry(series).or_insert(rule);
                    }
                }
            }
        }
        let rule_of = |e: &serde_json::Value| e["seriesMasterId"].as_str().and_then(|s| rules.get(s).cloned().flatten());
        let mut objects: Vec<FeedObject> = Vec::new();
        let mut removed: Vec<String> = Vec::new();
        for id in &order {
            match latest[id].as_ref() {
                Some(e) => match graph::to_object(e, rule_of(e).as_deref()) {
                    Some(o) => objects.push(o),
                    // Became something we don't show (a series' master): drop the old copy.
                    None => removed.push(id.clone()),
                },
                None => removed.push(id.clone()),
            }
        }
        let applied = if full {
            self.store().replace_objects(&cal.id, &objects, tz)?
        } else {
            self.store().apply_changes(&cal.id, &objects, &removed, tz)?
        };
        self.applied(&cal.id, &applied);
        let round = link.map(|link| serde_json::to_string(&Round { link, from: window.0, to: window.1 })).transpose()?;
        self.store().db().execute("UPDATE calendars SET ctag = ? WHERE id = ?", params![round, cal.id])?;
        Ok(())
    }

    /// After a write Microsoft took: fetch the calendar's changes. If that
    /// fails, the write still happened (so saying it failed would invite a
    /// duplicate): try again in the background.
    async fn graph_after_write(&self, client: &GraphClient, cal: &CalendarRow, acc: &AccountRow, then: AfterWrite) {
        let done = match then {
            AfterWrite::Nothing => Ok(()),
            AfterWrite::Store(o) => self.store().put_object(&cal.id, &o.href, &o.etag, &o.ics, local_tz()),
            AfterWrite::Refresh => match self.store().calendar_row(&cal.id) {
                Ok(Some(row)) => self.graph_sync_calendar(client, &row, local_tz()).await,
                Ok(None) => Ok(()),
                Err(e) => Err(e),
            },
        };
        if let Err(e) = done {
            tracing::warn!("updating an Outlook calendar after a change: {e:#}");
            let engine = self.clone();
            let id = acc.id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(15)).await;
                let _ = engine.cal_sync_account(&id).await;
                engine.emit_event(EngineEvent::CalendarChanged);
            });
        }
    }

    /// Create an event; returns its id (the object's href).
    async fn graph_create(&self, cal: &CalendarRow, acc: &AccountRow, input: &EventInput) -> Result<String> {
        let client = self.graph_client(acc).await?;
        let mut body = graph::event_body(input);
        let rule = input.rrule.as_deref().map(str::trim).filter(|r| !r.is_empty());
        if let Some(r) = rule {
            body["recurrence"] = graph::recurrence_for(r, graph::start_date(input), &graph::graph_zone(&input.tz, input.start_ms).0)?;
        }
        let created = match client.create(&cal.href, &body).await {
            Ok(c) => c,
            Err(e) => {
                self.graph_refused(acc, &client, &e);
                return Err(e);
            }
        };
        let id = created["id"].as_str().unwrap_or_default().to_string();
        // A series: Microsoft lays out its occurrences, fetched with the changes.
        let then = match graph::to_object(&created, None).filter(|_| rule.is_none()) {
            Some(o) => AfterWrite::Store(o),
            None => AfterWrite::Refresh,
        };
        self.graph_after_write(&client, cal, acc, then).await;
        Ok(id)
    }

    /// Edit one event, one occurrence, or (from an occurrence) the whole series.
    /// Only fields the editor changed are sent.
    async fn graph_update(&self, cal: &CalendarRow, acc: &AccountRow, href: &str, input: EventInput, scope: EditScope, occurrence_start: Option<i64>) -> Result<()> {
        let (stored_etag, ics) = self.store().object(&cal.id, href)?.context("event not found; it may have been deleted")?;
        let comp = Component::parse(&ics)?;
        let series = graph::series_of(&comp);
        let (now, century) = (now_ms(), 100 * 366 * DAY_MS);
        let old = event::occurrences(&comp, now - century, now + century, local_tz())?.into_iter().next().context("the event has no time")?;
        let user_tz = Tz::from_str(&input.tz).unwrap_or_else(|_| local_tz());
        let zone = graph::graph_zone(&input.tz, input.start_ms).0;
        let rule = input.rrule.clone().map(|r| r.trim().to_string()).filter(|r| !r.is_empty());
        let client = self.graph_client(acc).await?;
        let written: Result<AfterWrite> = async {
            match (series, scope) {
                (Some((master, _, shown_rule)), EditScope::All) => {
                    let m = client.series(&cal.href, &master).await?;
                    let first = graph::start_ms(&m, &input.tz).context("the series has no start")?;
                    // The series moves by however much this occurrence moved (in wall-clock time).
                    let mut whole = input.clone();
                    shift_series(&mut whole, first, occurrence_start.unwrap_or(old.start_ms), user_tz);
                    let mut body = graph::patch_body(&whole, &input, &old);
                    let was = EventInput { start_ms: first, ..whole.clone() };
                    let moved_day = graph::start_date(&whole) != graph::start_date(&was);
                    if rule != shown_rule {
                        // The user picked another repetition. "Doesn't repeat" ends the
                        // series only when we knew its rule (never because a lookup failed).
                        match (&rule, &shown_rule) {
                            (Some(r), _) => body["recurrence"] = graph::recurrence_for(r, graph::start_date(&whole), &zone)?,
                            (None, Some(_)) => body["recurrence"] = serde_json::Value::Null,
                            (None, None) => {}
                        }
                    } else if moved_day {
                        // Same rule, new first day: the series' own pattern, starting then.
                        if let Some(r) = graph::moved_recurrence(&m["recurrence"], graph::start_date(&whole)) {
                            body["recurrence"] = r;
                        }
                    }
                    if body.as_object().is_none_or(|b| b.is_empty()) {
                        return Ok(AfterWrite::Nothing);
                    }
                    client.update(&cal.href, &master, &body, m["@odata.etag"].as_str()).await?;
                    Ok(AfterWrite::Refresh)
                }
                (series, _) => {
                    // A single event, or just this occurrence.
                    let mut body = graph::patch_body(&input, &input, &old);
                    let becomes_series = series.is_none() && rule.is_some();
                    if let (true, Some(r)) = (becomes_series, &rule) {
                        body["recurrence"] = graph::recurrence_for(r, graph::start_date(&input), &zone)?;
                    }
                    if body.as_object().is_none_or(|b| b.is_empty()) {
                        return Ok(AfterWrite::Nothing);
                    }
                    let updated = client.update(&cal.href, href, &body, graph::graph_etag(&stored_etag)).await?;
                    let known_rule = series.and_then(|s| s.2);
                    Ok(match updated.as_ref().and_then(|u| graph::to_object(u, known_rule.as_deref())).filter(|_| !becomes_series) {
                        Some(o) => AfterWrite::Store(o),
                        None => AfterWrite::Refresh,
                    })
                }
            }
        }
        .await;
        match written {
            Ok(then) => {
                self.graph_after_write(&client, cal, acc, then).await;
                Ok(())
            }
            Err(e) => {
                self.graph_refused(acc, &client, &e);
                Err(e)
            }
        }
    }

    async fn graph_delete(&self, cal: &CalendarRow, acc: &AccountRow, href: &str, scope: EditScope) -> Result<()> {
        let (stored_etag, ics) = self.store().object(&cal.id, href)?.context("event not found; it may have been deleted")?;
        let series = graph::series_of(&Component::parse(&ics)?);
        let client = self.graph_client(acc).await?;
        let deleted = match &series {
            Some((master, _, _)) if matches!(scope, EditScope::All) => client.delete(&cal.href, master, None).await,
            _ => client.delete(&cal.href, href, graph::graph_etag(&stored_etag)).await,
        };
        if let Err(e) = deleted {
            self.graph_refused(acc, &client, &e);
            return Err(e);
        }
        if series.is_some() && matches!(scope, EditScope::All) {
            self.graph_after_write(&client, cal, acc, AfterWrite::Refresh).await;
        } else if let Err(e) = self.delete_stored(&cal.id, href) {
            tracing::warn!("removing a deleted Outlook event locally: {e:#}");
        }
        Ok(())
    }

    /// Whether an event can be moved by copying it and deleting the original:
    /// not a repeating one (its history and exceptions would be lost) or a
    /// meeting (deleting it would cancel it for everyone invited).
    fn movable(&self, cal: &CalendarRow, href: &str) -> Result<()> {
        let (_, ics) = self.store().object(&cal.id, href)?.context("event not found; it may have been deleted")?;
        let comp = Component::parse(&ics)?;
        let events: Vec<&Component> = comp.children.iter().filter(|c| c.name == "VEVENT").collect();
        let repeating = graph::series_of(&comp).is_some() || events.len() > 1 || events.iter().any(|e| e.prop("RRULE").is_some() || e.prop("RDATE").is_some());
        if repeating {
            bail!("Repeating events can't be moved between Outlook and other calendars. Create it in the other calendar, then delete this one.");
        }
        if events.iter().any(|e| e.prop("ATTENDEE").is_some()) {
            bail!("Meetings can't be moved between Outlook and other calendars: deleting the original would cancel it for everyone invited.");
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Weather (Open-Meteo, no key)
    // ------------------------------------------------------------------

    pub async fn weather_forecast(&self) -> Result<Vec<DayWeather>> {
        let prefs = self.settings().calendar;
        let Some(place) = prefs.weather else { return Ok(vec![]) };
        if let Some((p, at, days)) = self.cal().weather.lock().unwrap().clone() {
            if p == place && now_ms() - at < 3_600_000 {
                return Ok(days);
            }
        }
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={:.4}&longitude={:.4}&daily=weather_code,temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=16",
            place.latitude, place.longitude
        );
        let v: serde_json::Value = self.http().get(url).send().await?.error_for_status()?.json().await?;
        let d = &v["daily"];
        let arr = |k: &str| d[k].as_array().cloned().unwrap_or_default();
        let (dates, codes, max, min) = (arr("time"), arr("weather_code"), arr("temperature_2m_max"), arr("temperature_2m_min"));
        let days: Vec<DayWeather> = dates
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                Some(DayWeather {
                    date: t.as_str()?.to_string(),
                    code: codes.get(i)?.as_i64()?,
                    max: max.get(i)?.as_f64()?,
                    min: min.get(i)?.as_f64()?,
                })
            })
            .collect();
        *self.cal().weather.lock().unwrap() = Some((place, now_ms(), days.clone()));
        Ok(days)
    }

    pub async fn weather_places(&self, query: &str) -> Result<Vec<Place>> {
        let q = query.trim();
        if q.len() < 2 {
            return Ok(vec![]);
        }
        let url = reqwest::Url::parse_with_params(
            "https://geocoding-api.open-meteo.com/v1/search",
            &[("name", q), ("count", "8"), ("language", "en"), ("format", "json")],
        )?;
        let v: serde_json::Value = self.http().get(url).send().await?.error_for_status()?.json().await?;
        Ok(v["results"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|r| {
                        Some(Place {
                            name: r["name"].as_str()?.to_string(),
                            region: r["admin1"].as_str().unwrap_or_default().to_string(),
                            country: r["country"].as_str().unwrap_or_default().to_string(),
                            latitude: r["latitude"].as_f64()?,
                            longitude: r["longitude"].as_f64()?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// Move an edited series by the change made to the occurrence that was shown:
/// timed events by the same duration, all-day events by whole days.
fn shift_series(input: &mut EventInput, master_start: i64, shown_start: i64, tz: Tz) {
    use chrono::{Duration as ChronoDuration, TimeZone};
    let len = input.end_ms - input.start_ms;
    if input.all_day {
        let day = |ms: i64| tz.timestamp_millis_opt(ms).single().map(|d| d.date_naive());
        let (Some(m), Some(shown), Some(new)) = (day(master_start), day(shown_start), day(input.start_ms)) else { return };
        let target = m + ChronoDuration::days((new - shown).num_days());
        let days = ((len as f64) / 86_400_000.0).round() as i64;
        let midnight = |d: chrono::NaiveDate| d.and_hms_opt(0, 0, 0).and_then(|n| tz.from_local_datetime(&n).earliest()).map(|t| t.timestamp_millis());
        if let (Some(s), Some(e)) = (midnight(target), midnight(target + ChronoDuration::days(days.max(1)))) {
            input.start_ms = s;
            input.end_ms = e;
        }
    } else {
        // By wall-clock time, so a move across a daylight-saving change keeps the chosen hour.
        let local = |ms: i64| tz.timestamp_millis_opt(ms).single().map(|d| d.naive_local());
        let moved = match (local(master_start), local(shown_start), local(input.start_ms)) {
            (Some(m), Some(shown), Some(new)) => tz.from_local_datetime(&(m + (new - shown))).earliest().map(|t| t.timestamp_millis()),
            _ => None,
        };
        input.start_ms = moved.unwrap_or(master_start + (input.start_ms - shown_start));
        input.end_ms = input.start_ms + len;
    }
}

/// "Today", "Tomorrow" or the weekday and date of an event start.
fn day_label(start_ms: i64, tz: Tz) -> String {
    use chrono::TimeZone;
    let (Some(d), Some(now)) = (tz.timestamp_millis_opt(start_ms).single(), tz.timestamp_millis_opt(now_ms()).single()) else {
        return String::new();
    };
    match (d.date_naive() - now.date_naive()).num_days() {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        _ => d.format("%a %-d %b").to_string(),
    }
}

/// A calendar account's portable name, the same on every device: its id
/// (synced), except Outlook calendars (`graph:<mail account>`), which each
/// device connects by itself.
pub(crate) fn cal_key(kind: &str, id: &str, mail_account_id: Option<&str>) -> String {
    match (kind, mail_account_id) {
        (KIND_GRAPH, Some(m)) => format!("graph:{m}"),
        _ => id.to_string(),
    }
}

// ----------------------------------------------------------------------------
// Background: periodic sync and reminders
// ----------------------------------------------------------------------------

pub(crate) async fn calendar_loop(weak: Weak<crate::engine::Inner>) {
    tokio::time::sleep(Duration::from_secs(8)).await;
    let mut last_sync = 0i64;
    let mut last_check = now_ms();
    let mut fired: HashSet<String> = HashSet::new();
    loop {
        let Some(inner) = weak.upgrade() else { return };
        let engine = Engine::from_inner(inner.clone());
        let now = now_ms();
        // Syncs run beside this loop (a slow feed or a first Outlook sync can
        // take minutes, and reminders mustn't wait), one at a time.
        if now - last_sync >= SYNC_EVERY.as_millis() as i64 && !engine.cal().sync_running.swap(true, Ordering::AcqRel) {
            last_sync = now;
            let engine = engine.clone();
            tokio::spawn(async move {
                struct Done<'a>(&'a AtomicBool);
                impl Drop for Done<'_> {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                let _done = Done(&engine.cal().sync_running);
                engine.cal_sync_scheduled().await;
            });
        }
        // Reminders that came due since the last look.
        let now = now_ms();
        // Alerts can be set up to a week or more ahead of the event. Expanding
        // events is CPU work: off the async threads.
        let scan = engine.clone();
        let events = tokio::task::spawn_blocking(move || scan.cal_events(now - 3_600_000, now + 15 * 86_400_000)).await;
        if let Ok(Ok(events)) = events {
            for e in events {
                for m in &e.occurrence.alarms_minutes_before {
                    let at = e.occurrence.start_ms - m * 60_000;
                    let id = format!("{}|{}", e.key, m);
                    if at > last_check && at <= now && fired.insert(id) {
                        engine.emit_event(EngineEvent::EventReminder {
                            summary: e.occurrence.summary.clone(),
                            start_ms: e.occurrence.start_ms,
                            location: e.occurrence.location.clone(),
                            all_day: e.occurrence.all_day,
                            day: day_label(e.occurrence.start_ms, local_tz()),
                        });
                    }
                }
            }
        }
        last_check = now;
        if fired.len() > 5000 {
            fired.clear();
        }
        drop(engine);
        let wake = async { inner.cal.wake.notified().await };
        tokio::select! {
            _ = wake => {}
            _ = tokio::time::sleep(Duration::from_secs(30)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrades_first_release_tables() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // cal_accounts as the first calendar release created it.
        conn.execute_batch(
            "CREATE TABLE accounts (id TEXT PRIMARY KEY);
             CREATE TABLE cal_accounts (id TEXT PRIMARY KEY, mail_account_id TEXT, name TEXT NOT NULL, url TEXT NOT NULL,
               username TEXT NOT NULL, allow_invalid_certs INTEGER NOT NULL DEFAULT 0, status TEXT NOT NULL DEFAULT 'ok',
               last_error TEXT, last_sync INTEGER, created_at INTEGER NOT NULL);
             INSERT INTO cal_accounts(id, name, url, username, created_at) VALUES('c1', 'me', 'https://dav', 'me', 1);",
        )
        .unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        upgrade(&conn).unwrap();
        upgrade(&conn).unwrap();
        let (kind, refresh): (String, Option<i64>) =
            conn.query_row("SELECT kind, refresh_secs FROM cal_accounts WHERE id = 'c1'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((kind.as_str(), refresh), (KIND_CALDAV, None));
    }

    #[test]
    fn moving_a_series_across_daylight_saving_keeps_the_hour() {
        let berlin: Tz = "Europe/Berlin".parse().unwrap();
        let ms = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis();
        // A daily 09:00 series from Jan 5; its Mar 28 occurrence (CET) is moved to Mar 29 (CEST), 09:00 both.
        let mut input = EventInput {
            summary: String::new(),
            location: String::new(),
            description: String::new(),
            start_ms: ms("2026-03-29T09:00:00+02:00"),
            end_ms: ms("2026-03-29T09:30:00+02:00"),
            all_day: false,
            tz: "Europe/Berlin".into(),
            rrule: Some("FREQ=DAILY".into()),
            alarms_minutes_before: vec![],
        };
        shift_series(&mut input, ms("2026-01-05T09:00:00+01:00"), ms("2026-03-28T09:00:00+01:00"), berlin);
        assert_eq!(input.start_ms, ms("2026-01-06T09:00:00+01:00"), "one day later, still 09:00");
        assert_eq!(input.end_ms - input.start_ms, 30 * 60_000);
    }

    #[test]
    fn upgrade_fills_in_search_text_and_open_spans() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // cal_objects as v0.1.4 created it, with a series stored with a NULL end.
        conn.execute_batch(
            "CREATE TABLE accounts (id TEXT PRIMARY KEY);
             CREATE TABLE cal_objects (calendar_id TEXT NOT NULL, href TEXT NOT NULL, etag TEXT NOT NULL, uid TEXT, ics TEXT NOT NULL,
               span_start INTEGER, span_end INTEGER, summary TEXT NOT NULL DEFAULT '', PRIMARY KEY (calendar_id, href));",
        )
        .unwrap();
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:u\r\nDTSTART:20240101T100000Z\r\nRRULE:FREQ=DAILY\r\nSUMMARY:Lunch\\, then talk\r\nLOCATION:Caf\r\n \u{e9} Riche\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        conn.execute("INSERT INTO cal_objects VALUES('c', 'h', 'e', 'u', ?, 1704103200000, NULL, 'Lunch, then talk')", [ics]).unwrap();
        conn.execute("INSERT INTO cal_objects VALUES('c', 'junk', 'e', NULL, 'not ics', NULL, NULL, '')", []).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        upgrade(&conn).unwrap();
        upgrade(&conn).unwrap();
        let (text, end): (String, i64) =
            conn.query_row("SELECT search_text, span_end FROM cal_objects WHERE href = 'h'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(text, "lunch, then talk\ncafé riche");
        assert_eq!(end, OPEN_END);
        let junk: (String, Option<i64>) = conn.query_row("SELECT search_text, span_end FROM cal_objects WHERE href = 'junk'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(junk, (String::new(), None));
    }

    #[derive(Default)]
    struct Count(std::sync::atomic::AtomicUsize);
    impl crate::EventSink for Count {
        fn emit(&self, e: EngineEvent) {
            if matches!(e, EngineEvent::CalendarChanged) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    fn engine() -> (Engine, Arc<Count>) {
        let count = Arc::new(Count::default());
        let e = Engine::with_store(Store::open_in_memory().unwrap(), Arc::new(crate::secrets::MemoryStore::default()), count.clone()).unwrap();
        e.store()
            .db()
            .execute_batch(
                "INSERT INTO cal_accounts(id, name, url, username, kind, created_at) VALUES('a', 'Feed', 'http://127.0.0.1:9/x.ics', '', 'ics', 1);
                 INSERT INTO calendars(id, cal_account_id, href, name, color, read_only) VALUES('c', 'a', 'http://127.0.0.1:9/x.ics', 'Feed', '#ff0000', 1);",
            )
            .unwrap();
        (e, count)
    }

    fn event_ics(uid: &str, start: &str, end: Option<&str>, summary: &str) -> String {
        let end = end.map(|e| format!("DTEND:{e}\r\n")).unwrap_or_default();
        let mut ev = crate::caldav::ical::Component::new("VEVENT");
        ev.props.push(crate::caldav::ical::Property::new_text("SUMMARY", summary));
        let summary = ev.to_ics().lines().filter(|l| !l.starts_with("BEGIN") && !l.starts_with("END")).collect::<Vec<_>>().join("\r\n");
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTART:{start}\r\n{end}{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n")
    }

    #[tokio::test]
    async fn search_reads_unescaped_unfolded_text() {
        let (e, _) = engine();
        let now = chrono::Utc::now();
        let at = |days: i64| (now + chrono::Duration::days(days)).format("%Y%m%dT100000Z").to_string();
        let tz = local_tz();
        // Enough events to fill the old LIMIT, all with "event" in their text (BEGIN:VEVENT).
        for i in 0..320 {
            e.store().put_object("c", &format!("s{i}"), "1", &event_ics(&format!("s{i}"), &at(1), None, "Standup"), tz).unwrap();
        }
        e.store().put_object("c", "plan", "1", &event_ics("plan", &at(2), None, "Event planning"), tz).unwrap();
        e.store().put_object("c", "lunch", "1", &event_ics("lunch", &at(3), None, "Lunch, then talk"), tz).unwrap();
        let arabic = "اجتماع مجلس الإدارة السنوي لمناقشة الميزانية والخطة الاستراتيجية للعام القادم";
        let ics = event_ics("ar", &at(4), None, arabic);
        assert!(!ics.contains("الميزانية والخطة الاستراتيجية"), "the fold splits the query");
        e.store().put_object("c", "ar", "1", &ics, tz).unwrap();
        let titles = |q: &str| e.cal_search(q).unwrap().into_iter().map(|x| x.occurrence.summary).collect::<Vec<_>>();
        assert_eq!(titles("event"), ["Event planning"]);
        assert_eq!(titles("EVENT PLAN"), ["Event planning"]);
        assert_eq!(titles("lunch, then"), ["Lunch, then talk"]);
        assert_eq!(titles("الميزانية والخطة الاستراتيجية"), [arabic]);
        assert!(titles("vevent").is_empty() && titles("dtstart").is_empty());
        assert!(titles("%").is_empty() && titles("_").is_empty());
        // People: attendees by name or address, the organizer by address.
        let meeting = format!(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:m\r\nDTSTART:{}\r\nSUMMARY:Review\r\nORGANIZER;CN=Omar Khalid:mailto:omar@example.org\r\n\
             ATTENDEE;CN=\"Ahmed, Sara\":MAILTO:sara@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            at(5)
        );
        e.store().put_object("c", "m", "1", &meeting, tz).unwrap();
        assert_eq!(titles("sara@example"), ["Review"]);
        assert_eq!(titles("ahmed, sara"), ["Review"]);
        assert_eq!(titles("omar@example"), ["Review"]);
        assert!(titles("mailto").is_empty());
    }

    #[tokio::test]
    async fn range_query_and_parsed_cache() {
        let (e, _) = engine();
        let tz = local_tz();
        let ms = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis();
        e.store().put_object("c", "zero", "1", &event_ics("zero", "20261020T100000Z", None, "Zero"), tz).unwrap();
        e.store().put_object("c", "daily", "1", "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:d\r\nDTSTART:20200101T080000Z\r\nRRULE:FREQ=DAILY\r\nSUMMARY:Daily\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n", tz).unwrap();
        let titles = |from: i64, to: i64| e.cal_events(from, to).unwrap().into_iter().map(|x| x.occurrence.summary).collect::<Vec<_>>();
        // An event of no length right at the start of the range, and a series that never ends.
        assert_eq!(titles(ms("2026-10-20T10:00:00Z"), ms("2026-10-20T12:00:00Z")), ["Zero"]);
        assert_eq!(titles(ms("2026-10-21T00:00:00Z"), ms("2026-10-22T00:00:00Z")), ["Daily"]);
        assert_eq!(e.cal().parsed.lock().unwrap().len(), 2);
        e.delete_stored("c", "zero").unwrap();
        let cached: Vec<String> = e.cal().parsed.lock().unwrap().keys().map(|(_, h)| h.clone()).collect();
        assert_eq!(cached, ["daily"]);
        let applied = e.store().replace_objects("c", &[], tz).unwrap();
        e.applied("c", &applied);
        assert!(e.cal().parsed.lock().unwrap().is_empty());
        assert!(titles(ms("2026-10-21T00:00:00Z"), ms("2026-10-22T00:00:00Z")).is_empty());
    }

    #[tokio::test]
    async fn scheduled_syncs_tell_the_ui_only_about_changes() {
        use axum::extract::State;
        type Body = Arc<Mutex<String>>;
        let body: Body = Arc::new(Mutex::new(event_ics("x", "20261020T100000Z", None, "First")));
        let app = axum::Router::new()
            .route("/x.ics", axum::routing::get(|State(b): State<Body>| async move { ([("content-type", "text/calendar")], b.lock().unwrap().clone()) }))
            .with_state(body.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let count = Arc::new(Count::default());
        let e = Engine::with_store(Store::open_in_memory().unwrap(), Arc::new(crate::secrets::MemoryStore::default()), count.clone()).unwrap();
        e.cal_subscribe(CalSubscribe { url: format!("http://{addr}/x.ics"), name: None, username: None, password: None }).await.unwrap();
        let emitted = || count.0.load(Ordering::SeqCst);
        let before = emitted();
        let due_now = |e: &Engine| e.cal().attempts.lock().unwrap().clear();
        let reset = |e: &Engine| e.store().db().execute("UPDATE cal_accounts SET last_sync = 0", []).unwrap();
        due_now(&e);
        reset(&e);
        e.cal_sync_scheduled().await;
        assert_eq!(emitted(), before, "nothing changed");
        *body.lock().unwrap() = event_ics("x", "20261020T100000Z", None, "Second");
        due_now(&e);
        reset(&e);
        e.cal_sync_scheduled().await;
        assert_eq!(emitted(), before + 1);
    }

    #[test]
    fn feed_tags_round_trip() {
        let t = FeedTag { etag: Some("W/\"a b\"".into()), modified: None, hash: "00ff".into() };
        assert_eq!(FeedTag::decode(&t.encode()), t);
        assert_eq!(FeedTag::decode(""), FeedTag::default());
    }
}
