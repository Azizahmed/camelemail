use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

use crate::caldav::client::test_access as ta;
use crate::caldav::client::{CalDavClient, DavCredentials, DavError};

const ICLOUD_ROOT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<multistatus xmlns="DAV:"><response><href>/</href><propstat><prop><current-user-principal><href>/1234567890/principal/</href></current-user-principal></prop><status>HTTP/1.1 200 OK</status></propstat><propstat><prop><calendar-home-set xmlns="urn:ietf:params:xml:ns:caldav"/></prop><status>HTTP/1.1 404 Not Found</status></propstat></response></multistatus>"#;

const ICLOUD_PRINCIPAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<multistatus xmlns="DAV:"><response><href>/1234567890/principal/</href><propstat><prop><calendar-home-set xmlns="urn:ietf:params:xml:ns:caldav"><href xmlns="DAV:">https://p42-caldav.icloud.com:443/1234567890/calendars/</href></calendar-home-set></prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#;

const ICLOUD_CALENDARS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<multistatus xmlns="DAV:">
<response><href>/1234567890/calendars/</href><propstat><prop><resourcetype><collection/></resourcetype></prop><status>HTTP/1.1 200 OK</status></propstat></response>
<response><href>/1234567890/calendars/home/</href><propstat><prop>
  <resourcetype><collection/><calendar xmlns="urn:ietf:params:xml:ns:caldav"/></resourcetype>
  <displayname>Home</displayname>
  <calendar-color xmlns="http://apple.com/ns/ical/" symbolic-color="red">#FF2968FF</calendar-color>
  <getctag xmlns="http://calendarserver.org/ns/">HwoQEgwAAA</getctag>
  <sync-token>HwoQEgwAAAh4</sync-token>
  <supported-calendar-component-set xmlns="urn:ietf:params:xml:ns:caldav"><comp name="VEVENT"/></supported-calendar-component-set>
  <current-user-privilege-set><privilege><read/></privilege><privilege><write/></privilege><privilege><read-current-user-privilege-set/></privilege></current-user-privilege-set>
</prop><status>HTTP/1.1 200 OK</status></propstat></response>
<response><href>/1234567890/calendars/shared-ro/</href><propstat><prop>
  <resourcetype><collection/><calendar xmlns="urn:ietf:params:xml:ns:caldav"/><shared xmlns="http://calendarserver.org/ns/"/></resourcetype>
  <displayname>Family (shared)</displayname>
  <calendar-color xmlns="http://apple.com/ns/ical/">#1BADF8</calendar-color>
  <current-user-privilege-set><privilege><read/></privilege><privilege><read-free-busy xmlns="urn:ietf:params:xml:ns:caldav"/></privilege></current-user-privilege-set>
</prop><status>HTTP/1.1 200 OK</status></propstat></response>
<response><href>/1234567890/calendars/tasks/</href><propstat><prop>
  <resourcetype><collection/><calendar xmlns="urn:ietf:params:xml:ns:caldav"/></resourcetype>
  <displayname>Reminders</displayname>
  <supported-calendar-component-set xmlns="urn:ietf:params:xml:ns:caldav"><comp name="VTODO"/></supported-calendar-component-set>
</prop><status>HTTP/1.1 200 OK</status></propstat></response>
<response><href>/1234567890/calendars/inbox/</href><propstat><prop><resourcetype><collection/><schedule-inbox xmlns="urn:ietf:params:xml:ns:caldav"/></resourcetype></prop><status>HTTP/1.1 200 OK</status></propstat></response>
<response><href>/1234567890/calendars/notification/</href><propstat><prop><resourcetype><collection/><notification xmlns="http://calendarserver.org/ns/"/></resourcetype></prop><status>HTTP/1.1 200 OK</status></propstat></response>
</multistatus>"#;

