//! iCal subscriptions end to end against a local feed server: subscribe
//! (webcal, names, colours, passwords), read-only events, conditional
//! refreshes, changes and removals, bad links.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use remail_core::caldav::event::EventInput;
use remail_core::calendar::{local_tz, CalSubscribe};
use remail_core::secrets::{MemoryStore, SecretStore};
use remail_core::store::Store;
use remail_core::*;

struct Quiet;
impl EventSink for Quiet {
    fn emit(&self, _: EngineEvent) {}
}

#[derive(Default)]
struct Feed {
    body: String,
    etag: String,
    /// Full responses (not 304s) served for /team.ics.
    served: usize,
    not_modified: usize,
}

type Shared = Arc<Mutex<Feed>>;

const OCT_2026: i64 = 1_790_812_800_000; // 2026-10-01T00:00:00Z

fn feed(standup: &str, with_holiday: bool) -> String {
    let holiday = if with_holiday { "BEGIN:VEVENT\r\nUID:holiday@x\r\nDTSTART;VALUE=DATE:20261025\r\nSUMMARY:Founders day\r\nEND:VEVENT\r\n" } else { "" };
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nX-WR-CALNAME:Team\r\nX-APPLE-CALENDAR-COLOR:#AF52DE\r\n\
BEGIN:VEVENT\r\nUID:standup@x\r\nDTSTART:20261019T060000Z\r\nDTEND:20261019T061500Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\nSUMMARY:{standup}\r\nEND:VEVENT\r\n\
{holiday}END:VCALENDAR\r\n"
    )
}

async fn team(State(s): State<Shared>, headers: HeaderMap) -> Response {
    let mut f = s.lock().unwrap();
    if headers.get("if-none-match").and_then(|v| v.to_str().ok()) == Some(f.etag.as_str()) {
        f.not_modified += 1;
        return StatusCode::NOT_MODIFIED.into_response();
    }
    f.served += 1;
    ([("content-type", "text/calendar"), ("etag", f.etag.as_str())], f.body.clone()).into_response()
}

async fn private(headers: HeaderMap) -> Response {
    // user:secret
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some("Basic dXNlcjpzZWNyZXQ=") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ([("content-type", "text/calendar")], "BEGIN:VCALENDAR\nVERSION:2.0\nBEGIN:VEVENT\nUID:p@x\nDTSTART:20261020T120000Z\nSUMMARY:Private lunch\nEND:VEVENT\nEND:VCALENDAR\n")
        .into_response()
}

fn titles(e: &Engine) -> Vec<String> {
    e.cal_events(OCT_2026, OCT_2026 + 31 * 86_400_000).unwrap().into_iter().map(|e| e.occurrence.summary).collect()
}

