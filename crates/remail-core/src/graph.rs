//! Outlook calendars over Microsoft Graph (Outlook.com, Hotmail, Live, MSN
//! and Microsoft 365), which offer no CalDAV.
//!
//! Each calendar is synced with a change-tracked `calendarView` over a date
//! window: the first round downloads the window, later rounds only what
//! changed. Graph expands recurring series itself, so every instance is
//! stored as its own one-VEVENT iCalendar object; `X-REMAIL-GRAPH-*`
//! properties remember the series it belongs to and the series' rule, so the
//! editor can still change "this event" or "all events".

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, Offset, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde_json::{json, Value};

use crate::caldav::client::DavError;
use crate::caldav::event::{EventInput, Occurrence};
use crate::caldav::feed::FeedObject;
use crate::caldav::ical::{Component, Property};
use crate::caldav::recur::{Freq, Recur, Until};

/// Delegated permissions: the user's own calendars (read and write) and the
/// ones shared with them (read). Both work for personal Microsoft accounts
/// and don't need an administrator.
pub const SCOPE: &str = "https://graph.microsoft.com/Calendars.ReadWrite https://graph.microsoft.com/Calendars.Read.Shared offline_access";
pub const BASE: &str = "https://graph.microsoft.com/v1.0";

/// The event's Graph id (also the object's href).
pub const X_ID: &str = "X-REMAIL-GRAPH-ID";
/// Id of the series an occurrence or exception belongs to.
pub const X_SERIES: &str = "X-REMAIL-GRAPH-SERIES";
/// Where the occurrence was originally scheduled (epoch ms): its recurrence id.
pub const X_ORIGINAL: &str = "X-REMAIL-GRAPH-ORIGINAL";
/// The series' rule as an RRULE (for display and editing only, never expanded).
pub const X_RRULE: &str = "X-REMAIL-GRAPH-RRULE";

/// Days synced before today, and after.
pub const WINDOW_PAST_DAYS: i64 = 365;
pub const WINDOW_FUTURE_DAYS: i64 = 730;
const DAY_MS: i64 = 86_400_000;

/// UTC times and plain-text bodies (smaller, and what the iCalendar side
/// wants); immutable ids, so events keep their id when Outlook moves them.
const PREFER: &str = r#"outlook.timezone="UTC", outlook.body-content-type="text", IdType="ImmutableId", odata.maxpagesize=200"#;

/// An error answer from Graph.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct GraphError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl GraphError {
    /// The change-tracking state expired: start the window over.
    pub fn sync_reset(&self) -> bool {
        self.status == 410 || ["syncStateNotFound", "resyncRequired", "SyncStateInvalid"].iter().any(|c| self.code.eq_ignore_ascii_case(c))
    }
}

/// One calendar in the user's list.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphCalendar {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    /// We can write to it (our token can't write to calendars shared with the user).
    pub writable: bool,
    pub is_default: bool,
}

/// One page of a change-tracking round.
#[derive(Debug, Default)]
pub struct DeltaPage {
    /// Added or changed events, and deleted ones (`None`), in order.
    pub changes: Vec<(String, Option<Value>)>,
    pub next: Option<String>,
    pub delta: Option<String>,
}