const FASTMAIL_CALENDARS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CS="http://calendarserver.org/ns/" xmlns:A="http://apple.com/ns/ical/">
 <D:response><D:href>/dav/calendars/user/me%40fastmail.com/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
 <D:propstat><D:prop><D:displayname/><A:calendar-color/><CS:getctag/></D:prop><D:status>HTTP/1.1 404 Not Found</D:status></D:propstat></D:response>
 <D:response><D:href>/dav/calendars/user/me%40fastmail.com/2d4d8f5c-fa3b-4b1a-9d3e-1f2a3b4c5d6e/</D:href>
  <D:propstat><D:prop>
   <D:resourcetype><D:collection/><C:calendar/></D:resourcetype>
   <D:displayname>Personal &amp; Work</D:displayname>
   <A:calendar-color>#3a429c</A:calendar-color>
   <CS:getctag>"1708512345"</CS:getctag>
   <D:sync-token>https://caldav.fastmail.com/sync/1708512345</D:sync-token>
   <C:supported-calendar-component-set><C:comp name="VEVENT"/><C:comp name="VTODO"/></C:supported-calendar-component-set>
   <D:current-user-privilege-set><D:privilege><D:all/></D:privilege></D:current-user-privilege-set>
  </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
 <D:response><D:href>/dav/calendars/user/me%40fastmail.com/Default/</D:href>
  <D:propstat><D:prop><D:resourcetype><D:collection/><C:calendar/></D:resourcetype><A:calendar-color>#F00</A:calendar-color>
   <D:current-user-privilege-set><D:privilege><D:read/></D:privilege><D:privilege><D:write-content/></D:privilege></D:current-user-privilege-set></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
  <D:propstat><D:prop><D:displayname/></D:prop><D:status>HTTP/1.1 404 Not Found</D:status></D:propstat></D:response>
</D:multistatus>"#;

const NEXTCLOUD_HOME: &str = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns" xmlns:cal="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:oc="http://owncloud.org/ns" xmlns:nc="http://nextcloud.org/ns" xmlns:x1="http://apple.com/ns/ical/">
 <d:response><d:href>/remote.php/dav/calendars/admin/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>/remote.php/dav/calendars/admin/personal/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/><cal:calendar/></d:resourcetype><d:displayname>Personal</d:displayname><x1:calendar-color>#0082c9</x1:calendar-color><cs:getctag>http://sabre.io/ns/sync/42</cs:getctag><d:sync-token>http://sabre.io/ns/sync/42</d:sync-token>
   <cal:supported-calendar-component-set><cal:comp name="VEVENT"/></cal:supported-calendar-component-set>
   <d:current-user-privilege-set><d:privilege><d:write/></d:privilege><d:privilege><d:read/></d:privilege></d:current-user-privilege-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>/remote.php/dav/calendars/admin/contact_birthdays/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/><cal:calendar/></d:resourcetype><d:displayname>Contact birthdays</d:displayname><x1:calendar-color>#E9D859</x1:calendar-color>
   <d:current-user-privilege-set><d:privilege><d:read/></d:privilege></d:current-user-privilege-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  <d:propstat><d:prop><cal:supported-calendar-component-set/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>
 <d:response><d:href>/remote.php/dav/calendars/admin/trashbin/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><nc:trash-bin/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>/remote.php/dav/calendars/admin/inbox/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><cal:schedule-inbox/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#;

const NEXTCLOUD_ETAGS: &str = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns">
 <d:response><d:href>/remote.php/dav/calendars/admin/personal/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><cal:calendar xmlns:cal="urn:ietf:params:xml:ns:caldav"/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  <d:propstat><d:prop><d:getetag/><d:getcontenttype/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>
 <d:response><d:href>/remote.php/dav/calendars/admin/personal/Meeting%20with%20Bob.ics</d:href><d:propstat><d:prop><d:getetag>&quot;8f2e0c1d&quot;</d:getetag><d:getcontenttype>text/calendar; charset=utf-8; component=vevent</d:getcontenttype><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>personal-relative.ics</d:href><d:propstat><d:prop><d:getetag>"rel"</d:getetag><d:getcontenttype>text/calendar</d:getcontenttype><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>/remote.php/dav/calendars/admin/personal/notes.txt</d:href><d:propstat><d:prop><d:getetag>"x"</d:getetag><d:getcontenttype>text/plain</d:getcontenttype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#;

const MULTIGET: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
<response><href>https://p42-caldav.icloud.com/1234567890/calendars/home/A%40B.ics</href><propstat><prop><getetag>"C=1@U=abc"</getetag><C:calendar-data><![CDATA[BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:a@b
DTSTART:20240101T100000Z
SUMMARY:Hello & <welcome>
END:VEVENT
END:VCALENDAR
]]></C:calendar-data></prop><status>HTTP/1.1 200 OK</status></propstat></response>
<response><href>/1234567890/calendars/home/gone.ics</href><status>HTTP/1.1 404 Not Found</status></response>
</multistatus>"#;

const SYNC: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:">
<d:response><d:href>/cal/new.ics</d:href><d:propstat><d:prop><d:getetag>"e2"</d:getetag><d:getcontenttype>text/calendar</d:getcontenttype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/cal/old.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>
<d:sync-token>http://example.com/sync/7</d:sync-token>
</d:multistatus>"#;