#[tokio::test]
async fn ics_subscriptions() {
    let shared: Shared = Arc::new(Mutex::new(Feed { body: feed("Standup", true), etag: "\"v1\"".into(), ..Default::default() }));
    let app = Router::new()
        .route("/team.ics", get(team))
        .route("/private.ics", get(private))
        .route("/page", get(|| async { ([("content-type", "text/html")], "<!doctype html><html><body>Calendar</body></html>") }))
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://127.0.0.1:{port}");

    let secrets = Arc::new(MemoryStore::default());
    let e = Engine::with_store(Store::open_in_memory().unwrap(), secrets.clone(), Arc::new(Quiet)).unwrap();
    let sub = |url: String| CalSubscribe { url, name: None, username: None, password: None };

    // webcal:// means https; it never silently drops to plain http.
    let insecure = e.cal_subscribe(sub(format!("webcal://127.0.0.1:{port}/team.ics"))).await.unwrap_err();
    assert!(format!("{insecure:#}").contains("starting with http://"), "{insecure:#}");
    assert!(e.cal_accounts().unwrap().is_empty());

    let acc = e.cal_subscribe(sub(format!("{base}/team.ics"))).await.unwrap();
    assert_eq!(acc.kind, "ics");
    assert_eq!(acc.url, format!("{base}/team.ics"));
    assert_eq!(acc.name, "Team", "named after X-WR-CALNAME");
    assert_eq!(acc.calendars.len(), 1);
    let cal = &acc.calendars[0];
    assert!(cal.read_only);
    assert_eq!(cal.color, "#af52de");
    assert_eq!(titles(&e), ["Standup", "Standup", "Standup", "Founders day"]);
    assert_eq!(shared.lock().unwrap().served, 1);

    // Read-only.
    let input = EventInput {
        summary: "Nope".into(),
        location: String::new(),
        description: String::new(),
        start_ms: OCT_2026,
        end_ms: OCT_2026 + 3_600_000,
        all_day: false,
        tz: local_tz().name().to_string(),
        rrule: None,
        alarms_minutes_before: vec![],
    };
    assert!(e.cal_create(&cal.id, input).await.unwrap_err().to_string().contains("read-only"));

    // The same feed again is refused.
    let dup = e.cal_subscribe(sub(format!("{base}/team.ics"))).await.unwrap_err();
    assert!(dup.to_string().contains("already subscribed"), "{dup:#}");

    // Unchanged: a conditional request, answered 304.
    e.cal_sync(Some(&acc.id)).await.unwrap();
    {
        let f = shared.lock().unwrap();
        assert_eq!((f.served, f.not_modified), (1, 1));
    }

    // Changed: a renamed series and a removed event are picked up.
    {
        let mut f = shared.lock().unwrap();
        f.body = feed("Daily sync", false);
        f.etag = "\"v2\"".into();
    }
    e.cal_sync(Some(&acc.id)).await.unwrap();
    assert_eq!(titles(&e), ["Daily sync", "Daily sync", "Daily sync"]);
    assert_eq!(e.cal_search("daily").unwrap().len(), 3);
    let synced = e.cal_accounts().unwrap().into_iter().find(|a| a.id == acc.id).unwrap();
    assert_eq!(synced.status, "ok");

    // Bad links don't leave anything behind.
    let page = e.cal_subscribe(sub(format!("{base}/page"))).await.unwrap_err();
    assert!(page.to_string().contains("web page"), "{page:#}");
    let missing = e.cal_subscribe(sub(format!("{base}/nothing.ics?token=s3cret"))).await.unwrap_err();
    assert!(missing.to_string().contains("HTTP 404"), "{missing:#}");
    // Errors name the host, never the link (it may hold a secret token).
    let unreachable = e.cal_subscribe(sub("http://127.0.0.1:9/cal.ics?token=s3cret".into())).await.unwrap_err();
    assert!(!format!("{unreachable:#}").contains("s3cret"), "{unreachable:#}");
    assert!(e.cal_subscribe(sub("ftp://example.com/a.ics".into())).await.is_err());

    // A feed behind a password.
    let denied = e.cal_subscribe(sub(format!("{base}/private.ics"))).await.unwrap_err();
    assert!(denied.to_string().contains("refused access"), "{denied:#}");
    let private = e
        .cal_subscribe(CalSubscribe {
            url: format!("{base}/private.ics"),
            name: Some("  Partner ".into()),
            username: Some("user".into()),
            password: Some("secret".into()),
        })
        .await
        .unwrap();
    assert_eq!(private.name, "Partner");
    assert_eq!(secrets.get(&format!("cal:{}", private.id)).unwrap().as_deref(), Some("secret"));
    e.cal_sync(Some(&private.id)).await.unwrap();
    assert!(titles(&e).contains(&"Private lunch".to_string()));
    assert_eq!(e.cal_accounts().unwrap().len(), 2);

    // Hiding and removing.
    e.cal_set_calendar(&cal.id, Some(false), None).unwrap();
    assert_eq!(titles(&e), ["Private lunch"]);
    e.cal_remove_account(&private.id).unwrap();
    assert!(secrets.get(&format!("cal:{}", private.id)).unwrap().is_none());
    e.cal_remove_account(&acc.id).unwrap();
    assert!(titles(&e).is_empty());
    assert!(e.cal_accounts().unwrap().is_empty());
}