pub struct GraphClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl GraphClient {
    pub fn new(http: reqwest::Client, base: &str, token: String) -> GraphClient {
        GraphClient { http, base: base.trim_end_matches('/').to_string(), token }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// `{base}/me/{segments…}` with each segment escaped (ids can contain `/` and `+`).
    fn url(&self, segments: &[&str]) -> Result<reqwest::Url> {
        let mut u = reqwest::Url::parse(&format!("{}/me", self.base))?;
        u.path_segments_mut().map_err(|_| anyhow!("bad Graph address"))?.extend(segments);
        Ok(u)
    }

    /// Send a request; retries when Graph is busy (429/503/504, honouring Retry-After).
    async fn send(&self, method: reqwest::Method, url: reqwest::Url, body: Option<&Value>, if_match: Option<&str>) -> Result<Option<Value>> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let mut req = self
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(&self.token)
                .header("Prefer", PREFER)
                .timeout(Duration::from_secs(60));
            if let Some(b) = body {
                req = req.json(b);
            }
            if let Some(e) = if_match.filter(|e| !e.is_empty()) {
                req = req.header(reqwest::header::IF_MATCH, e);
            }
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
            if status == 412 {
                return Err(DavError::Conflict.into());
            }
            let text = resp.text().await.map_err(|e| e.without_url()).context("reading Microsoft's answer")?;
            if status == 409 && text.contains("ErrorIrresolvableConflict") {
                return Err(DavError::Conflict.into());
            }
            if (200..300).contains(&status) {
                if text.trim().is_empty() {
                    return Ok(None);
                }
                return Ok(Some(serde_json::from_str(&text).context("Microsoft sent an unreadable answer")?));
            }
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let code = v["error"]["code"].as_str().unwrap_or_default().to_string();
            let message = match v["error"]["message"].as_str().map(str::trim).filter(|m| !m.is_empty()) {
                Some(m) => format!("Microsoft: {m} (HTTP {status})"),
                None => format!("Microsoft answered HTTP {status}"),
            };
            return Err(GraphError { status, code, message }.into());
        }
    }

    async fn get(&self, url: reqwest::Url) -> Result<Value> {
        self.send(reqwest::Method::GET, url, None, None).await?.context("Microsoft sent an empty answer")
    }

    /// A GET that names the properties it wants (smaller answers). Microsoft's
    /// live service doesn't know every property its documentation lists, and
    /// refuses the whole request then (HTTP 400): ask again for everything.
    async fn get_selected(&self, url: reqwest::Url) -> Result<Value> {
        match self.get(url.clone()).await {
            Err(e) if e.downcast_ref::<GraphError>().is_some_and(|g| g.status == 400) => {
                tracing::debug!("Graph refused a property list: {e:#}");
                let mut all = url.clone();
                let kept: Vec<(String, String)> = url.query_pairs().filter(|(k, _)| k != "$select").map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
                all.set_query(None);
                if !kept.is_empty() {
                    all.query_pairs_mut().extend_pairs(kept);
                }
                self.get(all).await
            }
            r => r,
        }
    }

    /// A paging or change link from an answer (or saved from one): only ever
    /// followed to Graph itself, since the token goes with it.
    fn link(&self, url: &str) -> Result<reqwest::Url> {
        if !url.starts_with(&format!("{}/", self.base)) {
            return Err(anyhow!("Microsoft sent a link to somewhere else"));
        }
        Ok(reqwest::Url::parse(url)?)
    }

    /// All the user's calendars (every calendar group, and calendars shared
    /// with them). `me`: the account's address, used when Microsoft doesn't say
    /// whose mailbox this is.
    pub async fn calendars(&self, me: &str) -> Result<Vec<GraphCalendar>> {
        let mut url = self.url(&["calendars"])?;
        url.query_pairs_mut().append_pair("$select", "id,name,color,hexColor,canEdit,isDefaultCalendar,owner").append_pair("$top", "100");
        let mut raw: Vec<Value> = Vec::new();
        for _ in 0..20 {
            let v = self.get_selected(url.clone()).await?;
            raw.extend(v["value"].as_array().into_iter().flatten().cloned());
            match v["@odata.nextLink"].as_str() {
                Some(n) => url = self.link(n)?,
                None => break,
            }
        }
        // The mailbox's own address is the main calendar's owner (it may be another alias than the sign-in's).
        let owner = |c: &Value| c["owner"]["address"].as_str().map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty());
        let mailbox = raw.iter().find(|c| c["isDefaultCalendar"].as_bool() == Some(true)).and_then(owner).unwrap_or_else(|| me.trim().to_lowercase());
        Ok(raw.iter().filter_map(|c| calendar_from(c, &mailbox)).collect())
    }

    /// The first request of a full round for `[from_ms, to_ms)` of a calendar.
    pub fn delta_start(&self, calendar_id: &str, from_ms: i64, to_ms: i64) -> Result<String> {
        let mut u = self.url(&["calendars", calendar_id, "calendarView", "delta"])?;
        u.query_pairs_mut().append_pair("startDateTime", &iso_utc(from_ms)).append_pair("endDateTime", &iso_utc(to_ms));
        Ok(u.into())
    }

    /// One page of a change-tracking round (`url`: a start, next or delta link).
    pub async fn delta_page(&self, url: &str) -> Result<DeltaPage> {
        let v = self.get(self.link(url)?).await?;
        let mut page = DeltaPage {
            next: v["@odata.nextLink"].as_str().map(String::from),
            delta: v["@odata.deltaLink"].as_str().map(String::from),
            ..Default::default()
        };
        for e in v["value"].as_array().into_iter().flatten() {
            let Some(id) = e["id"].as_str() else { continue };
            let gone = e.get("@removed").is_some();
            page.changes.push((id.to_string(), (!gone).then(|| e.clone())));
        }
        Ok(page)
    }

    /// Every event in `[from_ms, to_ms)` without change tracking (for calendars that don't offer it).
    pub async fn list_view(&self, calendar_id: &str, from_ms: i64, to_ms: i64) -> Result<Vec<Value>> {
        let mut url = self.url(&["calendars", calendar_id, "calendarView"])?;
        url.query_pairs_mut().append_pair("startDateTime", &iso_utc(from_ms)).append_pair("endDateTime", &iso_utc(to_ms)).append_pair("$top", "200");
        let mut out = Vec::new();
        for _ in 0..500 {
            let v = self.get(url.clone()).await?;
            out.extend(v["value"].as_array().into_iter().flatten().cloned());
            match v["@odata.nextLink"].as_str() {
                Some(n) => url = self.link(n)?,
                None => return Ok(out),
            }
        }
        Err(anyhow!("Microsoft kept sending more events"))
    }

    /// A series' start, end and rule. (Events are addressed through their
    /// calendar, which also works for calendars shared with the user.)
    pub async fn series(&self, calendar_id: &str, id: &str) -> Result<Value> {
        let mut u = self.url(&["calendars", calendar_id, "events", id])?;
        u.query_pairs_mut().append_pair("$select", "id,start,end,isAllDay,recurrence,originalStartTimeZone");
        self.get_selected(u).await
    }

    pub async fn create(&self, calendar_id: &str, body: &Value) -> Result<Value> {
        let u = self.url(&["calendars", calendar_id, "events"])?;
        self.send(reqwest::Method::POST, u, Some(body), None).await?.context("Microsoft didn't return the new event")
    }

    /// Change an event (only if it still has `etag`, when given).
    pub async fn update(&self, calendar_id: &str, id: &str, body: &Value, etag: Option<&str>) -> Result<Option<Value>> {
        let u = self.url(&["calendars", calendar_id, "events", id])?;
        self.send(reqwest::Method::PATCH, u, Some(body), etag).await
    }

    /// Delete an event or a whole series; one that's already gone counts as deleted.
    pub async fn delete(&self, calendar_id: &str, id: &str, etag: Option<&str>) -> Result<()> {
        let u = self.url(&["calendars", calendar_id, "events", id])?;
        match self.send(reqwest::Method::DELETE, u, None, etag).await {
            Err(e) if e.downcast_ref::<GraphError>().is_some_and(|g| g.status == 404) => Ok(()),
            r => r.map(|_| ()),
        }
    }
}

/// `mailbox`: the user's own address; calendars owned by someone else were
/// shared with the user, and our permission only reads those.
fn calendar_from(v: &Value, mailbox: &str) -> Option<GraphCalendar> {
    let id = v["id"].as_str()?.to_string();
    let hex = v["hexColor"].as_str().map(str::trim).filter(|h| h.starts_with('#') && h.len() == 7).map(str::to_lowercase);
    let named = match v["color"].as_str().unwrap_or_default() {
        "lightBlue" => Some("#3b82f6"),
        "lightGreen" => Some("#22c55e"),
        "lightOrange" => Some("#f97316"),
        "lightGray" => Some("#8e8e93"),
        "lightYellow" => Some("#eab308"),
        "lightTeal" => Some("#14b8a6"),
        "lightPink" => Some("#ec4899"),
        "lightBrown" => Some("#a2845e"),
        "lightRed" => Some("#ef4444"),
        _ => None,
    };
    let owner = v["owner"]["address"].as_str().map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty());
    let shared = v["isSharedWithMe"].as_bool() == Some(true) || owner.is_some_and(|o| !mailbox.is_empty() && o != mailbox);
    Some(GraphCalendar {
        id,
        name: v["name"].as_str().unwrap_or("Calendar").to_string(),
        color: hex.or(named.map(String::from)),
        writable: v["canEdit"].as_bool().unwrap_or(false) && !shared,
        is_default: v["isDefaultCalendar"].as_bool().unwrap_or(false),
    })
}

fn iso_utc(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms).unwrap_or_default().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The window synced around `now`: from the start of the day a year ago to two years ahead.
pub fn window(now_ms: i64) -> (i64, i64) {
    let from = (now_ms - WINDOW_PAST_DAYS * DAY_MS).div_euclid(DAY_MS) * DAY_MS;
    (from, from + (WINDOW_PAST_DAYS + WINDOW_FUTURE_DAYS + 1) * DAY_MS)
}

