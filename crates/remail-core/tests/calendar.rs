//! Calendar sync end to end against a real CalDAV server (Radicale).
//!
//! REMAIL_CALDAV_IT=http://127.0.0.1:5232/ REMAIL_CALDAV_USER=test REMAIL_CALDAV_PASS=test \
//!   cargo test -p remail-core --test calendar
//! Skipped when REMAIL_CALDAV_IT isn't set.

use std::sync::Arc;

use remail_core::caldav::client::{CalDavClient, DavCredentials};
use remail_core::caldav::event::{EditScope, EventInput};
use remail_core::calendar::{local_tz, CalConnect};
use remail_core::secrets::MemoryStore;
use remail_core::store::Store;
use remail_core::*;

struct Quiet;
impl EventSink for Quiet {
    fn emit(&self, _: EngineEvent) {}
}

fn input(summary: &str, start_ms: i64, hours: i64, rrule: Option<&str>) -> EventInput {
    EventInput {
        summary: summary.into(),
        location: "Room 1".into(),
        description: String::new(),
        start_ms,
        end_ms: start_ms + hours * 3_600_000,
        all_day: false,
        tz: local_tz().name().to_string(),
        rrule: rrule.map(String::from),
        alarms_minutes_before: vec![15],
    }
}

#[tokio::test]
async fn calendar_round_trip() {
    let Ok(base) = std::env::var("REMAIL_CALDAV_IT") else { return };
    let user = std::env::var("REMAIL_CALDAV_USER").unwrap_or_else(|_| "test".into());
    let pass = std::env::var("REMAIL_CALDAV_PASS").unwrap_or_else(|_| "test".into());
    let run = format!("{:x}", store::now_ms());

    // A fresh calendar on the server for this run.
    let dav = CalDavClient::new(DavCredentials { username: user.clone(), password: pass.clone(), allow_invalid_certs: false }).unwrap();
    let home = dav.discover(&base).await.unwrap();
    let cal_href = format!("{}/remail-it-{run}/", home.trim_end_matches('/'));
    dav.make_calendar(&cal_href, &format!("Work {run}"), Some("#34c759")).await.unwrap();

    let e = Engine::with_store(Store::open_in_memory().unwrap(), Arc::new(MemoryStore::default()), Arc::new(Quiet)).unwrap();

    // Wrong password is reported as such.
    let bad = e
        .cal_connect(CalConnect { mail_account_id: None, url: Some(base.clone()), username: Some(user.clone()), password: Some("nope".into()), allow_invalid_certs: false })
        .await
        .unwrap_err();
    assert!(format!("{bad:#}").contains("username or password"), "{bad:#}");

    let acc = e
        .cal_connect(CalConnect { mail_account_id: None, url: Some(base.clone()), username: Some(user), password: Some(pass), allow_invalid_certs: false })
        .await
        .unwrap();
    let cal = acc.calendars.iter().find(|c| c.name == format!("Work {run}")).expect("calendar listed").clone();
    assert_eq!(cal.color, "#34c759");

    // A single event and a weekly series.
    let day = 86_400_000;
    let now = store::now_ms();
    let t0 = now - now % 3_600_000 + day; // tomorrow, on the hour
    e.cal_create(&cal.id, input("Dentist", t0, 1, None)).await.unwrap();
    e.cal_create(&cal.id, input("Weekly review", t0 + 2 * 3_600_000, 1, Some("FREQ=WEEKLY;COUNT=4"))).await.unwrap();
    // Only this run's calendar (the server may hold others).
    let cal_id = cal.id.clone();
    let range = move |e: &Engine| e.cal_events(now, now + 40 * day).unwrap().into_iter().filter(|x| x.calendar_id == cal_id).collect::<Vec<_>>();
    let evs = range(&e);
    assert_eq!(evs.iter().filter(|x| x.occurrence.summary == "Dentist").count(), 1);
    let weekly: Vec<_> = evs.iter().filter(|x| x.occurrence.summary == "Weekly review").collect();
    assert_eq!(weekly.len(), 4);
    assert!(weekly.iter().all(|x| x.occurrence.recurring && x.occurrence.alarms_minutes_before == vec![15]));

    // Change one occurrence, then the whole series.
    let second = weekly[1].clone();
    let rid = second.occurrence.recurrence_id.unwrap();
    let moved = input("Weekly review (moved)", second.occurrence.start_ms + 3_600_000, 1, Some("FREQ=WEEKLY;COUNT=4"));
    e.cal_update(&cal.id, &second.href, moved, EditScope::This { recurrence_id: rid }, None, Some(second.occurrence.start_ms)).await.unwrap();
    let evs = range(&e);
    assert!(evs.iter().any(|x| x.occurrence.summary == "Weekly review (moved)" && x.occurrence.start_ms == second.occurrence.start_ms + 3_600_000));
    assert_eq!(evs.iter().filter(|x| x.occurrence.summary == "Weekly review").count(), 3);

    // Renaming the whole series from its fourth occurrence keeps the earlier ones
    // (the series moves by the occurrence's change, here none).
    let fourth = weekly[3].clone();
    e.cal_update(
        &cal.id,
        &fourth.href,
        input("Team review", fourth.occurrence.start_ms, 1, Some("FREQ=WEEKLY;COUNT=4")),
        EditScope::All,
        None,
        Some(fourth.occurrence.start_ms),
    )
    .await
    .unwrap();
    let renamed: Vec<_> = range(&e).into_iter().filter(|x| x.occurrence.summary == "Team review").collect();
    assert_eq!(renamed.len(), 3);
    assert_eq!(renamed[0].occurrence.start_ms, weekly[0].occurrence.start_ms);
    // Moving it 30 minutes later from that occurrence moves every occurrence.
    e.cal_update(
        &cal.id,
        &fourth.href,
        input("Team review", fourth.occurrence.start_ms + 1_800_000, 1, Some("FREQ=WEEKLY;COUNT=4")),
        EditScope::All,
        None,
        Some(fourth.occurrence.start_ms),
    )
    .await
    .unwrap();
    let moved_all: Vec<_> = range(&e).into_iter().filter(|x| x.occurrence.summary == "Team review").collect();
    assert_eq!(moved_all[0].occurrence.start_ms, weekly[0].occurrence.start_ms + 1_800_000);
    // Moving the series' time discards single-occurrence changes (as Google Calendar does).
    assert_eq!(moved_all.len(), 4);

    // Delete one occurrence.
    let third = moved_all[2].clone();
    e.cal_delete(&cal.id, &third.href, EditScope::This { recurrence_id: third.occurrence.recurrence_id.unwrap() }).await.unwrap();
    assert_eq!(range(&e).iter().filter(|x| x.occurrence.summary == "Team review").count(), 3);

    // A change made on another device arrives with the next sync…
    let dentist = range(&e).into_iter().find(|x| x.occurrence.summary == "Dentist").unwrap();
    let objs = dav.multiget(&cal_href, std::slice::from_ref(&dentist.href)).await.unwrap();
    let remote_ics = objs[0].ics.replace("SUMMARY:Dentist", "SUMMARY:Dentist (rescheduled)");
    dav.put(&dentist.href, &remote_ics, Some(&objs[0].etag), false).await.unwrap();
    // …and an edit based on the old version is refused rather than overwriting it.
    let stale = e.cal_update(&cal.id, &dentist.href, input("Dentist v2", t0, 2, None), EditScope::All, None, None).await.unwrap_err();
    assert!(format!("{stale:#}").contains("changed on another device"), "{stale:#}");
    assert!(range(&e).iter().any(|x| x.occurrence.summary == "Dentist (rescheduled)"));

    // Search, then delete everything.
    assert!(e.cal_search("rescheduled").unwrap().iter().any(|x| x.occurrence.summary == "Dentist (rescheduled)"));
    for ev in range(&e) {
        if range(&e).iter().any(|x| x.href == ev.href) {
            e.cal_delete(&cal.id, &ev.href, EditScope::All).await.unwrap();
        }
    }
    assert!(range(&e).is_empty());
    e.cal_sync(None).await.unwrap();
    assert!(range(&e).is_empty());
    assert!(dav.list_etags(&cal_href).await.unwrap().is_empty());

    // Hiding a calendar hides its events; removing the account clears them.
    e.cal_create(&cal.id, input("Lunch", t0, 1, None)).await.unwrap();
    e.cal_set_calendar(&cal.id, Some(false), None).unwrap();
    assert!(range(&e).is_empty());
    e.cal_set_calendar(&cal.id, Some(true), None).unwrap();
    assert_eq!(range(&e).len(), 1);
    e.cal_remove_account(&acc.id).unwrap();
    assert!(e.cal_accounts().unwrap().is_empty() && range(&e).is_empty());
    // Leave the server as we found it.
    dav.delete(&cal_href, None).await.unwrap();
}

