//! Outlook calendars end to end, against a mock Microsoft identity platform
//! and a mock Microsoft Graph: connected by themselves when a Microsoft
//! account is added, change-tracked sync, recurring series, creating,
//! editing and deleting events, conflicts, an expired sync round, a missing
//! calendar permission, and disconnecting.

mod common;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use common::*;
use remail_core::caldav::event::{EditScope, EventInput};
use remail_core::calendar::{CalConnect, CalEvent};
use remail_core::secrets::{MemoryStore, SecretStore};
use remail_core::store::Store;
use remail_core::*;
use serde_json::{json, Value};

#[derive(Default)]
struct Graph {
    base: String,
    ver: u64,
    /// calendar → event id → (event, version it last changed at, removed).
    events: HashMap<String, BTreeMap<String, (Value, u64, bool)>>,
    /// Series masters (calendar, id) → master.
    masters: HashMap<String, (String, Value)>,
    /// "METHOD path?query" of every request.
    log: Vec<String>,
    /// Bodies of writes: (method, event id or calendar, body).
    writes: Vec<(String, String, Value)>,
    expire_next_round: bool,
    /// Series lookups fail (e.g. throttled).
    fail_series: bool,
    /// Refuse every `$select` (a service that knows fewer properties than documented).
    refuse_select: bool,
    next_id: u64,
}
type G = Arc<Mutex<Graph>>;

fn ms(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis()
}

fn utc(dt: &str) -> Value {
    json!({"dateTime": format!("{dt}.0000000"), "timeZone": "UTC"})
}

impl Graph {
    fn put(&mut self, cal: &str, mut ev: Value) {
        self.ver += 1;
        ev["@odata.etag"] = json!(format!("W/\"v{}\"", self.ver));
        let id = ev["id"].as_str().unwrap().to_string();
        self.events.entry(cal.to_string()).or_default().insert(id, (ev, self.ver, false));
    }

    fn remove(&mut self, cal: &str, id: &str) {
        self.ver += 1;
        let v = self.ver;
        if let Some(e) = self.events.get_mut(cal).and_then(|m| m.get_mut(id)) {
            e.1 = v;
            e.2 = true;
        }
    }

    fn alive(&self, cal: &str) -> Vec<Value> {
        self.events.get(cal).map(|m| m.values().filter(|e| !e.2).map(|e| e.0.clone()).collect()).unwrap_or_default()
    }

    fn event(&self, cal: &str, id: &str) -> Option<Value> {
        self.events.get(cal)?.get(id).filter(|e| !e.2).map(|e| e.0.clone())
    }
}

fn graph_error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({"error": {"code": code, "message": message}}))).into_response()
}