// ----------------------------------------------------------------------------
// Graph event → iCalendar
// ----------------------------------------------------------------------------

/// A `dateTimeTimeZone` as UTC (we ask for UTC; other zones are converted when we know them).
fn graph_time(v: &Value) -> Option<NaiveDateTime> {
    let s = v["dateTime"].as_str()?;
    let naive = NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%dT%H:%M:%S%.f").ok()?;
    let zone = v["timeZone"].as_str().unwrap_or("UTC");
    if zone.eq_ignore_ascii_case("UTC") || zone.is_empty() {
        return Some(naive);
    }
    match Tz::from_str(zone) {
        Ok(tz) => tz.from_local_datetime(&naive).earliest().map(|t| t.naive_utc()),
        Err(_) => Some(naive),
    }
}

/// The date of an all-day event's start or end. Graph gives their local
/// midnights converted to UTC (21:00 the day before for Riyadh, 04:00 for New
/// York). In the event's own zone (`originalStartTimeZone`, a Windows or IANA
/// name) that is its midnight; without a zone we know, the nearest midnight
/// is the date (right from UTC−11 to UTC+12).
pub fn all_day_date(t: NaiveDateTime, zone: Option<&str>) -> NaiveDate {
    use chrono::Timelike;
    let local = match zone.and_then(crate::caldav::tz::resolve_tzid) {
        Some(tz) => tz.from_utc_datetime(&t).naive_local(),
        None => t,
    };
    if local.hour() >= 12 {
        local.date().succ_opt().unwrap_or(local.date())
    } else {
        local.date()
    }
}

fn ical_utc(t: NaiveDateTime) -> String {
    t.format("%Y%m%dT%H%M%SZ").to_string()
}

fn partstat(response: &str) -> &'static str {
    match response {
        "accepted" | "organizer" => "ACCEPTED",
        "declined" => "DECLINED",
        "tentativelyAccepted" => "TENTATIVE",
        _ => "NEEDS-ACTION",
    }
}

/// Text from Microsoft without control characters (a line break in a name
/// would otherwise start a new iCalendar line).
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string()
}

fn person(name: &str, v: &Value) -> Option<Property> {
    let addr = v["emailAddress"]["address"].as_str().map(clean).filter(|a| !a.is_empty() && !a.contains([';', ':', ',']))?;
    let mut p = Property::new(name, &format!("mailto:{addr}"));
    if let Some(cn) = v["emailAddress"]["name"].as_str().map(clean).filter(|n| !n.is_empty()) {
        p = p.with_param("CN", &cn.replace('"', "'"));
    }
    Some(p)
}

/// An event from Graph as an iCalendar object (None for series masters and
/// anything without a usable time). `rrule`: the series' rule, if known.
pub fn to_object(ev: &Value, rrule: Option<&str>) -> Option<FeedObject> {
    let id = ev["id"].as_str().map(clean).filter(|i| !i.is_empty())?;
    let id = id.as_str();
    if ev["type"].as_str() == Some("seriesMaster") {
        return None;
    }
    let start = graph_time(&ev["start"])?;
    let end = graph_time(&ev["end"]).filter(|e| *e >= start).unwrap_or(start);
    let all_day = ev["isAllDay"].as_bool().unwrap_or(false);
    let text = |v: &Value| v.as_str().map(|s| s.replace("\r\n", "\n").trim().to_string()).unwrap_or_default();

    let mut e = Component::new("VEVENT");
    let uid = ev["iCalUId"].as_str().map(clean).filter(|u| !u.is_empty()).unwrap_or_else(|| id.to_string());
    e.props.push(Property::new_text("UID", &uid));
    let stamp = ev["lastModifiedDateTime"].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|d| d.naive_utc()).unwrap_or(start);
    e.props.push(Property::new("DTSTAMP", &ical_utc(stamp)));
    if all_day {
        let zone = |k: &str| ev[k].as_str().filter(|z| !z.trim().is_empty());
        let start_zone = zone("originalStartTimeZone");
        let (s, mut en) = (all_day_date(start, start_zone), all_day_date(end, zone("originalEndTimeZone").or(start_zone)));
        if en <= s {
            en = s.succ_opt().unwrap_or(s);
        }
        e.props.push(Property::new("DTSTART", &s.format("%Y%m%d").to_string()).with_param("VALUE", "DATE"));
        e.props.push(Property::new("DTEND", &en.format("%Y%m%d").to_string()).with_param("VALUE", "DATE"));
    } else {
        e.props.push(Property::new("DTSTART", &ical_utc(start)));
        e.props.push(Property::new("DTEND", &ical_utc(end)));
    }
    e.props.push(Property::new_text("SUMMARY", &text(&ev["subject"])));
    let location = text(&ev["location"]["displayName"]);
    if !location.is_empty() {
        e.props.push(Property::new_text("LOCATION", &location));
    }
    let body = text(&ev["body"]["content"]);
    if !body.is_empty() {
        e.props.push(Property::new_text("DESCRIPTION", &body));
    }
    let join = ev["onlineMeeting"]["joinUrl"].as_str().or(ev["onlineMeetingUrl"].as_str()).map(clean).filter(|u| u.starts_with("https://"));
    if let Some(u) = join {
        e.props.push(Property::new("URL", &u));
    }
    if ev["isCancelled"].as_bool() == Some(true) {
        e.props.push(Property::new("STATUS", "CANCELLED"));
    } else if ev["showAs"].as_str() == Some("tentative") {
        e.props.push(Property::new("STATUS", "TENTATIVE"));
    }
    if ev["showAs"].as_str() == Some("free") {
        e.props.push(Property::new("TRANSP", "TRANSPARENT"));
    }
    if let Some(p) = person("ORGANIZER", &ev["organizer"]) {
        e.props.push(p);
    }
    for a in ev["attendees"].as_array().into_iter().flatten() {
        if let Some(p) = person("ATTENDEE", a) {
            e.props.push(p.with_param("PARTSTAT", partstat(a["status"]["response"].as_str().unwrap_or_default())));
        }
    }
    e.props.push(Property::new_text(X_ID, id));
    if let Some(series) = ev["seriesMasterId"].as_str().map(clean).filter(|s| !s.is_empty()) {
        e.props.push(Property::new_text(X_SERIES, &series));
        let original = ev["originalStart"].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp_millis());
        let rid = original.unwrap_or_else(|| Utc.from_utc_datetime(&start).timestamp_millis());
        e.props.push(Property::new(X_ORIGINAL, &rid.to_string()));
        if let Some(r) = rrule {
            e.props.push(Property::new_text(X_RRULE, &clean(r)));
        }
    }
    if ev["isReminderOn"].as_bool() == Some(true) {
        let minutes = ev["reminderMinutesBeforeStart"].as_i64().unwrap_or(15).max(0);
        let mut alarm = Component::new("VALARM");
        alarm.props.push(Property::new("ACTION", "DISPLAY"));
        alarm.props.push(Property::new_text("DESCRIPTION", "Reminder"));
        alarm.props.push(Property::new("TRIGGER", &format!("-PT{minutes}M")));
        e.children.push(alarm);
    }
    let mut cal = Component::new("VCALENDAR");
    cal.props.push(Property::new("VERSION", "2.0"));
    cal.props.push(Property::new("PRODID", "-//Remail//Outlook calendar//EN"));
    cal.children.push(e);
    let etag = clean(ev["@odata.etag"].as_str().or(ev["changeKey"].as_str()).unwrap_or_default());
    // The rule isn't part of Graph's etag for an occurrence, but is part of what we store.
    let etag = if rrule.is_some() { format!("{etag}|{}", rrule.unwrap_or_default()) } else { etag };
    Some(FeedObject { href: id.to_string(), etag, ics: cal.to_ics() })
}