#[test]
fn icloud_principal_on_another_host() {
    let (home, principal) = ta::principal(ICLOUD_ROOT, "https://caldav.icloud.com/").unwrap();
    assert_eq!(home, None, "404 propstat ignored");
    assert_eq!(
        principal.as_deref(),
        Some("https://caldav.icloud.com/1234567890/principal/")
    );
    let (home, _) = ta::principal(
        ICLOUD_PRINCIPAL,
        "https://caldav.icloud.com/1234567890/principal/",
    )
    .unwrap();
    assert_eq!(
        home.as_deref(),
        Some("https://p42-caldav.icloud.com/1234567890/calendars/")
    );
}

#[test]
fn icloud_calendar_list() {
    let cals = ta::calendars(
        ICLOUD_CALENDARS,
        "https://p42-caldav.icloud.com/1234567890/calendars/",
    )
    .unwrap();
    assert_eq!(cals.len(), 3);
    let home = &cals[0];
    assert_eq!(
        home.href,
        "https://p42-caldav.icloud.com/1234567890/calendars/home/"
    );
    assert_eq!(home.name, "Home");
    assert_eq!(home.color.as_deref(), Some("#FF2968"));
    assert_eq!(home.ctag.as_deref(), Some("HwoQEgwAAA"));
    assert_eq!(home.sync_token.as_deref(), Some("HwoQEgwAAAh4"));
    assert!(!home.read_only && home.supports_events);
    assert!(cals[1].read_only);
    assert_eq!(cals[1].color.as_deref(), Some("#1BADF8"));
    assert!(!cals[2].supports_events);
}

#[test]
fn fastmail_calendar_list() {
    let base = "https://caldav.fastmail.com/dav/calendars/user/me@fastmail.com/";
    let cals = ta::calendars(FASTMAIL_CALENDARS, base).unwrap();
    assert_eq!(cals.len(), 2);
    assert_eq!(cals[0].href, "https://caldav.fastmail.com/dav/calendars/user/me@fastmail.com/2d4d8f5c-fa3b-4b1a-9d3e-1f2a3b4c5d6e/");
    assert_eq!(cals[0].name, "Personal & Work");
    assert_eq!(cals[0].color.as_deref(), Some("#3A429C"));
    assert_eq!(cals[0].ctag.as_deref(), Some("\"1708512345\""));
    assert!(!cals[0].read_only && cals[0].supports_events);
    assert_eq!(cals[1].name, "Default", "falls back to the path segment");
    assert_eq!(cals[1].color.as_deref(), Some("#FF0000"));
    assert!(!cals[1].read_only);
}

#[test]
fn nextcloud_relative_hrefs() {
    let base = "https://cloud.example.org/remote.php/dav/calendars/admin/";
    let cals = ta::calendars(NEXTCLOUD_HOME, base).unwrap();
    let names: Vec<&str> = cals.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Personal", "Contact birthdays"]);
    assert_eq!(
        cals[0].href,
        "https://cloud.example.org/remote.php/dav/calendars/admin/personal/"
    );
    assert!(cals[1].read_only && cals[1].supports_events);

    let etags = ta::etags(
        NEXTCLOUD_ETAGS,
        "https://cloud.example.org/remote.php/dav/calendars/admin/personal/",
    )
    .unwrap();
    assert_eq!(
        etags,
        [
            (
                "https://cloud.example.org/remote.php/dav/calendars/admin/personal/Meeting%20with%20Bob.ics".to_string(),
                "\"8f2e0c1d\"".to_string()
            ),
            ("https://cloud.example.org/remote.php/dav/calendars/admin/personal/personal-relative.ics".to_string(), "\"rel\"".to_string()),
        ]
    );
}

#[test]
fn multiget_and_sync_parsing() {
    let objs = ta::objects(
        MULTIGET,
        "https://p42-caldav.icloud.com/1234567890/calendars/home/",
    )
    .unwrap();
    assert_eq!(objs.len(), 1);
    assert_eq!(
        objs[0].0,
        "https://p42-caldav.icloud.com/1234567890/calendars/home/A@B.ics"
    );
    assert_eq!(objs[0].1, "\"C=1@U=abc\"");
    assert!(objs[0].2.contains("SUMMARY:Hello & <welcome>"));

    let changes = ta::sync(SYNC, "http://example.com/cal/").unwrap();
    assert_eq!(
        changes.sync_token.as_deref(),
        Some("http://example.com/sync/7")
    );
    assert_eq!(
        changes.changed,
        [(
            "http://example.com/cal/new.ics".to_string(),
            "\"e2\"".to_string()
        )]
    );
    assert_eq!(changes.deleted, ["http://example.com/cal/old.ics"]);
}