/// Only calendar tokens, and always UTC times.
fn check(h: &HeaderMap) -> Option<Response> {
    let auth = h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if !auth.starts_with("Bearer gat-") {
        return Some(graph_error(StatusCode::UNAUTHORIZED, "InvalidAuthenticationToken", "Access token is empty or not for Graph."));
    }
    let prefer = h.get("prefer").and_then(|v| v.to_str().ok()).unwrap_or_default();
    assert!(prefer.contains(r#"outlook.timezone="UTC""#) && prefer.contains("ImmutableId"), "{prefer}");
    None
}

/// Properties the live service takes in a calendar `$select` (it refuses
/// `isSharedWithMe` for personal accounts, though the documentation lists it).
const CALENDAR_PROPS: &[&str] = &["id", "name", "color", "hexColor", "canEdit", "canShare", "canViewPrivateItems", "changeKey", "isDefaultCalendar", "isRemovable", "isTallyingResponses", "owner", "allowedOnlineMeetingProviders", "defaultOnlineMeetingProvider"];

async fn calendars(State(g): State<G>, RawQuery(raw): RawQuery, h: HeaderMap) -> Response {
    if let Some(r) = check(&h) {
        return r;
    }
    g.lock().unwrap().log.push(format!("GET calendars?{}", raw.clone().unwrap_or_default()));
    let q: HashMap<String, String> = reqwest::Url::parse(&format!("http://x/?{}", raw.unwrap_or_default())).unwrap().query_pairs().into_owned().collect();
    let refuse_all = g.lock().unwrap().refuse_select;
    if let Some(bad) = q.get("$select").and_then(|sel| sel.split(',').find(|p| refuse_all || !CALENDAR_PROPS.contains(p)).map(String::from)) {
        return graph_error(StatusCode::BAD_REQUEST, "BadRequest", &format!("Could not find a property named '{bad}' on type 'microsoft.graph.calendar'."));
    }
    let owner = |name: &str, address: &str| json!({"name": name, "address": address});
    Json(json!({"value": [
        {"id": "cal-team", "name": "Team", "color": "auto", "hexColor": "#112233", "canEdit": true, "isDefaultCalendar": false, "owner": owner("Nora", "nora@contoso.com")},
        {"id": "cal-main", "name": "Calendar", "color": "lightGreen", "hexColor": "", "canEdit": true, "isDefaultCalendar": true, "owner": owner("Aziz", "Aziz@Outlook.com")},
        {"id": "cal-home", "name": "Home", "color": "lightOrange", "canEdit": true, "owner": owner("Aziz", "aziz@outlook.com")},
    ]}))
    .into_response()
}

async fn delta(State(g): State<G>, Path(cal): Path<String>, RawQuery(raw): RawQuery, h: HeaderMap) -> Response {
    if let Some(r) = check(&h) {
        return r;
    }
    let q: HashMap<String, String> = reqwest::Url::parse(&format!("http://x/?{}", raw.unwrap_or_default())).unwrap().query_pairs().into_owned().collect();
    let mut g = g.lock().unwrap();
    let mut query: Vec<String> = q.iter().map(|(k, v)| format!("{k}={v}")).collect();
    query.sort();
    g.log.push(format!("GET delta {cal}?{}", query.join("&")));
    let link = |g: &Graph, kind: &str, token: String| format!("{}/me/calendars/{cal}/calendarView/delta?{kind}={token}", g.base);
    let page = |g: &Graph, offset: usize| {
        let all = g.alive(&cal);
        let items: Vec<Value> = all.iter().skip(offset).take(2).cloned().collect();
        let mut v = json!({"value": items});
        if offset + 2 < all.len() {
            v["@odata.nextLink"] = json!(link(g, "$skiptoken", format!("full.{}", offset + 2)));
        } else {
            v["@odata.deltaLink"] = json!(link(g, "$deltatoken", g.ver.to_string()));
        }
        Json(v).into_response()
    };
    if q.contains_key("startDateTime") {
        assert!(q.contains_key("endDateTime"));
        return page(&g, 0);
    }
    if let Some(skip) = q.get("$skiptoken") {
        return page(&g, skip.trim_start_matches("full.").parse().unwrap());
    }
    let since: u64 = q.get("$deltatoken").unwrap().parse().unwrap();
    if std::mem::take(&mut g.expire_next_round) {
        return graph_error(StatusCode::GONE, "syncStateNotFound", "The sync state has expired.");
    }
    let changes: Vec<Value> = g
        .events
        .get(&cal)
        .map(|m| {
            m.values()
                .filter(|e| e.1 > since)
                .map(|(ev, _, removed)| if *removed { json!({"id": ev["id"], "@removed": {"reason": "deleted"}}) } else { ev.clone() })
                .collect()
        })
        .unwrap_or_default();
    Json(json!({"value": changes, "@odata.deltaLink": link(&g, "$deltatoken", g.ver.to_string())})).into_response()
}

async fn create(State(g): State<G>, Path(cal): Path<String>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Some(r) = check(&h) {
        return r;
    }
    let mut g = g.lock().unwrap();
    g.log.push(format!("POST {cal}"));
    g.writes.push(("POST".into(), cal.clone(), body.clone()));
    g.next_id += 1;
    let id = format!("new-{}", g.next_id);
    assert_eq!(body["start"]["timeZone"], "UTC", "the test writes in UTC");
    let mut ev = json!({
        "id": id, "type": "singleInstance", "subject": body["subject"], "body": body["body"], "location": body["location"],
        "start": {"dateTime": format!("{}.0000000", body["start"]["dateTime"].as_str().unwrap()), "timeZone": "UTC"},
        "end": {"dateTime": format!("{}.0000000", body["end"]["dateTime"].as_str().unwrap()), "timeZone": "UTC"},
        "isAllDay": body["isAllDay"], "isReminderOn": body["isReminderOn"], "reminderMinutesBeforeStart": body["reminderMinutesBeforeStart"],
    });
    if body.get("recurrence").is_some() {
        // A series: the master, and two occurrences in the window.
        ev["type"] = json!("seriesMaster");
        ev["recurrence"] = body["recurrence"].clone();
        g.masters.insert(id.clone(), (cal.clone(), ev.clone()));
        for (n, day) in [(1, "2026-10-27"), (2, "2026-11-03")] {
            let mut occ = ev.clone();
            occ["id"] = json!(format!("{id}-occ{n}"));
            occ["type"] = json!("occurrence");
            occ["seriesMasterId"] = json!(id);
            occ["start"] = utc(&format!("{day}T09:00:00"));
            occ["end"] = utc(&format!("{day}T09:30:00"));
            occ.as_object_mut().unwrap().remove("recurrence");
            g.put(&cal, occ);
        }
        return (StatusCode::CREATED, Json(ev)).into_response();
    }
    g.put(&cal, ev);
    let ev = g.event(&cal, &id).unwrap();
    (StatusCode::CREATED, Json(ev)).into_response()
}

async fn event_get(State(g): State<G>, Path((cal, id)): Path<(String, String)>, h: HeaderMap) -> Response {
    if let Some(r) = check(&h) {
        return r;
    }
    let mut g = g.lock().unwrap();
    g.log.push(format!("GET event {id}"));
    if g.fail_series {
        let mut r = graph_error(StatusCode::TOO_MANY_REQUESTS, "ApplicationThrottled", "Too many requests.");
        r.headers_mut().insert("retry-after", "0".parse().unwrap());
        return r;
    }
    match g.masters.get(&id).filter(|(c, _)| *c == cal) {
        Some((_, m)) => Json(m.clone()).into_response(),
        None => graph_error(StatusCode::NOT_FOUND, "ErrorItemNotFound", "The specified object was not found in the store."),
    }
}

async fn event_patch(State(g): State<G>, Path((cal, id)): Path<(String, String)>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Some(r) = check(&h) {
        return r;
    }
    let mut g = g.lock().unwrap();
    g.log.push(format!("PATCH {id}"));
    g.writes.push(("PATCH".into(), id.clone(), body.clone()));
    let if_match = h.get("if-match").and_then(|v| v.to_str().ok()).map(String::from);
    if let Some((_, mut master)) = g.masters.get(&id).cloned() {
        if let Some(m) = &if_match {
            assert_eq!(Some(m.as_str()), master["@odata.etag"].as_str(), "series If-Match");
        }
        if let Some(subject) = body.get("subject") {
            master["subject"] = subject.clone();
        }
        master["@odata.etag"] = json!("W/\"master-2\"");
        g.masters.insert(id.clone(), (cal.clone(), master.clone()));
        // Every occurrence of the series changes with it.
        for mut occ in g.alive(&cal).into_iter().filter(|e| e["seriesMasterId"] == json!(id)) {
            occ["subject"] = master["subject"].clone();
            g.put(&cal, occ);
        }
        return Json(master).into_response();
    }
    let Some(mut ev) = g.event(&cal, &id) else {
        return graph_error(StatusCode::NOT_FOUND, "ErrorItemNotFound", "The specified object was not found in the store.");
    };
    if if_match.as_deref().is_some_and(|m| Some(m) != ev["@odata.etag"].as_str()) {
        return graph_error(StatusCode::PRECONDITION_FAILED, "ErrorIrresolvableConflict", "The send or update operation could not be performed because the change key passed in the request does not match the current change key for the item.");
    }
    if let Some(subject) = body.get("subject") {
        ev["subject"] = subject.clone();
    }
    if let (Some(start), Some(end)) = (body["start"]["dateTime"].as_str(), body["end"]["dateTime"].as_str()) {
        ev["start"] = json!({"dateTime": format!("{start}.0000000"), "timeZone": "UTC"});
        ev["end"] = json!({"dateTime": format!("{end}.0000000"), "timeZone": "UTC"});
    }
    if ev["type"] == "occurrence" {
        ev["type"] = json!("exception");
    }
    g.put(&cal, ev);
    Json(g.event(&cal, &id).unwrap()).into_response()
}

async fn event_delete(State(g): State<G>, Path((cal, id)): Path<(String, String)>, h: HeaderMap) -> Response {
    if let Some(r) = check(&h) {
        return r;
    }
    let mut g = g.lock().unwrap();
    g.log.push(format!("DELETE {id}"));
    if g.masters.remove(&id).is_some() {
        let occs: Vec<String> = g.alive(&cal).into_iter().filter(|e| e["seriesMasterId"] == json!(id)).map(|e| e["id"].as_str().unwrap().to_string()).collect();
        for o in occs {
            g.remove(&cal, &o);
        }
    } else {
        g.remove(&cal, &id);
    }
    StatusCode::NO_CONTENT.into_response()
}

fn input(summary: &str, start: &str, end: &str) -> EventInput {
    EventInput {
        summary: summary.into(),
        location: String::new(),
        description: String::new(),
        start_ms: ms(start),
        end_ms: ms(end),
        all_day: false,
        tz: "UTC".into(),
        rrule: None,
        alarms_minutes_before: vec![10],
    }
}

fn events(engine: &Engine) -> Vec<CalEvent> {
    engine.cal_events(ms("2026-10-01T00:00:00Z"), ms("2027-01-31T00:00:00Z")).unwrap()
}

fn find<'a>(list: &'a [CalEvent], summary: &str) -> &'a CalEvent {
    list.iter().find(|e| e.occurrence.summary == summary).unwrap_or_else(|| panic!("no {summary} in {:?}", list.iter().map(|e| &e.occurrence.summary).collect::<Vec<_>>()))
}