/// The Graph etag inside a stored etag (see [`to_object`]), for If-Match.
pub fn graph_etag(stored: &str) -> Option<&str> {
    stored.split('|').next().filter(|e| !e.is_empty())
}

/// For an object that is one occurrence of a series: `(series id, recurrence id, the series' rule)`.
pub fn series_of(cal: &Component) -> Option<(String, Option<i64>, Option<String>)> {
    let ev = cal.children.iter().find(|c| c.name == "VEVENT")?;
    let series = ev.prop(X_SERIES).map(|p| p.text()).filter(|s| !s.is_empty())?;
    let original = ev.prop(X_ORIGINAL).and_then(|p| p.value.trim().parse().ok());
    let rrule = ev.prop(X_RRULE).map(|p| p.text()).filter(|s| !s.is_empty());
    Some((series, original, rrule))
}

// ----------------------------------------------------------------------------
// Recurrence: Graph patterns ↔ RRULE
// ----------------------------------------------------------------------------

const DAYS: [(&str, &str, Weekday); 7] = [
    ("sunday", "SU", Weekday::Sun),
    ("monday", "MO", Weekday::Mon),
    ("tuesday", "TU", Weekday::Tue),
    ("wednesday", "WE", Weekday::Wed),
    ("thursday", "TH", Weekday::Thu),
    ("friday", "FR", Weekday::Fri),
    ("saturday", "SA", Weekday::Sat),
];
const INDEXES: [(&str, i32); 5] = [("first", 1), ("second", 2), ("third", 3), ("fourth", 4), ("last", -1)];

fn day_code(graph: &str) -> Option<&'static str> {
    DAYS.iter().find(|d| d.0.eq_ignore_ascii_case(graph)).map(|d| d.1)
}

fn day_name(w: Weekday) -> &'static str {
    DAYS.iter().find(|d| d.2 == w).map(|d| d.0).unwrap_or("monday")
}

/// A Graph `patternedRecurrence` as an RRULE (None for patterns it can't express).
pub fn rrule_from(rec: &Value) -> Option<String> {
    let p = &rec["pattern"];
    let r = &rec["range"];
    let days: Vec<&str> = p["daysOfWeek"].as_array().into_iter().flatten().filter_map(|d| d.as_str().and_then(day_code)).collect();
    let index = INDEXES.iter().find(|i| Some(i.0) == p["index"].as_str()).map(|i| i.1).unwrap_or(1);
    let relative = |parts: &mut Vec<String>| {
        if days.len() == 1 {
            parts.push(format!("BYDAY={index}{}", days[0]));
        } else if !days.is_empty() {
            parts.push(format!("BYDAY={};BYSETPOS={index}", days.join(",")));
        }
    };
    let mut parts = Vec::new();
    match p["type"].as_str()? {
        "daily" => parts.push("FREQ=DAILY".to_string()),
        "weekly" => {
            parts.push("FREQ=WEEKLY".into());
            if !days.is_empty() {
                parts.push(format!("BYDAY={}", days.join(",")));
            }
        }
        "absoluteMonthly" => {
            parts.push("FREQ=MONTHLY".into());
            parts.push(format!("BYMONTHDAY={}", p["dayOfMonth"].as_u64().filter(|d| (1..=31).contains(d))?));
        }
        "relativeMonthly" => {
            parts.push("FREQ=MONTHLY".into());
            relative(&mut parts);
        }
        "absoluteYearly" => {
            parts.push("FREQ=YEARLY".into());
            parts.push(format!("BYMONTH={}", p["month"].as_u64().filter(|m| (1..=12).contains(m))?));
            parts.push(format!("BYMONTHDAY={}", p["dayOfMonth"].as_u64().filter(|d| (1..=31).contains(d))?));
        }
        "relativeYearly" => {
            parts.push("FREQ=YEARLY".into());
            parts.push(format!("BYMONTH={}", p["month"].as_u64().filter(|m| (1..=12).contains(m))?));
            relative(&mut parts);
        }
        _ => return None,
    }
    let interval = p["interval"].as_u64().unwrap_or(1);
    if interval > 1 {
        parts.insert(1, format!("INTERVAL={interval}"));
        if p["type"] == "weekly" {
            if let Some(w) = p["firstDayOfWeek"].as_str().and_then(day_code).filter(|w| *w != "MO") {
                parts.push(format!("WKST={w}"));
            }
        }
    }
    match r["type"].as_str() {
        Some("endDate") => {
            let end = NaiveDate::parse_from_str(r["endDate"].as_str()?, "%Y-%m-%d").ok()?;
            parts.push(format!("UNTIL={}", end.format("%Y%m%d")));
        }
        Some("numbered") => parts.push(format!("COUNT={}", r["numberOfOccurrences"].as_u64().filter(|n| *n > 0)?)),
        _ => {}
    }
    Some(parts.join(";"))
}