#[test]
fn hostile_bodies_do_not_panic() {
    assert!(ta::calendars("", "https://x.example/").unwrap().is_empty());
    assert!(ta::calendars("   \n", "https://x.example/")
        .unwrap()
        .is_empty());
    assert!(ta::calendars("<html><body>Not DAV", "https://x.example/").is_err());
    assert!(ta::calendars(
        "<multistatus xmlns=\"DAV:\"><response><href></href></response></multistatus>",
        "https://x.example/"
    )
    .unwrap()
    .is_empty());
    let odd = "<multistatus xmlns=\"DAV:\"><response><href>%zz/%</href><propstat><status>garbage</status></propstat></response></multistatus>";
    assert!(ta::etags(odd, "https://x.example/cal/").is_ok());
}

#[test]
fn start_url_guesses() {
    let g = CalDavClient::guess_start_urls;
    assert_eq!(
        g("me@icloud.com", "imap.mail.me.com")[0],
        "https://caldav.icloud.com/"
    );
    assert_eq!(
        g("me@fastmail.com", "imap.fastmail.com")[0],
        "https://caldav.fastmail.com/dav/"
    );
    assert_eq!(
        g("me@custom.org", "imap.fastmail.com")[0],
        "https://caldav.fastmail.com/dav/"
    );
    assert_eq!(
        g("me@zoho.com", "imap.zoho.eu")[0],
        "https://calendar.zoho.eu/caldav/"
    );
    assert_eq!(
        g("me@zohomail.in", "imap.zoho.in")[0],
        "https://calendar.zoho.in/caldav/"
    );
    assert_eq!(
        g("me@yahoo.co.uk", "imap.mail.yahoo.com")[0],
        "https://caldav.calendar.yahoo.com/"
    );
    assert_eq!(
        g("me@aol.com", "imap.aol.com")[0],
        "https://caldav.aol.com/"
    );
    assert_eq!(g("me@gmx.de", "imap.gmx.net")[0], "https://caldav.gmx.net/");
    assert_eq!(
        g("me@gmx.com", "imap.gmx.com")[0],
        "https://caldav.gmx.com/"
    );
    assert_eq!(g("me@web.de", "imap.web.de")[0], "https://caldav.web.de/");
    assert_eq!(
        g("me@mailbox.org", "imap.mailbox.org")[0],
        "https://dav.mailbox.org/"
    );
    assert_eq!(g("me@posteo.de", "posteo.de")[0], "https://posteo.de:8443/");
    assert_eq!(
        g("me@yandex.ru", "imap.yandex.ru")[0],
        "https://caldav.yandex.ru/"
    );
    assert_eq!(
        g("me@example.org", "mail.example.net"),
        [
            "https://example.org/.well-known/caldav",
            "https://example.net/.well-known/caldav"
        ]
    );
    assert_eq!(
        g("me@example.org", "imap.example.org"),
        ["https://example.org/.well-known/caldav"]
    );
    assert!(CalDavClient::new_href("https://x.example/cal/").starts_with("https://x.example/cal/"));
    assert!(CalDavClient::new_href("https://x.example/cal").ends_with(".ics"));
}

#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<String>>>);