#[tokio::test(flavor = "multi_thread")]
async fn outlook_calendars() {
    let idp: Shared = Arc::new(Mutex::new(Idp::default()));
    idp.lock().unwrap().calendar_ok.insert("rt-1".into());
    let graph: G = Arc::new(Mutex::new(Graph::default()));
    let app = Router::new()
        .route("/common/oauth2/v2.0/token", post(token))
        .with_state(idp.clone())
        .merge(
            Router::new()
                .route("/v1.0/me/calendars", get(calendars))
                .route("/v1.0/me/calendars/{cal}/calendarView/delta", get(delta))
                .route("/v1.0/me/calendars/{cal}/events", post(create))
                .route("/v1.0/me/calendars/{cal}/events/{id}", get(event_get).patch(event_patch).delete(event_delete))
                .with_state(graph.clone()),
        );
    let port = serve(app).await;
    std::env::set_var("REMAIL_MS_AUTHORITY", format!("http://127.0.0.1:{port}/common"));
    std::env::set_var("REMAIL_MS_GRAPH", format!("http://127.0.0.1:{port}/v1.0"));
    {
        let mut g = graph.lock().unwrap();
        g.base = format!("http://127.0.0.1:{port}/v1.0");
        g.put("cal-main", json!({"id": "dentist", "type": "singleInstance", "subject": "Dentist", "start": utc("2026-10-22T10:00:00"), "end": utc("2026-10-22T11:00:00"), "location": {"displayName": "Clinic"}, "isReminderOn": true, "reminderMinutesBeforeStart": 30}));
        g.put("cal-main", json!({"id": "holiday", "type": "singleInstance", "subject": "Holiday", "isAllDay": true, "start": utc("2026-12-25T00:00:00"), "end": utc("2026-12-26T00:00:00")}));
        let recurrence = json!({"pattern": {"type": "weekly", "interval": 1, "daysOfWeek": ["tuesday"], "firstDayOfWeek": "sunday"}, "range": {"type": "noEnd", "startDate": "2026-10-20"}});
        g.masters.insert("M1".into(), ("cal-main".into(), json!({"id": "M1", "@odata.etag": "W/\"master-1\"", "type": "seriesMaster", "subject": "Standup", "start": utc("2026-10-20T06:00:00"), "end": utc("2026-10-20T06:15:00"), "recurrence": recurrence})));
        for (n, day) in [(1, "2026-10-20"), (2, "2026-10-27"), (3, "2026-11-03")] {
            g.put("cal-main", json!({"id": format!("M1-{n}"), "type": "occurrence", "seriesMasterId": "M1", "subject": "Standup", "originalStart": format!("{day}T06:00:00Z"), "start": utc(&format!("{day}T06:00:00")), "end": utc(&format!("{day}T06:15:00"))}));
        }
        g.put("cal-main", json!({"id": "planning", "type": "singleInstance", "subject": "Planning", "start": utc("2026-10-23T08:00:00"), "end": utc("2026-10-23T09:00:00"),
            "attendees": [{"emailAddress": {"name": "Sara", "address": "sara@contoso.com"}, "status": {"response": "accepted"}}]}));
        g.put("cal-team", json!({"id": "offsite", "type": "singleInstance", "subject": "Team offsite", "start": utc("2026-11-10T07:00:00"), "end": utc("2026-11-10T15:00:00")}));
    }

    let accepted = Arc::new(Mutex::new(vec!["at-1".to_string()]));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let imap = ServerConfig { host: "127.0.0.1".into(), port: fake_imap(accepted.clone(), seen.clone()).await, security: Security::None };
    let smtp = ServerConfig { host: "127.0.0.1".into(), port: fake_smtp(accepted.clone()).await, security: Security::None };
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemoryStore::default());
    let engine = Engine::with_store(Store::open(&dir.path().join("remail.db")).unwrap(), secrets.clone(), Arc::new(Quiet)).unwrap();
    engine.set_org(microsoft_org());

    // Adding the Microsoft account connects its calendars by itself.
    let req = OAuthAccountRequest { imap: Some(imap.clone()), smtp: Some(smtp.clone()), ..Default::default() };
    let added = engine.add_microsoft_account(browser(idp.clone(), "code-1"), req).await.unwrap();
    let mail_id = added.account.id.clone();
    let mut outlook = None;
    for _ in 0..100 {
        outlook = engine.cal_accounts().unwrap().into_iter().find(|a| a.kind == "graph" && a.last_sync.is_some());
        if outlook.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let outlook = outlook.expect("the Outlook calendars connect after sign-in");
    assert_eq!((outlook.mail_account_id.as_deref(), outlook.name.as_str(), outlook.status.as_str()), (Some(mail_id.as_str()), "aziz@outlook.com", "ok"));
    let names: Vec<(&str, bool, &str)> = outlook.calendars.iter().map(|c| (c.name.as_str(), c.read_only, c.color.as_str())).collect();
    assert_eq!(names, vec![("Calendar", false, "#22c55e"), ("Team", true, "#112233"), ("Home", false, "#f97316")], "main calendar first; shared ones are read-only");
    let reqs = idp.lock().unwrap().token_requests.clone();
    assert!(reqs.iter().any(|r| r.0 == "refresh_token" && r.1.contains("Calendars.ReadWrite") && !r.1.contains("IMAP")), "{reqs:?}");

    // Everything in the window, recurring occurrences tied to their series.
    let list = events(&engine);
    assert_eq!(list.len(), 7, "{:?}", list.iter().map(|e| &e.occurrence.summary).collect::<Vec<_>>());
    let dentist = find(&list, "Dentist");
    assert_eq!((dentist.occurrence.location.as_str(), dentist.occurrence.alarms_minutes_before.clone(), dentist.occurrence.recurring), ("Clinic", vec![30], false));
    let standups: Vec<&CalEvent> = list.iter().filter(|e| e.occurrence.summary == "Standup").collect();
    assert_eq!(standups.len(), 3);
    assert!(standups.iter().all(|e| e.occurrence.recurring && e.occurrence.rrule.as_deref() == Some("FREQ=WEEKLY;BYDAY=TU")));
    assert_eq!(standups[1].occurrence.recurrence_id, Some(ms("2026-10-27T06:00:00Z")));
    assert!(find(&list, "Holiday").occurrence.all_day);
    assert!(find(&list, "Team offsite").read_only);
    let main_id = outlook.calendars[0].id.clone();
    let team_id = outlook.calendars[1].id.clone();
    let home_id = outlook.calendars[2].id.clone();
    let auth = idp.lock().unwrap().authorize_scopes.clone();
    assert!(auth[0].contains(CALENDAR_SCOPE) && auth[0].contains("IMAP.AccessAsUser.All"), "one sign-in asks for mail and calendar: {auth:?}");

    // A service that refuses the property list still syncs (everything is asked for instead).
    graph.lock().unwrap().refuse_select = true;
    engine.cal_sync(Some(&outlook.id)).await.unwrap();
    graph.lock().unwrap().refuse_select = false;
    let asked = graph.lock().unwrap().log.iter().filter(|l| l.starts_with("GET calendars")).cloned().collect::<Vec<_>>();
    assert!(asked.iter().rev().take(2).any(|l| !l.contains("select")), "{asked:?}");
    assert_eq!(engine.cal_accounts().unwrap().into_iter().find(|a| a.id == outlook.id).unwrap().calendars.len(), 3);

    // Later syncs fetch only what changed.
    {
        let mut g = graph.lock().unwrap();
        let mut d = g.event("cal-main", "dentist").unwrap();
        d["subject"] = json!("Dentist (moved)");
        g.put("cal-main", d);
        g.remove("cal-main", "holiday");
        g.log.clear();
    }
    engine.cal_sync(Some(&outlook.id)).await.unwrap();
    let list = events(&engine);
    assert!(list.iter().any(|e| e.occurrence.summary == "Dentist (moved)") && !list.iter().any(|e| e.occurrence.summary == "Holiday"));
    let log = graph.lock().unwrap().log.clone();
    assert!(log.iter().all(|l| !l.contains("startDateTime")), "no full download: {log:?}");
    assert!(log.iter().filter(|l| l.contains("$deltatoken")).count() == 3, "{log:?}");

    // Create: in the user's calendar right away.
    engine.cal_create(&main_id, input("Lunch", "2026-10-25T09:00:00Z", "2026-10-25T10:00:00Z")).await.unwrap();
    let lunch = find(&events(&engine), "Lunch").clone();
    let (method, cal, body) = graph.lock().unwrap().writes.last().cloned().unwrap();
    assert_eq!((method.as_str(), cal.as_str()), ("POST", "cal-main"));
    assert_eq!((body["start"]["dateTime"].as_str(), body["reminderMinutesBeforeStart"].as_i64(), body["isAllDay"].as_bool()), (Some("2026-10-25T09:00:00"), Some(10), Some(false)));
    assert!(body.get("recurrence").is_none());

    // Edit a single event: conditional on its version.
    engine.cal_update(&main_id, &lunch.href, input("Lunch with Sara", "2026-10-25T09:30:00Z", "2026-10-25T10:30:00Z"), EditScope::All, None, Some(lunch.occurrence.start_ms)).await.unwrap();
    let lunch = find(&events(&engine), "Lunch with Sara").clone();
    assert_eq!(lunch.occurrence.start_ms, ms("2026-10-25T09:30:00Z"));
    let (_, _, body) = graph.lock().unwrap().writes.last().cloned().unwrap();
    assert!(body.get("subject").is_some() && body.get("start").is_some(), "{body}");
    assert!(body.get("body").is_none() && body.get("location").is_none() && body.get("isReminderOn").is_none(), "only what changed: {body}");

    // Someone changed it in Outlook meanwhile: refused, and the latest version is loaded.
    {
        let mut g = graph.lock().unwrap();
        let mut e = g.event("cal-main", &lunch.href).unwrap();
        e["subject"] = json!("Lunch (changed in Outlook)");
        g.put("cal-main", e);
    }
    let conflict = engine.cal_update(&main_id, &lunch.href, input("Mine", "2026-10-25T09:30:00Z", "2026-10-25T10:30:00Z"), EditScope::All, None, None).await.unwrap_err().to_string();
    assert!(conflict.contains("changed on another device"), "{conflict}");
    find(&events(&engine), "Lunch (changed in Outlook)");

    // One occurrence of the series.
    let second = events(&engine).into_iter().find(|e| e.occurrence.summary == "Standup" && e.occurrence.start_ms == ms("2026-10-27T06:00:00Z")).unwrap();
    let mut moved = input("Standup (late)", "2026-10-27T07:00:00Z", "2026-10-27T07:15:00Z");
    moved.rrule = second.occurrence.rrule.clone();
    engine
        .cal_update(&main_id, &second.href, moved, EditScope::This { recurrence_id: second.occurrence.recurrence_id.unwrap() }, None, Some(second.occurrence.start_ms))
        .await
        .unwrap();
    assert_eq!(graph.lock().unwrap().writes.last().unwrap().1, second.href, "only that occurrence");
    let late = find(&events(&engine), "Standup (late)").clone();
    assert!(late.occurrence.recurring && late.occurrence.start_ms == ms("2026-10-27T07:00:00Z"));

    // The whole series, edited from one occurrence: the series' start moves by the same amount, the rule stays.
    let third = events(&engine).into_iter().find(|e| e.occurrence.summary == "Standup" && e.occurrence.start_ms == ms("2026-11-03T06:00:00Z")).unwrap();
    let mut all = input("Daily sync", "2026-11-03T06:30:00Z", "2026-11-03T06:45:00Z");
    all.rrule = third.occurrence.rrule.clone();
    engine.cal_update(&main_id, &third.href, all, EditScope::All, None, Some(third.occurrence.start_ms)).await.unwrap();
    let (method, id, body) = graph.lock().unwrap().writes.last().cloned().unwrap();
    assert_eq!((method.as_str(), id.as_str()), ("PATCH", "M1"));
    assert_eq!(body["start"]["dateTime"], "2026-10-20T06:30:00", "the series' first occurrence, moved 30 minutes");
    assert!(body.get("recurrence").is_none(), "unchanged rule isn't rewritten: {body}");
    assert_eq!(events(&engine).iter().filter(|e| e.occurrence.summary == "Daily sync").count(), 3);

    // Moving to another calendar: a simple event moves; repeating ones and meetings stay put.
    let dentist = find(&events(&engine), "Dentist (moved)").clone();
    engine.cal_update(&main_id, &dentist.href, input("Dentist (moved)", "2026-10-22T10:00:00Z", "2026-10-22T11:00:00Z"), EditScope::All, Some(home_id.clone()), None).await.unwrap();
    let moved = find(&events(&engine), "Dentist (moved)").clone();
    assert_eq!(moved.calendar_id, home_id);
    assert!(graph.lock().unwrap().log.contains(&"DELETE dentist".to_string()));
    let daily = find(&events(&engine), "Daily sync").clone();
    let refused = engine.cal_update(&main_id, &daily.href, input("Daily sync", "2026-10-20T06:30:00Z", "2026-10-20T06:45:00Z"), EditScope::All, Some(home_id.clone()), None).await.unwrap_err().to_string();
    assert!(refused.contains("Repeating events"), "{refused}");
    let planning = find(&events(&engine), "Planning").clone();
    let refused = engine.cal_update(&main_id, &planning.href, input("Planning", "2026-10-23T08:00:00Z", "2026-10-23T09:00:00Z"), EditScope::All, Some(home_id.clone()), None).await.unwrap_err().to_string();
    assert!(refused.contains("cancel it for everyone"), "{refused}");

    // A series lookup that fails (throttled) keeps the rule known before.
    {
        let mut g = graph.lock().unwrap();
        g.fail_series = true;
        let mut occ = g.event("cal-main", "M1-1").unwrap();
        occ["subject"] = json!("Standup (renamed)");
        g.put("cal-main", occ);
    }
    engine.cal_sync(Some(&outlook.id)).await.unwrap();
    graph.lock().unwrap().fail_series = false;
    let renamed = find(&events(&engine), "Standup (renamed)").clone();
    assert_eq!(renamed.occurrence.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=TU"));

    // Delete one event, then the whole series.
    engine.cal_delete(&main_id, &lunch.href, EditScope::All).await.unwrap();
    assert!(!events(&engine).iter().any(|e| e.href == lunch.href));
    let any = events(&engine).into_iter().find(|e| e.occurrence.summary == "Daily sync").unwrap();
    engine.cal_delete(&main_id, &any.href, EditScope::All).await.unwrap();
    assert!(graph.lock().unwrap().log.contains(&"DELETE M1".to_string()));
    assert!(!events(&engine).iter().any(|e| e.occurrence.recurring), "the series is gone");

    // A new repeating event: Outlook gets the pattern, the occurrences come back from it.
    let mut weekly = input("Gym", "2026-10-27T09:00:00Z", "2026-10-27T09:30:00Z");
    weekly.rrule = Some("FREQ=WEEKLY;BYDAY=TU".into());
    engine.cal_create(&main_id, weekly).await.unwrap();
    let body = graph.lock().unwrap().writes.last().unwrap().2.clone();
    assert_eq!(body["recurrence"]["pattern"], json!({"type": "weekly", "interval": 1, "daysOfWeek": ["tuesday"], "firstDayOfWeek": "monday"}));
    assert_eq!(body["recurrence"]["range"]["startDate"], "2026-10-27");
    let gyms: Vec<CalEvent> = events(&engine).into_iter().filter(|e| e.occurrence.summary == "Gym").collect();
    assert_eq!(gyms.len(), 2);
    assert!(gyms.iter().all(|g| g.occurrence.recurring && g.occurrence.rrule.as_deref() == Some("FREQ=WEEKLY;BYDAY=TU")));

    // Calendars shared with the user are read-only here.
    let ro = engine.cal_create(&team_id, input("Nope", "2026-10-25T09:00:00Z", "2026-10-25T10:00:00Z")).await.unwrap_err().to_string();
    assert!(ro.contains("read-only"), "{ro}");

    // An expired sync round starts the window over.
    graph.lock().unwrap().expire_next_round = true;
    graph.lock().unwrap().log.clear();
    engine.cal_sync(Some(&outlook.id)).await.unwrap();
    assert!(graph.lock().unwrap().log.iter().any(|l| l.contains("startDateTime")));
    assert_eq!(events(&engine).iter().filter(|e| e.occurrence.summary == "Gym").count(), 2);

    // Disconnected calendars stay disconnected, until connected again.
    engine.cal_remove_account(&outlook.id).unwrap();
    engine.cal_sync(None).await.unwrap();
    assert!(engine.cal_accounts().unwrap().iter().all(|a| a.kind != "graph"));
    let back = engine
        .cal_connect(CalConnect { mail_account_id: Some(mail_id.clone()), url: None, username: None, password: None, allow_invalid_certs: false })
        .await
        .unwrap();
    assert_eq!((back.kind.as_str(), back.calendars.len()), ("graph", 3));

    // An account signed in before Remail asked for the calendar: mail works, the calendar asks to sign in again.
    let older = NewAccount {
        email: "old@hotmail.com".into(),
        display_name: "Old".into(),
        username: "old@hotmail.com".into(),
        password: String::new(),
        imap: imap.clone(),
        smtp: smtp.clone(),
        allow_invalid_certs: false,
    };
    let older = engine.store().insert_account(&older, false, "microsoft").unwrap();
    secrets.set(&older.id, r#"{"provider":"microsoft","clientId":"client-123","refreshToken":"rt-old"}"#).unwrap();
    let _ = engine.cal_sync(None).await;
    let cal = engine.cal_accounts().unwrap().into_iter().find(|a| a.mail_account_id.as_deref() == Some(older.id.as_str())).expect("connected by itself");
    assert_eq!(cal.status, "signin", "only signing in again helps");
    assert!(cal.last_error.as_deref().unwrap_or_default().contains("Sign in to Microsoft again"), "{:?}", cal.last_error);
    assert_eq!(engine.accounts().unwrap().into_iter().find(|a| a.id == older.id).unwrap().status, AccountStatus::Active, "mail isn't marked as signed out");
    // Signing in again allows it.
    idp.lock().unwrap().calendar_ok.insert("rt-1".into());
    engine.reauthorize_account(&older.id, browser_as(idp.clone(), "code-2", "old@hotmail.com", "oid-old")).await.unwrap();
    let mut ok = false;
    for _ in 0..100 {
        if engine.cal_accounts().unwrap().iter().any(|a| a.id == cal.id && a.status == "ok") {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ok, "{:?}", engine.cal_accounts().unwrap().into_iter().find(|a| a.id == cal.id));

    // Someone who doesn't allow the calendar still gets their mail: the sign-in is asked again for mail alone.
    let before = idp.lock().unwrap().authorize_scopes.len();
    let req = OAuthAccountRequest { imap: Some(imap.clone()), smtp: Some(smtp.clone()), ..Default::default() };
    let work = engine.add_microsoft_account(browser_declining_calendar(idp.clone(), "code-3", "nora@contoso.com", "oid-nora"), req).await.unwrap();
    let asked: Vec<String> = idp.lock().unwrap().authorize_scopes[before..].to_vec();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked[0].contains(CALENDAR_SCOPE) && !asked[1].contains(CALENDAR_SCOPE), "{asked:?}");
    let mut status = String::new();
    for _ in 0..100 {
        if let Some(a) = engine.cal_accounts().unwrap().into_iter().find(|a| a.mail_account_id.as_deref() == Some(work.account.id.as_str()) && a.last_error.is_some()) {
            status = a.status;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(status, "signin", "its calendar waits for permission");
    assert_ne!(engine.accounts().unwrap().into_iter().find(|a| a.id == work.account.id).unwrap().status, AccountStatus::AuthError, "mail stays signed in");

    // Removing the mail account removes its calendars.
    engine.remove_account(&mail_id).unwrap();
    assert!(engine.cal_accounts().unwrap().iter().all(|a| a.mail_account_id.as_deref() != Some(mail_id.as_str())));
}