/// An RRULE as a Graph `patternedRecurrence` for a series starting on `start` in zone `tz`.
pub fn recurrence_for(rrule: &str, start: NaiveDate, tz: &str) -> Result<Value> {
    let r = Recur::parse(rrule)?;
    let unsupported = || anyhow!("Outlook can't repeat an event this way ({rrule}). Pick another repeat option.");
    if !(r.by_second.is_empty() && r.by_minute.is_empty() && r.by_hour.is_empty() && r.by_yearday.is_empty() && r.by_weekno.is_empty())
        || r.freq < Freq::Daily
    {
        return Err(unsupported());
    }
    let days = |r: &Recur| -> Vec<&'static str> { r.by_day.iter().map(|(_, w)| day_name(*w)).collect() };
    // A relative pattern's position: BYDAY=2TU, or BYDAY=TU,TH;BYSETPOS=2.
    let index = |r: &Recur| -> Result<Option<&'static str>> {
        let ord = r.by_day.iter().map(|(n, _)| *n).find(|n| *n != 0).or(r.by_setpos.first().copied());
        match ord {
            None => Ok(None),
            Some(n) => INDEXES.iter().find(|i| i.1 == n).map(|i| Some(i.0)).ok_or_else(unsupported),
        }
    };
    let interval = r.interval.max(1);
    let pattern = match r.freq {
        // Every weekday and the like: Outlook calls that weekly.
        Freq::Daily if !r.by_day.is_empty() => json!({"type": "weekly", "interval": interval, "daysOfWeek": days(&r), "firstDayOfWeek": day_name(r.wkst)}),
        Freq::Daily => json!({"type": "daily", "interval": interval}),
        Freq::Weekly => {
            let mut d = days(&r);
            if d.is_empty() {
                d.push(day_name(start.weekday()));
            }
            json!({"type": "weekly", "interval": interval, "daysOfWeek": d, "firstDayOfWeek": day_name(r.wkst)})
        }
        Freq::Monthly => match (r.by_monthday.first(), index(&r)?) {
            (Some(d), _) if *d > 0 => json!({"type": "absoluteMonthly", "interval": interval, "dayOfMonth": d}),
            (None, Some(i)) if !r.by_day.is_empty() => json!({"type": "relativeMonthly", "interval": interval, "daysOfWeek": days(&r), "index": i}),
            (None, None) if r.by_day.is_empty() => json!({"type": "absoluteMonthly", "interval": interval, "dayOfMonth": start.day()}),
            _ => return Err(unsupported()),
        },
        Freq::Yearly => {
            let month = r.by_month.first().copied().unwrap_or(start.month());
            match (r.by_monthday.first(), index(&r)?) {
                (Some(d), _) if *d > 0 => json!({"type": "absoluteYearly", "interval": interval, "month": month, "dayOfMonth": d}),
                (None, Some(i)) if !r.by_day.is_empty() => json!({"type": "relativeYearly", "interval": interval, "month": month, "daysOfWeek": days(&r), "index": i}),
                (None, None) if r.by_day.is_empty() => json!({"type": "absoluteYearly", "interval": interval, "month": month, "dayOfMonth": start.day()}),
                _ => return Err(unsupported()),
            }
        }
        _ => return Err(unsupported()),
    };
    let start_s = start.format("%Y-%m-%d").to_string();
    let mut range = match (r.count, r.until) {
        (Some(n), _) => json!({"type": "numbered", "startDate": start_s, "numberOfOccurrences": n}),
        (None, Some(u)) => {
            let end = match u {
                Until::Date(d) => d,
                Until::Local(t) => t.date(),
                Until::Utc(t) => Tz::from_str(tz).map(|z| z.from_utc_datetime(&t).date_naive()).unwrap_or(t.date()),
            };
            json!({"type": "endDate", "startDate": start_s, "endDate": end.format("%Y-%m-%d").to_string()})
        }
        (None, None) => json!({"type": "noEnd", "startDate": start_s}),
    };
    range["recurrenceTimeZone"] = json!(tz);
    Ok(json!({"pattern": pattern, "range": range}))
}

// ----------------------------------------------------------------------------
// Editor input → Graph event
// ----------------------------------------------------------------------------

/// IANA zones Graph takes besides Windows names (from its dateTimeTimeZone reference).
const GRAPH_ZONES: &[&str] = &[
    "Etc/GMT+12", "Etc/GMT+11", "Pacific/Honolulu", "America/Anchorage", "America/Santa_Isabel", "America/Los_Angeles", "America/Phoenix",
    "America/Chihuahua", "America/Denver", "America/Guatemala", "America/Chicago", "America/Mexico_City", "America/Regina", "America/Bogota",
    "America/New_York", "America/Indiana/Indianapolis", "America/Caracas", "America/Asuncion", "America/Halifax", "America/Cuiaba",
    "America/La_Paz", "America/Santiago", "America/St_Johns", "America/Sao_Paulo", "America/Argentina/Buenos_Aires", "America/Cayenne",
    "America/Godthab", "America/Montevideo", "America/Bahia", "Etc/GMT+2", "Atlantic/Azores", "Atlantic/Cape_Verde", "Africa/Casablanca",
    "Etc/GMT", "Europe/London", "Atlantic/Reykjavik", "Europe/Berlin", "Europe/Budapest", "Europe/Paris", "Europe/Warsaw", "Africa/Lagos",
    "Africa/Windhoek", "Europe/Bucharest", "Asia/Beirut", "Africa/Cairo", "Asia/Damascus", "Africa/Johannesburg", "Europe/Kyiv",
    "Europe/Istanbul", "Asia/Jerusalem", "Asia/Amman", "Asia/Baghdad", "Europe/Kaliningrad", "Asia/Riyadh", "Africa/Nairobi", "Asia/Tehran",
    "Asia/Dubai", "Asia/Baku", "Europe/Moscow", "Indian/Mauritius", "Asia/Tbilisi", "Asia/Yerevan", "Asia/Kabul", "Asia/Karachi",
    "Asia/Tashkent", "Asia/Kolkata", "Asia/Colombo", "Asia/Kathmandu", "Asia/Almaty", "Asia/Dhaka", "Asia/Yekaterinburg", "Asia/Yangon",
    "Asia/Bangkok", "Asia/Novosibirsk", "Asia/Shanghai", "Asia/Krasnoyarsk", "Asia/Singapore", "Australia/Perth", "Asia/Taipei",
    "Asia/Ulaanbaatar", "Asia/Irkutsk", "Asia/Tokyo", "Asia/Seoul", "Australia/Adelaide", "Australia/Darwin", "Australia/Brisbane",
    "Australia/Sydney", "Pacific/Port_Moresby", "Australia/Hobart", "Asia/Yakutsk", "Pacific/Guadalcanal", "Asia/Vladivostok",
    "Pacific/Auckland", "Etc/GMT-12", "Pacific/Fiji", "Asia/Magadan", "Pacific/Tongatapu", "Pacific/Apia", "Pacific/Kiritimati",
];

