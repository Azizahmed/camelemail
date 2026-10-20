//! Live test against a CalDAV server, e.g. Radicale:
//!
//! ```sh
//! REMAIL_CALDAV_IT=http://127.0.0.1:5232/ REMAIL_CALDAV_USER=test REMAIL_CALDAV_PASS=test cargo test radicale
//! ```
//! Skipped (passes trivially) when `REMAIL_CALDAV_IT` is unset.

use crate::caldav::client::{CalDavClient, DavCredentials, DavError};
use crate::caldav::event::{new_object, occurrences, update_object, EditScope, EventInput};
use crate::caldav::ical::Component;

fn is(err: &anyhow::Error, f: fn(&DavError) -> bool) -> bool {
    err.downcast_ref::<DavError>().is_some_and(f)
}

#[tokio::test]
async fn radicale_round_trip() {
    let Ok(url) = std::env::var("REMAIL_CALDAV_IT") else {
        eprintln!("REMAIL_CALDAV_IT not set; skipping live CalDAV test");
        return;
    };
    let creds = DavCredentials {
        username: std::env::var("REMAIL_CALDAV_USER").unwrap_or_else(|_| "test".into()),
        password: std::env::var("REMAIL_CALDAV_PASS").unwrap_or_else(|_| "test".into()),
        allow_invalid_certs: true,
    };
    let client = CalDavClient::new(creds.clone()).unwrap();
    let start = format!("{}/.well-known/caldav", url.trim_end_matches('/'));
    let home = client.discover(&start).await.unwrap();
    assert!(home.starts_with(url.trim_end_matches('/')), "{home}");

    let cal_href = format!(
        "{}/remail-it-{}/",
        home.trim_end_matches('/'),
        uuid::Uuid::new_v4()
    );
    client
        .make_calendar(&cal_href, "Remail IT", Some("#FF8800"))
        .await
        .unwrap();
    let cals = client.calendars(&home).await.unwrap();
    let cal = cals
        .iter()
        .find(|c| c.href == cal_href)
        .unwrap_or_else(|| panic!("{cal_href} not in {cals:?}"));
    assert_eq!(cal.name, "Remail IT");
    assert_eq!(cal.color.as_deref(), Some("#FF8800"));
    assert!(cal.supports_events && !cal.read_only);
    let token = cal.sync_token.clone();

    let input = EventInput {
        summary: "Planning, Q3; review".into(),
        location: "HQ".into(),
        description: "Line 1\nLine 2".into(),
        start_ms: 1_721_030_400_000, // 2024-07-15T08:00Z
        end_ms: 1_721_034_000_000,
        all_day: false,
        tz: "Europe/Berlin".into(),
        rrule: Some("FREQ=WEEKLY;COUNT=4".into()),
        alarms_minutes_before: vec![15],
    };
    let obj = new_object(&input);
    let href = CalDavClient::new_href(&cal_href);
    let etag1 = client.put(&href, &obj.to_ics(), None, true).await.unwrap();
    let etag1 = match etag1 {
        Some(e) => e,
        None => client.etag_of(&href).await.unwrap().expect("etag"),
    };
    let err = client
        .put(&href, &obj.to_ics(), None, true)
        .await
        .unwrap_err();
    assert!(
        is(&err, |e| matches!(e, DavError::Conflict)),
        "create over existing: {err:#}"
    );

    let listed = client.list_etags(&cal_href).await.unwrap();
    assert_eq!(listed, [(href.clone(), etag1.clone())]);

    let mut got = client
        .multiget(&cal_href, &[href.clone(), format!("{cal_href}missing.ics")])
        .await
        .unwrap();
    assert_eq!(got.len(), 1);
    let fetched = got.remove(0);
    assert_eq!(
        (fetched.href.as_str(), fetched.etag.as_str()),
        (href.as_str(), etag1.as_str())
    );
    let mut parsed = Component::parse(&fetched.ics).unwrap();
    let occ = occurrences(
        &parsed,
        input.start_ms - 1,
        input.start_ms + 40 * 86_400_000,
        chrono_tz::Europe::Berlin,
    )
    .unwrap();
    assert_eq!(occ.len(), 4);
    assert_eq!(occ[0].summary, "Planning, Q3; review");

    let mut edit = input.clone();
    edit.summary = "Planning (moved)".into();
    edit.start_ms += 3_600_000;
    edit.end_ms += 3_600_000;
    update_object(
        &mut parsed,
        &edit,
        EditScope::This {
            recurrence_id: occ[1].recurrence_id.unwrap(),
        },
    )
    .unwrap();
    let etag2 = client
        .put(&href, &parsed.to_ics(), Some(&etag1), false)
        .await
        .unwrap();
    let etag2 = match etag2 {
        Some(e) => e,
        None => client.etag_of(&href).await.unwrap().expect("etag"),
    };
    assert_ne!(etag1, etag2);
    assert_eq!(
        client.etag_of(&href).await.unwrap().as_deref(),
        Some(etag2.as_str())
    );

    let err = client
        .put(&href, &obj.to_ics(), Some(&etag1), false)
        .await
        .unwrap_err();
    assert!(
        is(&err, |e| matches!(e, DavError::Conflict)),
        "stale etag: {err:#}"
    );

    let again = client
        .multiget(&cal_href, std::slice::from_ref(&href))
        .await
        .unwrap();
    let back = Component::parse(&again[0].ics).unwrap();
    let occ = occurrences(
        &back,
        input.start_ms - 1,
        input.start_ms + 40 * 86_400_000,
        chrono_tz::Europe::Berlin,
    )
    .unwrap();
    assert_eq!(
        occ.iter()
            .filter(|o| o.summary == "Planning (moved)")
            .count(),
        1
    );

    assert!(token.is_some(), "server reports a sync-token");
    if let Some(token) = token {
        let changes = client
            .sync_changes(&cal_href, &token)
            .await
            .unwrap()
            .expect("valid token");
        assert!(
            changes.changed.iter().any(|(h, _)| *h == href),
            "{changes:?}"
        );
        assert!(client
            .sync_changes(&cal_href, "http://radicale.org/ns/sync/bogus")
            .await
            .unwrap()
            .is_none());
    }

    let err = client.delete(&href, Some(&etag1)).await.unwrap_err();
    assert!(
        is(&err, |e| matches!(e, DavError::Conflict)),
        "stale delete: {err:#}"
    );
    client.delete(&href, Some(&etag2)).await.unwrap();
    client.delete(&href, None).await.unwrap();
    assert_eq!(client.etag_of(&href).await.unwrap(), None);
    assert!(client.list_etags(&cal_href).await.unwrap().is_empty());

    let bad = CalDavClient::new(DavCredentials {
        password: "definitely-wrong".into(),
        ..creds
    })
    .unwrap();
    let err = bad.calendars(&home).await.unwrap_err();
    assert!(is(&err, |e| matches!(e, DavError::Auth)), "{err:#}");
    client.delete(&cal_href, None).await.unwrap();
}