async fn principal_host(
    State(seen): State<Seen>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_string();
    seen.0
        .lock()
        .unwrap()
        .push(format!("{method} {} {auth} {}", uri.path(), body.len()));
    if auth != "Basic dXNlcjpwYXNz" {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let xml = match uri.path() {
        "/dav/" => {
            r#"<multistatus xmlns="DAV:"><response><href>/dav/</href><propstat><prop><current-user-principal><href>/dav/principals/user/</href></current-user-principal></prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#
        }
        "/dav/principals/user/" => {
            r#"<multistatus xmlns="DAV:"><response><href>/dav/principals/user/</href><propstat><prop><C:calendar-home-set xmlns:C="urn:ietf:params:xml:ns:caldav"><href>/dav/calendars/user/</href></C:calendar-home-set></prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#
        }
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    (StatusCode::MULTI_STATUS, xml).into_response()
}

async fn serve(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn redirects_keep_the_password_within_the_origin() {
    let seen = Seen::default();
    // One server: its well-known URL redirects to its own DAV root.
    let target = serve(
        axum::Router::new()
            .route(
                "/.well-known/caldav",
                axum::routing::any(|| async {
                    (StatusCode::MOVED_PERMANENTLY, [("location", "/dav/")]).into_response()
                }),
            )
            .fallback(principal_host)
            .with_state(seen.clone()),
    )
    .await;
    // Another host that redirects there.
    let redirect_to = format!("{target}/dav/");
    let front = serve(axum::Router::new().fallback(move || {
        let to = redirect_to.clone();
        async move { (StatusCode::MOVED_PERMANENTLY, [("location", to)]).into_response() }
    }))
    .await;
    let creds = DavCredentials {
        username: "user".into(),
        password: "pass".into(),
        allow_invalid_certs: false,
    };
    let client = CalDavClient::new(creds.clone()).unwrap();

    // Same origin: followed with the credentials, method and body.
    let home = client
        .discover(&format!("{target}/.well-known/caldav"))
        .await
        .unwrap();
    assert_eq!(home, format!("{target}/dav/calendars/user/"));
    let log = std::mem::take(&mut *seen.0.lock().unwrap());
    assert!(
        log[0].starts_with("PROPFIND /dav/ Basic dXNlcjpwYXNz "),
        "{log:?}"
    );
    assert!(!log[0].ends_with(" 0"), "body re-sent after redirect");
    assert!(log[1].starts_with("PROPFIND /dav/principals/user/ "));

    // Another origin never sees the password; the error says where to go instead.
    let err = client
        .discover(&format!("{front}/.well-known/caldav"))
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains(&format!("enter {target}/dav/")), "{err:#}");
    let log = seen.0.lock().unwrap().clone();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(log[0].starts_with("PROPFIND /dav/ - "), "{log:?}");

    let bad = CalDavClient::new(DavCredentials {
        password: "wrong".into(),
        ..creds
    })
    .unwrap();
    let err = bad
        .discover(&format!("{target}/.well-known/caldav"))
        .await
        .unwrap_err();
    assert!(
        matches!(err.downcast_ref::<DavError>(), Some(DavError::Auth)),
        "{err:#}"
    );
}

#[test]
fn redirect_credential_policy() {
    let r = ta::redirect_auth;
    let a = "https://dav.example.com/a/";
    assert!(r(a, a, "https://dav.example.com/b/").unwrap());
    assert!(!r(a, a, "https://evil.example.net/b/").unwrap());
    assert!(!r(a, a, "https://dav.example.com:8443/b/").unwrap());
    // Back to the first origin after a hop elsewhere: the password is sent again.
    assert!(r(a, "https://cdn.example.net/x", "https://dav.example.com/c/").unwrap());
    // Never from https to plain http, even on the same host.
    assert!(r(a, a, "http://dav.example.com/b/").is_err());
    // An http start may move to https on the same host.
    assert!(r("http://dav.example.com/", "http://dav.example.com/", "https://dav.example.com/dav/").unwrap());
    assert!(!r("http://dav.example.com/", "http://dav.example.com/", "https://other.example.com/").unwrap());
}

#[test]
fn deeply_nested_xml_does_not_overflow() {
    let n = 100_000;
    let body = format!(
        "<multistatus xmlns=\"DAV:\"><response><href>/cal/a.ics</href><propstat><prop><getetag>\"1\"</getetag>{}{}</prop></propstat></response></multistatus>",
        "<x>".repeat(n),
        "</x>".repeat(n)
    );
    // Rejected before the XML parser (which recurses per level) sees it.
    assert!(ta::etags(&body, "https://x.example/cal/").is_err());
    assert!(ta::sync(&body, "https://x.example/cal/").is_err());
    // Quotes, comments and CDATA don't hide nesting.
    let sneaky = format!(
        "<multistatus xmlns=\"DAV:\">{}</multistatus>",
        "<x a=\"/>\"><!-- </x> --><![CDATA[</x>]]>".repeat(n)
    );
    assert!(ta::etags(&sneaky, "https://x.example/cal/").is_err());
    // Deep, but within what the parser handles: elements past 64 levels are dropped.
    let deep = format!(
        "<multistatus xmlns=\"DAV:\"><response><href>/cal/a.ics</href><propstat><prop><getetag>\"1\"</getetag>{}{}</prop></propstat></response></multistatus>",
        "<x/><x>".repeat(200),
        "</x>".repeat(200)
    );
    assert_eq!(
        ta::etags(&deep, "https://x.example/cal/").unwrap(),
        [("https://x.example/cal/a.ics".to_string(), "\"1\"".to_string())]
    );
}