/// The zone to give Graph for the user's zone: itself when Graph knows it,
/// else one Graph knows with the same offsets (summer and winter), else UTC.
pub fn graph_zone(name: &str, around_ms: i64) -> (String, Tz) {
    let Ok(tz) = Tz::from_str(name) else { return ("UTC".into(), Tz::UTC) };
    let alias = match name {
        // Windows calls it UTC too.
        "UTC" | "Etc/UTC" | "Etc/Universal" | "Universal" | "Zulu" => return ("UTC".into(), Tz::UTC),
        "Asia/Aden" | "Asia/Kuwait" | "Asia/Bahrain" | "Asia/Qatar" => Some("Asia/Riyadh"),
        "Asia/Muscat" => Some("Asia/Dubai"),
        _ => None,
    };
    if let Some(a) = alias.or(GRAPH_ZONES.contains(&name).then_some(name)) {
        return (a.to_string(), Tz::from_str(a).unwrap_or(tz));
    }
    let year = DateTime::<Utc>::from_timestamp_millis(around_ms).unwrap_or_default().year();
    let offset_on = |z: Tz, month: u32| -> Option<i32> {
        let noon = NaiveDate::from_ymd_opt(year, month, 1)?.and_hms_opt(12, 0, 0)?;
        Some(z.from_utc_datetime(&noon).offset().fix().local_minus_utc())
    };
    let offsets = |z: Tz| (offset_on(z, 1), offset_on(z, 7));
    let want = offsets(tz);
    // Same offsets, preferring the same part of the world.
    let area = name.split('/').next().unwrap_or_default();
    let mut candidates: Vec<&str> = GRAPH_ZONES.to_vec();
    candidates.sort_by_key(|z| !z.starts_with(area));
    for z in candidates {
        if let Ok(candidate) = Tz::from_str(z) {
            if want.0.is_some() && offsets(candidate) == want {
                return (z.to_string(), candidate);
            }
        }
    }
    ("UTC".into(), Tz::UTC)
}

fn wall(ms: i64, tz: Tz) -> NaiveDateTime {
    tz.from_utc_datetime(&DateTime::<Utc>::from_timestamp_millis(ms).unwrap_or_default().naive_utc()).naive_local()
}

/// The zone Graph gets, and the start and end written in it. All-day events
/// are dates in the user's own zone (at midnight); timed ones are wall-clock
/// times in the zone Graph knows (UTC when it knows none like the user's).
fn times(input: &EventInput) -> (String, NaiveDateTime, NaiveDateTime) {
    let (zone, tz) = graph_zone(&input.tz, input.start_ms);
    let end_ms = input.end_ms.max(input.start_ms);
    if input.all_day {
        let own = Tz::from_str(&input.tz).unwrap_or(tz);
        let start = wall(input.start_ms, own).date();
        let mut end = wall(end_ms, own).date();
        if end <= start {
            end = start.succ_opt().unwrap_or(start);
        }
        let midnight = |d: NaiveDate| d.and_hms_opt(0, 0, 0).unwrap_or_default();
        return (zone, midnight(start), midnight(end));
    }
    (zone, wall(input.start_ms, tz), wall(end_ms, tz))
}

/// The date the event starts on, as written to Graph (a recurrence's start date must match it).
pub fn start_date(input: &EventInput) -> NaiveDate {
    times(input).1.date()
}

/// The fields the editor sets, as a Graph event body.
pub fn event_body(input: &EventInput) -> Value {
    let (zone, start, end) = times(input);
    let fmt = |t: NaiveDateTime| t.format("%Y-%m-%dT%H:%M:%S").to_string();
    let mut body = json!({
        "subject": input.summary,
        "body": {"contentType": "text", "content": input.description},
        "location": {"displayName": input.location},
        "start": {"dateTime": fmt(start), "timeZone": zone},
        "end": {"dateTime": fmt(end), "timeZone": zone},
        "isAllDay": input.all_day,
        "isReminderOn": !input.alarms_minutes_before.is_empty(),
    });
    if let Some(m) = input.alarms_minutes_before.first() {
        body["reminderMinutesBeforeStart"] = json!((*m).max(0));
    }
    body
}

/// Only what the editor changed compared with `old` (as the event was shown):
/// a body or location left alone keeps Outlook's richer version, and an
/// unchanged meeting isn't sent to its attendees again. `new` holds the
/// values to write (for a whole series, its times moved to the series' start).
pub fn patch_body(new: &EventInput, edited: &EventInput, old: &Occurrence) -> Value {
    let full = event_body(new);
    let mut out = json!({});
    if edited.summary != old.summary {
        out["subject"] = full["subject"].clone();
    }
    if edited.description.trim() != old.description.trim() {
        out["body"] = full["body"].clone();
    }
    if edited.location.trim() != old.location.trim() {
        out["location"] = full["location"].clone();
    }
    if edited.start_ms != old.start_ms || edited.end_ms != old.end_ms || edited.all_day != old.all_day {
        for k in ["start", "end", "isAllDay"] {
            out[k] = full[k].clone();
        }
    }
    if edited.alarms_minutes_before.first() != old.alarms_minutes_before.first() {
        out["isReminderOn"] = full["isReminderOn"].clone();
        if let Some(m) = full.get("reminderMinutesBeforeStart") {
            out["reminderMinutesBeforeStart"] = m.clone();
        }
    }
    out
}

/// A series' own recurrence, starting on `start` (when the series moves by days).
pub fn moved_recurrence(rec: &Value, start: NaiveDate) -> Option<Value> {
    let mut r = rec.clone();
    r.get("pattern")?;
    r["range"]["startDate"] = json!(start.format("%Y-%m-%d").to_string());
    Some(r)
}