// ----------------------------------------------------------------------------
// A small CalDAV server whose PUT replies carry no ETag (it rewrites what it stores).
// ----------------------------------------------------------------------------

use std::collections::BTreeMap;
use std::sync::Mutex;

use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

#[derive(Default)]
struct Dav {
    /// path → (etag, ics)
    objects: BTreeMap<String, (String, String)>,
    next: u32,
    fail_report: bool,
    log: Vec<String>,
}

type SharedDav = Arc<Mutex<Dav>>;

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn multistatus(responses: &str) -> Response {
    (StatusCode::MULTI_STATUS, [("content-type", "application/xml")], format!(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">{responses}</d:multistatus>"#)).into_response()
}

async fn dav(State(s): State<SharedDav>, method: Method, uri: Uri, headers: HeaderMap, body: String) -> Response {
    let mut d = s.lock().unwrap();
    let path = uri.path().to_string();
    d.log.push(format!("{method} {path}"));
    let header = |h: &str| headers.get(h).and_then(|v| v.to_str().ok()).map(String::from);
    match (method.as_str(), path.as_str()) {
        ("PROPFIND", "/") => multistatus(r#"<d:response><d:href>/</d:href><d:propstat><d:prop><c:calendar-home-set><d:href>/home/</d:href></c:calendar-home-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#),
        ("PROPFIND", "/home/") => multistatus(
            r#"<d:response><d:href>/home/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/home/cal/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>Mock</d:displayname></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#,
        ),
        ("PROPFIND", "/home/cal/") => {
            let mut out = String::from(r#"<d:response><d:href>/home/cal/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#);
            for (p, (etag, _)) in &d.objects {
                out.push_str(&format!(r#"<d:response><d:href>{p}</d:href><d:propstat><d:prop><d:getetag>{}</d:getetag><d:getcontenttype>text/calendar</d:getcontenttype><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#, xml(etag)));
            }
            multistatus(&out)
        }
        ("REPORT", "/home/cal/") if d.fail_report => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        ("REPORT", "/home/cal/") if body.contains("calendar-multiget") => {
            let mut out = String::new();
            for part in body.split("<d:href>").skip(1) {
                let href = part.split("</d:href>").next().unwrap_or_default();
                if let Some((etag, ics)) = d.objects.get(href) {
                    out.push_str(&format!(r#"<d:response><d:href>{href}</d:href><d:propstat><d:prop><d:getetag>{}</d:getetag><c:calendar-data>{}</c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#, xml(etag), xml(ics)));
                }
            }
            multistatus(&out)
        }
        ("PUT", p) => {
            let current = d.objects.get(p).map(|(e, _)| e.clone());
            if header("if-match").is_some_and(|m| Some(m) != current) || (header("if-none-match").is_some() && current.is_some()) {
                return StatusCode::PRECONDITION_FAILED.into_response();
            }
            d.next += 1;
            let etag = format!("\"v{}\"", d.next);
            // Servers may rewrite what they store; then they send no ETag.
            d.objects.insert(p.to_string(), (etag, body.replace("SUMMARY:", "SUMMARY:[server] ")));
            StatusCode::CREATED.into_response()
        }
        ("DELETE", p) => {
            let current = d.objects.get(p).map(|(e, _)| e.clone());
            if header("if-match").is_some_and(|m| Some(m) != current) {
                return StatusCode::PRECONDITION_FAILED.into_response();
            }
            d.objects.remove(p);
            StatusCode::NO_CONTENT.into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[tokio::test]
async fn writes_keep_what_the_server_stored() {
    let state: SharedDav = Arc::default();
    let app = axum::Router::new().fallback(dav).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let e = Engine::with_store(Store::open_in_memory().unwrap(), Arc::new(MemoryStore::default()), Arc::new(Quiet)).unwrap();
    let acc = e
        .cal_connect(CalConnect { mail_account_id: None, url: Some(format!("{base}/")), username: Some("u".into()), password: Some("p".into()), allow_invalid_certs: false })
        .await
        .unwrap();
    let cal = acc.calendars[0].clone();
    let now = store::now_ms();
    let t0 = now - now % 3_600_000 + 86_400_000;
    let titles = |e: &Engine| e.cal_events(now, now + 3 * 86_400_000).unwrap().into_iter().map(|x| x.occurrence.summary).collect::<Vec<_>>();

    // No ETag in the PUT reply: the server's version is what we keep.
    e.cal_create(&cal.id, input("Lunch", t0, 1, None)).await.unwrap();
    assert_eq!(titles(&e), ["[server] Lunch"]);
    // …and later edits are conditional on its etag (no conflict).
    let lunch = e.cal_events(now, now + 3 * 86_400_000).unwrap().remove(0);
    e.cal_update(&cal.id, &lunch.href, input("Lunch 2", t0, 1, None), EditScope::All, None, None).await.unwrap();
    assert_eq!(titles(&e), ["[server] Lunch 2"]);

    // The server's copy couldn't be fetched, so ours is kept without an etag…
    state.lock().unwrap().fail_report = true;
    e.cal_create(&cal.id, input("Dinner", t0 + 3_600_000, 1, None)).await.unwrap();
    state.lock().unwrap().fail_report = false;
    let dinner = e.cal_events(now, now + 3 * 86_400_000).unwrap().into_iter().find(|x| x.occurrence.summary == "Dinner").unwrap();
    // …then another device changes it: an edit based on our copy must not overwrite that.
    {
        let mut d = state.lock().unwrap();
        let path = reqwest::Url::parse(&dinner.href).unwrap().path().to_string();
        let (_, ics) = d.objects[&path].clone();
        d.objects.insert(path, ("\"other\"".into(), ics.replace("[server] Dinner", "Dinner (moved)")));
    }
    let err = e.cal_update(&cal.id, &dinner.href, input("Dinner v2", t0 + 3_600_000, 1, None), EditScope::All, None, None).await.unwrap_err();
    assert!(format!("{err:#}").contains("changed on another device"), "{err:#}");
    assert!(state.lock().unwrap().objects.values().any(|(_, ics)| ics.contains("SUMMARY:Dinner (moved)")));
    assert!(titles(&e).contains(&"Dinner (moved)".to_string()));

    // An invitation to one occurrence (an override without its master): deleting
    // that occurrence deletes the resource rather than writing an empty calendar.
    let rid = chrono::DateTime::from_timestamp_millis(t0 + 5 * 3_600_000).unwrap().format("%Y%m%dT%H%M%SZ").to_string();
    let start = chrono::DateTime::from_timestamp_millis(t0 + 6 * 3_600_000).unwrap().format("%Y%m%dT%H%M%SZ").to_string();
    state.lock().unwrap().objects.insert(
        "/home/cal/inv.ics".into(),
        ("\"inv\"".into(), format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:inv\r\nRECURRENCE-ID:{rid}\r\nDTSTART:{start}\r\nDURATION:PT1H\r\nSUMMARY:Just this one\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n")),
    );
    e.cal_sync(Some(&acc.id)).await.unwrap();
    let inv = e.cal_events(now, now + 3 * 86_400_000).unwrap().into_iter().find(|x| x.occurrence.summary == "Just this one").unwrap();
    e.cal_delete(&cal.id, &inv.href, EditScope::This { recurrence_id: inv.occurrence.recurrence_id.unwrap() }).await.unwrap();
    let d = state.lock().unwrap();
    assert!(!d.objects.contains_key("/home/cal/inv.ics"));
    assert!(d.log.contains(&"DELETE /home/cal/inv.ics".to_string()), "{:?}", d.log);
    assert!(!d.log.contains(&"PUT /home/cal/inv.ics".to_string()));
    drop(d);
    assert!(!titles(&e).contains(&"Just this one".to_string()));
}