/// When an event starts, as epoch ms; for all-day events, midnight of its
/// date in zone `tz` (as the editor has it).
pub fn start_ms(ev: &Value, tz: &str) -> Option<i64> {
    let t = graph_time(&ev["start"])?;
    if ev["isAllDay"].as_bool() == Some(true) {
        let zone = Tz::from_str(tz).unwrap_or(Tz::UTC);
        let midnight = all_day_date(t, ev["originalStartTimeZone"].as_str()).and_hms_opt(0, 0, 0)?;
        return zone.from_local_datetime(&midnight).earliest().map(|d| d.timestamp_millis());
    }
    Some(Utc.from_utc_datetime(&t).timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caldav::event;

    fn input(start: &str, end: &str, tz: &str, all_day: bool) -> EventInput {
        let ms = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis();
        EventInput {
            summary: "Standup".into(),
            location: "Room 1".into(),
            description: "Notes".into(),
            start_ms: ms(start),
            end_ms: ms(end),
            all_day,
            tz: tz.into(),
            rrule: None,
            alarms_minutes_before: vec![10],
        }
    }

    #[test]
    fn events_become_icalendar() {
        let ev = json!({
            "id": "AAMk=/x+1", "@odata.etag": "W/\"abc\"", "iCalUId": "ical-1", "type": "occurrence",
            "seriesMasterId": "MASTER", "originalStart": "2026-10-20T06:00:00Z",
            "subject": "Standup, daily", "body": {"contentType": "text", "content": "Line 1\r\nLine 2"},
            "start": {"dateTime": "2026-10-20T06:30:00.0000000", "timeZone": "UTC"},
            "end": {"dateTime": "2026-10-20T07:00:00.0000000", "timeZone": "UTC"},
            "location": {"displayName": "Teams"}, "isReminderOn": true, "reminderMinutesBeforeStart": 15,
            "showAs": "busy", "onlineMeeting": {"joinUrl": "https://teams.microsoft.com/l/x"},
            "organizer": {"emailAddress": {"name": "Sara", "address": "sara@contoso.com"}},
            "attendees": [{"emailAddress": {"name": "Aziz", "address": "aziz@outlook.com"}, "status": {"response": "tentativelyAccepted"}}]
        });
        let o = to_object(&ev, Some("FREQ=WEEKLY;BYDAY=TU")).unwrap();
        assert_eq!(o.href, "AAMk=/x+1");
        assert_eq!(graph_etag(&o.etag), Some("W/\"abc\""));
        let cal = Component::parse(&o.ics).unwrap();
        let occ = event::occurrences(&cal, 0, i64::MAX / 4, Tz::UTC).unwrap();
        assert_eq!(occ.len(), 1, "one instance, no expansion");
        let o1 = &occ[0];
        assert_eq!(o1.summary, "Standup, daily");
        assert_eq!(o1.description, "Line 1\nLine 2");
        assert_eq!(o1.start_ms, DateTime::parse_from_rfc3339("2026-10-20T06:30:00Z").unwrap().timestamp_millis());
        assert_eq!(o1.end_ms - o1.start_ms, 30 * 60_000);
        assert_eq!(o1.alarms_minutes_before, vec![15]);
        assert_eq!(o1.url, "https://teams.microsoft.com/l/x");
        assert_eq!(o1.attendees[0].partstat, "TENTATIVE");
        let (series, rid, rule) = series_of(&cal).unwrap();
        assert_eq!(series, "MASTER");
        assert_eq!(rid, Some(DateTime::parse_from_rfc3339("2026-10-20T06:00:00Z").unwrap().timestamp_millis()));
        assert_eq!(rule.as_deref(), Some("FREQ=WEEKLY;BYDAY=TU"));

        // All-day: dates, end exclusive; masters are skipped; single events have no series.
        let day = json!({"id": "d1", "subject": "Holiday", "isAllDay": true, "type": "singleInstance",
            "start": {"dateTime": "2026-12-25T00:00:00.0000000", "timeZone": "UTC"}, "end": {"dateTime": "2026-12-26T00:00:00.0000000", "timeZone": "UTC"}});
        let cal = Component::parse(&to_object(&day, None).unwrap().ics).unwrap();
        let ev = &cal.children[0];
        assert_eq!(ev.prop("DTSTART").unwrap().value, "20261225");
        assert_eq!(ev.prop("DTEND").unwrap().value, "20261226");
        assert!(series_of(&cal).is_none());
        // Made in Riyadh (Graph sends 21:00 UTC the day before) or New York (04:00 UTC): still the 25th.
        for (from, to) in [("2026-12-24T21:00:00", "2026-12-25T21:00:00"), ("2026-12-25T05:00:00", "2026-12-26T05:00:00")] {
            let d = json!({"id": "d2", "isAllDay": true, "start": utc_value(from), "end": utc_value(to)});
            let cal = Component::parse(&to_object(&d, None).unwrap().ics).unwrap();
            assert_eq!((cal.children[0].prop("DTSTART").unwrap().value.as_str(), cal.children[0].prop("DTEND").unwrap().value.as_str()), ("20261225", "20261226"), "{from}");
        }
        let riyadh_midnight = DateTime::parse_from_rfc3339("2026-12-24T21:00:00Z").unwrap().timestamp_millis();
        assert_eq!(start_ms(&json!({"isAllDay": true, "start": utc_value("2026-12-24T21:00:00")}), "Asia/Riyadh"), Some(riyadh_midnight));
        assert!(to_object(&json!({"id": "m", "type": "seriesMaster", "start": day["start"], "end": day["end"]}), None).is_none());
    }

    #[test]
    fn all_day_events_east_of_utc_plus_12_keep_their_date() {
        // Christmas made in Tonga (UTC+13), Kiritimati (UTC+14) and Auckland in summer (UTC+13):
        // Graph's UTC midnights are 11:00 / 10:00 the day before.
        for (zone, from, to) in [
            ("Tonga Standard Time", "2026-12-24T11:00:00", "2026-12-25T11:00:00"),
            ("Line Islands Standard Time", "2026-12-24T10:00:00", "2026-12-25T10:00:00"),
            ("Pacific/Auckland", "2026-12-24T11:00:00", "2026-12-25T11:00:00"),
            ("Arab Standard Time", "2026-12-24T21:00:00", "2026-12-25T21:00:00"),
        ] {
            let d = json!({"id": "d", "isAllDay": true, "originalStartTimeZone": zone, "originalEndTimeZone": zone,
                "start": utc_value(from), "end": utc_value(to)});
            let cal = Component::parse(&to_object(&d, None).unwrap().ics).unwrap();
            let ev = &cal.children[0];
            assert_eq!((ev.prop("DTSTART").unwrap().value.as_str(), ev.prop("DTEND").unwrap().value.as_str()), ("20261225", "20261226"), "{zone}");
            let midnight = |tz: &str| Tz::from_str(tz).unwrap().with_ymd_and_hms(2026, 12, 25, 0, 0, 0).unwrap().timestamp_millis();
            assert_eq!(start_ms(&d, "Asia/Riyadh"), Some(midnight("Asia/Riyadh")), "{zone}");
        }
        // An unknown zone name falls back to the nearest midnight.
        let d = json!({"id": "d", "isAllDay": true, "originalStartTimeZone": "Customized Time Zone",
            "start": utc_value("2026-12-25T05:00:00"), "end": utc_value("2026-12-26T05:00:00")});
        let cal = Component::parse(&to_object(&d, None).unwrap().ics).unwrap();
        assert_eq!(cal.children[0].prop("DTSTART").unwrap().value, "20261225");
    }

    fn utc_value(t: &str) -> Value {
        json!({"dateTime": format!("{t}.0000000"), "timeZone": "UTC"})
    }

    #[test]
    fn recurrence_round_trips() {
        let start = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(); // a Tuesday
        for (rule, pattern_type) in [
            ("FREQ=DAILY", "daily"),
            ("FREQ=WEEKLY;BYDAY=TU", "weekly"),
            ("FREQ=WEEKLY;INTERVAL=2;BYDAY=TU", "weekly"),
            ("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR", "weekly"),
            ("FREQ=MONTHLY;BYMONTHDAY=20", "absoluteMonthly"),
            ("FREQ=MONTHLY;BYDAY=3TU", "relativeMonthly"),
            ("FREQ=MONTHLY;BYDAY=-1FR", "relativeMonthly"),
            ("FREQ=YEARLY;BYMONTH=10;BYMONTHDAY=20", "absoluteYearly"),
            ("FREQ=WEEKLY;BYDAY=TU;COUNT=5", "weekly"),
            ("FREQ=DAILY;UNTIL=20261231", "daily"),
        ] {
            let rec = recurrence_for(rule, start, "Asia/Riyadh").unwrap();
            assert_eq!(rec["pattern"]["type"], pattern_type, "{rule}");
            assert_eq!(rec["range"]["startDate"], "2026-10-20");
            assert_eq!(rec["range"]["recurrenceTimeZone"], "Asia/Riyadh");
            assert_eq!(rrule_from(&rec).as_deref(), Some(rule), "{rule}");
        }
        // The editor's presets without BY parts take the start date's.
        let yearly = recurrence_for("FREQ=YEARLY", start, "UTC").unwrap();
        assert_eq!((yearly["pattern"]["month"].as_u64(), yearly["pattern"]["dayOfMonth"].as_u64()), (Some(10), Some(20)));
        let weekly = recurrence_for("FREQ=WEEKLY", start, "UTC").unwrap();
        assert_eq!(weekly["pattern"]["daysOfWeek"], json!(["tuesday"]));
        // Every weekday written as daily becomes Outlook's weekly pattern.
        let weekdays = recurrence_for("FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR", start, "UTC").unwrap();
        assert_eq!(weekdays["pattern"]["type"], "weekly");
        for bad in ["FREQ=HOURLY", "FREQ=MONTHLY;BYDAY=5TU", "FREQ=DAILY;BYHOUR=9,17", "FREQ=MONTHLY;BYMONTHDAY=-1"] {
            assert!(recurrence_for(bad, start, "UTC").is_err(), "{bad}");
        }
    }

    #[test]
    fn editor_input_becomes_a_graph_event() {
        let b = event_body(&input("2026-10-20T06:30:00Z", "2026-10-20T07:00:00Z", "Asia/Riyadh", false));
        assert_eq!(b["start"], json!({"dateTime": "2026-10-20T09:30:00", "timeZone": "Asia/Riyadh"}));
        assert_eq!(b["end"]["dateTime"], "2026-10-20T10:00:00");
        assert_eq!((b["subject"].as_str(), b["location"]["displayName"].as_str()), (Some("Standup"), Some("Room 1")));
        assert_eq!((b["isReminderOn"].as_bool(), b["reminderMinutesBeforeStart"].as_i64()), (Some(true), Some(10)));
        // All-day: midnights in the user's zone.
        let d = event_body(&input("2026-12-24T21:00:00Z", "2026-12-25T21:00:00Z", "Asia/Riyadh", true));
        assert_eq!((d["start"]["dateTime"].as_str(), d["end"]["dateTime"].as_str()), (Some("2026-12-25T00:00:00"), Some("2026-12-26T00:00:00")));
        // All-day dates come from the user's own zone, even when Graph is given another.
        let lord_howe = event_body(&input("2026-12-24T13:00:00Z", "2026-12-25T13:00:00Z", "Australia/Lord_Howe", true));
        assert_eq!(lord_howe["start"]["dateTime"], "2026-12-25T00:00:00");
        assert_eq!(start_date(&input("2026-12-24T13:00:00Z", "2026-12-25T13:00:00Z", "Australia/Lord_Howe", true)).to_string(), "2026-12-25");
        // A zone Graph doesn't list is replaced by one with the same offsets.
        assert_eq!(graph_zone("Asia/Kuwait", 0).0, "Asia/Riyadh");
        assert_eq!(graph_zone("Europe/Madrid", 1_790_000_000_000).0, "Europe/Berlin");
        assert_eq!(graph_zone("America/Toronto", 1_790_000_000_000).0, "America/New_York");
        assert_eq!(graph_zone("Not/AZone", 0).0, "UTC");
        assert_eq!(graph_zone("Etc/UTC", 0).0, "UTC");
    }

    #[test]
    fn edits_send_only_what_changed() {
        let ev = json!({"id": "x", "subject": "Review", "body": {"content": "Agenda"}, "location": {"displayName": "Room 1"},
            "start": utc_value("2026-10-20T06:30:00"), "end": utc_value("2026-10-20T07:00:00"), "isReminderOn": true, "reminderMinutesBeforeStart": 10});
        let cal = Component::parse(&to_object(&ev, None).unwrap().ics).unwrap();
        let old = event::occurrences(&cal, 0, i64::MAX / 4, Tz::UTC).unwrap().remove(0);
        let mut edited = input("2026-10-20T06:30:00Z", "2026-10-20T07:00:00Z", "UTC", false);
        edited.summary = "Review".into();
        edited.description = "Agenda".into();
        edited.location = "Room 1".into();
        assert_eq!(patch_body(&edited, &edited, &old), json!({}), "nothing changed");
        edited.summary = "Design review".into();
        assert_eq!(patch_body(&edited, &edited, &old), json!({"subject": "Design review"}));
        edited.start_ms += 3_600_000;
        edited.end_ms += 3_600_000;
        let b = patch_body(&edited, &edited, &old);
        assert_eq!((b["start"]["dateTime"].as_str(), b.get("body"), b.get("location")), (Some("2026-10-20T07:30:00"), None, None));
    }

    #[test]
    fn control_characters_are_dropped() {
        let ev = json!({"id": "x\r\nRRULE:FREQ=SECONDLY", "subject": "Hi", "start": utc_value("2026-10-20T06:30:00"), "end": utc_value("2026-10-20T07:00:00"),
            "organizer": {"emailAddress": {"name": "Eve\r\nRRULE:FREQ=SECONDLY", "address": "eve@x.com"}}});
        let ics = to_object(&ev, None).unwrap().ics;
        assert!(!ics.lines().any(|l| l.starts_with("RRULE")), "{ics}");
        let cal = Component::parse(&ics).unwrap();
        assert_eq!(event::occurrences(&cal, 0, i64::MAX / 4, Tz::UTC).unwrap().len(), 1);
    }

    #[test]
    fn sync_window_covers_a_year_back_and_two_ahead() {
        let now = DateTime::parse_from_rfc3339("2026-10-19T15:00:00Z").unwrap().timestamp_millis();
        let (from, to) = window(now);
        assert_eq!(iso_utc(from), "2025-10-19T00:00:00Z");
        assert!(to - now > 729 * DAY_MS);
        assert_eq!(from % DAY_MS, 0);
    }
}
