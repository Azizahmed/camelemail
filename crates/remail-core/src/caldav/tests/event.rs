use chrono::DateTime;
use chrono_tz::Tz;

use crate::caldav::event::*;
use crate::caldav::ical::Component;
use crate::caldav::recur::Recur;
use crate::caldav::tests::ical::APPLE_ICS;

const BERLIN: Tz = chrono_tz::Europe::Berlin;
const RIYADH: Tz = chrono_tz::Asia::Riyadh;

fn ms(s: &str) -> i64 {
    DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis()
}

fn iso(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn wrap(events: &str) -> Component {
    let ics = format!("BEGIN:VCALENDAR\nVERSION:2.0\nPRODID:test\n{events}\nEND:VCALENDAR\n");
    Component::parse(&ics).unwrap()
}

fn starts(cal: &Component, from: &str, to: &str, tz: Tz) -> Vec<String> {
    occurrences(cal, ms(from), ms(to), tz)
        .unwrap()
        .into_iter()
        .map(|o| iso(o.start_ms))
        .collect()
}

#[test]
fn weekly_with_exdate_and_dst_berlin() {
    let mut ics = APPLE_ICS.replace(
        "RRULE:FREQ=WEEKLY;BYDAY=MO\r\n",
        "RRULE:FREQ=WEEKLY;BYDAY=MO\r\nEXDATE;TZID=Europe/Berlin:20240122T090000\r\n",
    );
    let cal = Component::parse(&ics).unwrap();
    assert_eq!(
        starts(&cal, "2024-01-15T00:00:00Z", "2024-02-05T00:00:00Z", RIYADH),
        ["2024-01-15T08:00:00Z", "2024-01-29T08:00:00Z"]
    );
    // 09:00 local before and after the last-Sunday-of-March switch.
    assert_eq!(
        starts(&cal, "2024-03-25T00:00:00Z", "2024-04-02T00:00:00Z", RIYADH),
        ["2024-03-25T08:00:00Z", "2024-04-01T07:00:00Z"]
    );
    let occ = occurrences(
        &cal,
        ms("2024-04-01T00:00:00Z"),
        ms("2024-04-02T00:00:00Z"),
        RIYADH,
    )
    .unwrap();
    let o = &occ[0];
    assert_eq!(o.end_ms - o.start_ms, 3_600_000);
    assert!(o.recurring);
    assert_eq!(o.recurrence_id, Some(o.start_ms));
    assert_eq!(o.summary, "Weekly sync, planning ; review");
    assert_eq!(o.organizer.as_deref(), Some("jane@example.com"));
    assert_eq!(o.attendees[0].email, "bob@example.com");
    assert_eq!(o.attendees[0].name, "Bob");
    assert_eq!(o.attendees[0].partstat, "ACCEPTED");
    assert_eq!(o.alarms_minutes_before, [15]);
    assert_eq!(o.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO"));

    // Same object with an unrecognised TZID falls back to the VTIMEZONE rules.
    ics = ics.replace("Europe/Berlin", "Custom Berlin");
    let cal = Component::parse(&ics).unwrap();
    assert_eq!(
        starts(&cal, "2024-03-25T00:00:00Z", "2024-04-02T00:00:00Z", RIYADH),
        ["2024-03-25T08:00:00Z", "2024-04-01T07:00:00Z"]
    );
    assert_eq!(
        starts(&cal, "2024-10-21T00:00:00Z", "2024-11-01T00:00:00Z", RIYADH),
        ["2024-10-21T07:00:00Z", "2024-10-28T08:00:00Z"]
    );
}

#[test]
fn dst_new_york() {
    let cal = wrap(
        "BEGIN:VEVENT\nUID:ny\nDTSTART;TZID=America/New_York:20241021T090000\nDTEND;TZID=America/New_York:20241021T093000\nRRULE:FREQ=WEEKLY\nEND:VEVENT",
    );
    assert_eq!(
        starts(&cal, "2024-10-21T00:00:00Z", "2024-11-12T00:00:00Z", BERLIN),
        [
            "2024-10-21T13:00:00Z",
            "2024-10-28T13:00:00Z",
            "2024-11-04T14:00:00Z",
            "2024-11-11T14:00:00Z"
        ]
    );
    // Spring: 2025-03-09.
    assert_eq!(
        starts(&cal, "2025-03-03T00:00:00Z", "2025-03-11T00:00:00Z", BERLIN),
        ["2025-03-03T14:00:00Z", "2025-03-10T13:00:00Z"]
    );
}

#[test]
fn overrides_move_in_and_out_of_range() {
    let cal = wrap(
        "BEGIN:VEVENT\nUID:w\nDTSTART;TZID=Europe/Berlin:20240101T090000\nDTEND;TZID=Europe/Berlin:20240101T100000\nRRULE:FREQ=WEEKLY\nSUMMARY:Standup\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:w\nRECURRENCE-ID;TZID=Europe/Berlin:20240108T090000\nDTSTART;TZID=Europe/Berlin:20240117T150000\nDTEND;TZID=Europe/Berlin:20240117T160000\nSUMMARY:Moved standup\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:w\nRECURRENCE-ID;TZID=Europe/Berlin:20240122T090000\nDTSTART;TZID=Europe/Berlin:20240122T090000\nDTEND;TZID=Europe/Berlin:20240122T100000\nSTATUS:CANCELLED\nEND:VEVENT",
    );
    // Moved out: nothing on Jan 8.
    assert!(starts(&cal, "2024-01-08T00:00:00Z", "2024-01-09T00:00:00Z", RIYADH).is_empty());
    // Moved in: the Jan 17 override appears next to the regular Jan 15 instance.
    let occ = occurrences(
        &cal,
        ms("2024-01-15T00:00:00Z"),
        ms("2024-01-21T00:00:00Z"),
        RIYADH,
    )
    .unwrap();
    assert_eq!(occ.len(), 2);
    assert_eq!(occ[0].summary, "Standup");
    assert_eq!(occ[1].summary, "Moved standup");
    assert_eq!(iso(occ[1].start_ms), "2024-01-17T14:00:00Z");
    assert_eq!(occ[1].recurrence_id, Some(ms("2024-01-08T08:00:00Z")));
    assert_eq!(occ[1].rrule.as_deref(), Some("FREQ=WEEKLY"));
    // Cancelled instance dropped.
    assert_eq!(
        starts(&cal, "2024-01-21T00:00:00Z", "2024-01-30T00:00:00Z", RIYADH),
        ["2024-01-29T08:00:00Z"]
    );
}

#[test]
fn count_until_and_rdate() {
    let cal = wrap("BEGIN:VEVENT\nUID:c\nDTSTART:20240301T100000Z\nDURATION:PT1H30M\nRRULE:FREQ=DAILY;COUNT=5\nRDATE:20240320T100000Z\nEND:VEVENT");
    let occ = occurrences(
        &cal,
        ms("2024-01-01T00:00:00Z"),
        ms("2025-01-01T00:00:00Z"),
        BERLIN,
    )
    .unwrap();
    assert_eq!(occ.len(), 6);
    assert_eq!(iso(occ[4].start_ms), "2024-03-05T10:00:00Z");
    assert_eq!(iso(occ[5].start_ms), "2024-03-20T10:00:00Z");
    assert_eq!(occ[0].end_ms - occ[0].start_ms, 90 * 60_000);

    let cal = wrap("BEGIN:VEVENT\nUID:u\nDTSTART;TZID=Europe/Berlin:20240115T090000\nRRULE:FREQ=WEEKLY;UNTIL=20240205T080000Z\nEND:VEVENT");
    let occ = occurrences(
        &cal,
        ms("2024-01-01T00:00:00Z"),
        ms("2025-01-01T00:00:00Z"),
        BERLIN,
    )
    .unwrap();
    assert_eq!(occ.len(), 4);
    assert_eq!(
        occ[0].end_ms, occ[0].start_ms,
        "DATE-TIME without DTEND has zero length"
    );

    let cal = wrap("BEGIN:VEVENT\nUID:d\nDTSTART;VALUE=DATE:20240101\nRRULE:FREQ=DAILY;UNTIL=20240110\nEND:VEVENT");
    let occ = occurrences(
        &cal,
        ms("2023-12-01T00:00:00Z"),
        ms("2024-02-01T00:00:00Z"),
        BERLIN,
    )
    .unwrap();
    assert_eq!(occ.len(), 10);
    assert!(occ
        .iter()
        .all(|o| o.all_day && o.end_ms - o.start_ms == 86_400_000));
}

#[test]
fn monthly_and_yearly_rules() {
    let last_friday = wrap("BEGIN:VEVENT\nUID:f\nDTSTART:20240126T120000Z\nRRULE:FREQ=MONTHLY;BYDAY=-1FR;COUNT=4\nEND:VEVENT");
    assert_eq!(
        starts(
            &last_friday,
            "2024-01-01T00:00:00Z",
            "2025-01-01T00:00:00Z",
            BERLIN
        ),
        [
            "2024-01-26T12:00:00Z",
            "2024-02-23T12:00:00Z",
            "2024-03-29T12:00:00Z",
            "2024-04-26T12:00:00Z"
        ]
    );
    let second_tuesday = wrap("BEGIN:VEVENT\nUID:t\nDTSTART:20240109T120000Z\nRRULE:FREQ=MONTHLY;INTERVAL=2;BYDAY=2TU\nEND:VEVENT");
    assert_eq!(
        starts(
            &second_tuesday,
            "2024-01-01T00:00:00Z",
            "2024-07-01T00:00:00Z",
            BERLIN
        ),
        [
            "2024-01-09T12:00:00Z",
            "2024-03-12T12:00:00Z",
            "2024-05-14T12:00:00Z"
        ]
    );
    let last_workday = wrap("BEGIN:VEVENT\nUID:l\nDTSTART:20240131T120000Z\nRRULE:FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1\nEND:VEVENT");
    assert_eq!(
        starts(
            &last_workday,
            "2024-01-01T00:00:00Z",
            "2024-05-01T00:00:00Z",
            BERLIN
        ),
        [
            "2024-01-31T12:00:00Z",
            "2024-02-29T12:00:00Z",
            "2024-03-29T12:00:00Z",
            "2024-04-30T12:00:00Z"
        ]
    );
    let month_end = wrap("BEGIN:VEVENT\nUID:m\nDTSTART:20240131T120000Z\nRRULE:FREQ=MONTHLY;BYMONTHDAY=-1;COUNT=3\nEND:VEVENT");
    assert_eq!(
        starts(
            &month_end,
            "2024-01-01T00:00:00Z",
            "2025-01-01T00:00:00Z",
            BERLIN
        ),
        [
            "2024-01-31T12:00:00Z",
            "2024-02-29T12:00:00Z",
            "2024-03-31T12:00:00Z"
        ]
    );
    // Monthly on the 31st skips short months.
    let the_31st = wrap(
        "BEGIN:VEVENT\nUID:31\nDTSTART:20240131T120000Z\nRRULE:FREQ=MONTHLY;COUNT=3\nEND:VEVENT",
    );
    assert_eq!(
        starts(
            &the_31st,
            "2024-01-01T00:00:00Z",
            "2025-01-01T00:00:00Z",
            BERLIN
        ),
        [
            "2024-01-31T12:00:00Z",
            "2024-03-31T12:00:00Z",
            "2024-05-31T12:00:00Z"
        ]
    );
    let birthday = wrap("BEGIN:VEVENT\nUID:b\nDTSTART;VALUE=DATE:20200315\nRRULE:FREQ=YEARLY\nSUMMARY:Birthday\nEND:VEVENT");
    let occ = occurrences(
        &birthday,
        ms("2024-01-01T00:00:00Z"),
        ms("2025-01-01T00:00:00Z"),
        RIYADH,
    )
    .unwrap();
    assert_eq!(occ.len(), 1);
    assert_eq!(iso(occ[0].start_ms), "2024-03-14T21:00:00Z");
    let leap =
        wrap("BEGIN:VEVENT\nUID:leap\nDTSTART;VALUE=DATE:20200229\nRRULE:FREQ=YEARLY\nEND:VEVENT");
    assert_eq!(
        occurrences(
            &leap,
            ms("2021-01-01T00:00:00Z"),
            ms("2029-01-01T00:00:00Z"),
            BERLIN
        )
        .unwrap()
        .len(),
        2
    );
    let thanksgiving = wrap("BEGIN:VEVENT\nUID:tg\nDTSTART;VALUE=DATE:20221124\nRRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=4TH\nEND:VEVENT");
    assert_eq!(
        starts(
            &thanksgiving,
            "2024-01-01T00:00:00Z",
            "2025-01-01T00:00:00Z",
            chrono_tz::UTC
        ),
        ["2024-11-28T00:00:00Z"]
    );
    let weekno = wrap("BEGIN:VEVENT\nUID:wn\nDTSTART:19970512T090000Z\nRRULE:FREQ=YEARLY;BYWEEKNO=20;BYDAY=MO\nEND:VEVENT");
    assert_eq!(
        starts(
            &weekno,
            "1998-01-01T00:00:00Z",
            "2000-01-01T00:00:00Z",
            chrono_tz::UTC
        ),
        ["1998-05-11T09:00:00Z", "1999-05-17T09:00:00Z"]
    );
}

#[test]
fn all_day_floating_windows_and_duration() {
    let multi = wrap("BEGIN:VEVENT\nUID:a\nDTSTART;VALUE=DATE:20240310\nDTEND;VALUE=DATE:20240313\nSUMMARY:Trip\nEND:VEVENT");
    let occ = occurrences(
        &multi,
        ms("2024-03-11T00:00:00Z"),
        ms("2024-03-12T00:00:00Z"),
        RIYADH,
    )
    .unwrap();
    assert_eq!(occ.len(), 1);
    assert!(occ[0].all_day);
    assert!(!occ[0].recurring);
    assert_eq!(occ[0].recurrence_id, None);
    assert_eq!(iso(occ[0].start_ms), "2024-03-09T21:00:00Z");
    assert_eq!(iso(occ[0].end_ms), "2024-03-12T21:00:00Z");

    let one_day = wrap("BEGIN:VEVENT\nUID:o\nDTSTART;VALUE=DATE:20240310\nEND:VEVENT");
    let occ = occurrences(
        &one_day,
        ms("2024-03-01T00:00:00Z"),
        ms("2024-04-01T00:00:00Z"),
        BERLIN,
    )
    .unwrap();
    assert_eq!(occ[0].end_ms - occ[0].start_ms, 86_400_000);

    let floating =
        wrap("BEGIN:VEVENT\nUID:fl\nDTSTART:20240310T090000\nDTEND:20240310T100000\nEND:VEVENT");
    assert_eq!(
        starts(
            &floating,
            "2024-03-10T00:00:00Z",
            "2024-03-11T00:00:00Z",
            RIYADH
        ),
        ["2024-03-10T06:00:00Z"]
    );
    assert_eq!(
        starts(
            &floating,
            "2024-03-10T00:00:00Z",
            "2024-03-11T00:00:00Z",
            BERLIN
        ),
        ["2024-03-10T08:00:00Z"]
    );

    for (tzid, expect) in [
        ("\"W. Europe Standard Time\"", "2024-07-10T07:00:00Z"),
        ("Arab Standard Time", "2024-07-10T06:00:00Z"),
        ("Arabian Standard Time", "2024-07-10T05:00:00Z"),
        ("Pacific Standard Time", "2024-07-10T16:00:00Z"),
        ("GMT Standard Time", "2024-07-10T08:00:00Z"),
        ("India Standard Time", "2024-07-10T03:30:00Z"),
        (
            "/mozilla.org/20050126_1/Europe/Berlin",
            "2024-07-10T07:00:00Z",
        ),
        (
            "/freeassociation.sourceforge.net/Tzfile/America/New_York",
            "2024-07-10T13:00:00Z",
        ),
        (
            "\"(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna\"",
            "2024-07-10T07:00:00Z",
        ),
    ] {
        let cal = wrap(&format!(
            "BEGIN:VEVENT\nUID:w\nDTSTART;TZID={tzid}:20240710T090000\nEND:VEVENT"
        ));
        assert_eq!(
            starts(
                &cal,
                "2024-07-09T00:00:00Z",
                "2024-07-11T00:00:00Z",
                chrono_tz::UTC
            ),
            [expect],
            "{tzid}"
        );
    }
    // Unknown TZID without VTIMEZONE: local zone.
    let unknown =
        wrap("BEGIN:VEVENT\nUID:x\nDTSTART;TZID=Mars/Olympus:20240710T090000\nEND:VEVENT");
    assert_eq!(
        starts(
            &unknown,
            "2024-07-09T00:00:00Z",
            "2024-07-11T00:00:00Z",
            RIYADH
        ),
        ["2024-07-10T06:00:00Z"]
    );

    let nominal = wrap(
        "BEGIN:VEVENT\nUID:n\nDTSTART;TZID=Europe/Berlin:20240330T120000\nDURATION:P1D\nEND:VEVENT",
    );
    let occ = occurrences(
        &nominal,
        ms("2024-03-30T00:00:00Z"),
        ms("2024-04-01T00:00:00Z"),
        BERLIN,
    )
    .unwrap();
    assert_eq!(
        occ[0].end_ms - occ[0].start_ms,
        23 * 3_600_000,
        "P1D across spring-forward is 23h"
    );

    // Non-existent local time (in the gap) uses the offset before the gap.
    let gap = wrap("BEGIN:VEVENT\nUID:g\nDTSTART;TZID=Europe/Berlin:20240331T023000\nEND:VEVENT");
    assert_eq!(
        starts(&gap, "2024-03-31T00:00:00Z", "2024-04-01T00:00:00Z", BERLIN),
        ["2024-03-31T01:30:00Z"]
    );
}

#[test]
fn alarms_and_status() {
    let cal = wrap(
        "BEGIN:VEVENT\nUID:al\nDTSTART:20240310T090000Z\nDTEND:20240310T100000Z\nSTATUS:tentative\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-PT15M\nEND:VALARM\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER;RELATED=END:PT0S\nEND:VALARM\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER;VALUE=DATE-TIME:20240310T080000Z\nEND:VALARM\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER:-P1D\nEND:VALARM\nEND:VEVENT",
    );
    let occ = occurrences(
        &cal,
        ms("2024-03-10T00:00:00Z"),
        ms("2024-03-11T00:00:00Z"),
        BERLIN,
    )
    .unwrap();
    assert_eq!(occ[0].alarms_minutes_before, [15, -60, 60, 1440]);
    assert_eq!(occ[0].status, "TENTATIVE");
    let cancelled =
        wrap("BEGIN:VEVENT\nUID:x\nDTSTART:20240310T090000Z\nSTATUS:CANCELLED\nEND:VEVENT");
    assert!(occurrences(
        &cancelled,
        ms("2024-03-10T00:00:00Z"),
        ms("2024-03-11T00:00:00Z"),
        BERLIN
    )
    .unwrap()
    .is_empty());
}

#[test]
fn far_ranges_and_cap() {
    let daily = wrap("BEGIN:VEVENT\nUID:d\nDTSTART;TZID=Europe/Berlin:20000101T080000\nDTEND;TZID=Europe/Berlin:20000101T083000\nRRULE:FREQ=DAILY\nEND:VEVENT");
    assert_eq!(
        occurrences(
            &daily,
            ms("2026-10-19T00:00:00Z"),
            ms("2026-10-26T00:00:00Z"),
            BERLIN
        )
        .unwrap()
        .len(),
        7
    );
    assert_eq!(
        occurrences(
            &daily,
            ms("2000-01-01T00:00:00Z"),
            ms("2030-01-01T00:00:00Z"),
            BERLIN
        )
        .unwrap()
        .len(),
        MAX_INSTANCES
    );
    assert_eq!(
        object_span(&daily, BERLIN),
        Some((ms("2000-01-01T07:00:00Z"), None))
    );
    let counted = wrap("BEGIN:VEVENT\nUID:c\nDTSTART:20240101T100000Z\nDTEND:20240101T110000Z\nRRULE:FREQ=WEEKLY;COUNT=3\nEND:VEVENT");
    assert_eq!(
        object_span(&counted, BERLIN),
        Some((ms("2024-01-01T10:00:00Z"), Some(ms("2024-01-15T11:00:00Z"))))
    );
    let hourly = wrap("BEGIN:VEVENT\nUID:h\nDTSTART:20240101T000000Z\nRRULE:FREQ=HOURLY;INTERVAL=6;COUNT=4\nEND:VEVENT");
    assert_eq!(
        starts(
            &hourly,
            "2024-01-01T00:00:00Z",
            "2024-01-02T00:00:00Z",
            BERLIN
        ),
        [
            "2024-01-01T00:00:00Z",
            "2024-01-01T06:00:00Z",
            "2024-01-01T12:00:00Z",
            "2024-01-01T18:00:00Z"
        ]
    );
    assert_eq!(object_uid(&hourly).as_deref(), Some("h"));
    assert!(Recur::parse("BYDAY=MO").is_err());
}

fn input(summary: &str, start: &str, end: &str, rrule: Option<&str>) -> EventInput {
    EventInput {
        summary: summary.into(),
        location: "Room 1".into(),
        description: "Line one\nLine two, with comma".into(),
        start_ms: ms(start),
        end_ms: ms(end),
        all_day: false,
        tz: "Europe/Berlin".into(),
        rrule: rrule.map(String::from),
        alarms_minutes_before: vec![10],
    }
}

fn reparse(cal: &Component) -> Component {
    Component::parse(&cal.to_ics()).unwrap()
}

#[test]
fn new_object_forms() {
    let single = new_object(&input(
        "Dentist",
        "2024-07-10T07:00:00Z",
        "2024-07-10T08:00:00Z",
        None,
    ));
    let ev = &single.children[0];
    assert_eq!(
        single.prop("PRODID").unwrap().value,
        "-//Remail//Remail Calendar//EN"
    );
    assert!(ev.prop("UID").unwrap().value.ends_with("@remail"));
    assert_eq!(ev.prop("DTSTART").unwrap().value, "20240710T070000Z");
    assert_eq!(ev.prop("SEQUENCE").unwrap().value, "0");
    for p in ["DTSTAMP", "CREATED", "LAST-MODIFIED"] {
        assert!(ev.prop(p).is_some(), "{p}");
    }
    let occ = occurrences(
        &reparse(&single),
        ms("2024-07-10T00:00:00Z"),
        ms("2024-07-11T00:00:00Z"),
        BERLIN,
    )
    .unwrap();
    assert_eq!(occ.len(), 1);
    assert_eq!(occ[0].description, "Line one\nLine two, with comma");
    assert_eq!(occ[0].location, "Room 1");
    assert_eq!(occ[0].alarms_minutes_before, [10]);

    let weekly = new_object(&input(
        "Gym",
        "2024-03-18T17:00:00Z",
        "2024-03-18T18:00:00Z",
        Some("FREQ=WEEKLY;BYDAY=MO"),
    ));
    let vtz = weekly
        .children_named("VTIMEZONE")
        .next()
        .expect("VTIMEZONE");
    assert_eq!(vtz.prop("TZID").unwrap().value, "Europe/Berlin");
    let daylight = vtz.children_named("DAYLIGHT").next().expect("DAYLIGHT");
    assert_eq!(
        daylight.prop("RRULE").unwrap().value,
        "FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU"
    );
    let ev = weekly.children_named("VEVENT").next().unwrap();
    assert_eq!(
        ev.prop("DTSTART").unwrap().param("TZID"),
        Some("Europe/Berlin")
    );
    assert_eq!(ev.prop("DTSTART").unwrap().value, "20240318T180000");
    let w = reparse(&weekly);
    assert_eq!(
        starts(&w, "2024-03-25T00:00:00Z", "2024-04-02T00:00:00Z", BERLIN),
        ["2024-03-25T17:00:00Z", "2024-04-01T16:00:00Z"]
    );
    // The generated VTIMEZONE alone (renamed TZID) gives the same answer.
    let renamed =
        Component::parse(&w.to_ics().replace("Europe/Berlin", "Remail Test Zone")).unwrap();
    let renamed = Component::parse(
        &renamed
            .to_ics()
            .replace("X-LIC-LOCATION:Remail Test Zone\r\n", ""),
    )
    .unwrap();
    assert_eq!(
        starts(
            &renamed,
            "2024-03-25T00:00:00Z",
            "2024-04-02T00:00:00Z",
            RIYADH
        ),
        ["2024-03-25T17:00:00Z", "2024-04-01T16:00:00Z"]
    );

    let mut all_day = input(
        "Holiday",
        "2024-03-09T21:00:00Z",
        "2024-03-11T21:00:00Z",
        None,
    );
    all_day.all_day = true;
    all_day.tz = "Asia/Riyadh".into();
    let cal = new_object(&all_day);
    let ev = &cal.children[0];
    assert_eq!(ev.prop("DTSTART").unwrap().value, "20240310");
    assert_eq!(ev.prop("DTSTART").unwrap().param("VALUE"), Some("DATE"));
    assert_eq!(ev.prop("DTEND").unwrap().value, "20240312");
    let occ = occurrences(
        &reparse(&cal),
        ms("2024-03-01T00:00:00Z"),
        ms("2024-04-01T00:00:00Z"),
        RIYADH,
    )
    .unwrap();
    assert_eq!(
        (occ[0].start_ms, occ[0].end_ms),
        (all_day.start_ms, all_day.end_ms)
    );
    assert!(occ[0].all_day);
}

#[test]
fn update_all_and_this() {
    let mut cal = Component::parse(APPLE_ICS).unwrap();
    // Override for Jan 22 and an EXDATE on Jan 29, both on the original 09:00 grid.
    let master = cal.children_named("VEVENT").next().unwrap().clone();
    let first_rid = ms("2024-01-22T08:00:00Z");
    let mut this = input(
        "Moved",
        "2024-01-22T12:00:00Z",
        "2024-01-22T13:00:00Z",
        None,
    );
    update_object(
        &mut cal,
        &this,
        EditScope::This {
            recurrence_id: first_rid,
        },
    )
    .unwrap();
    delete_occurrence_in(&mut cal, ms("2024-01-29T08:00:00Z"), RIYADH).unwrap();
    let cal2 = reparse(&cal);
    let occ = occurrences(
        &cal2,
        ms("2024-01-15T00:00:00Z"),
        ms("2024-02-06T00:00:00Z"),
        RIYADH,
    )
    .unwrap();
    let got: Vec<(String, String)> = occ
        .iter()
        .map(|o| (iso(o.start_ms), o.summary.clone()))
        .collect();
    assert_eq!(
        got,
        [
            (
                "2024-01-15T08:00:00Z".to_string(),
                "Weekly sync, planning ; review".to_string()
            ),
            ("2024-01-22T12:00:00Z".to_string(), "Moved".to_string()),
            (
                "2024-02-05T08:00:00Z".to_string(),
                "Weekly sync, planning ; review".to_string()
            ),
        ]
    );
    assert_eq!(occ[1].recurrence_id, Some(first_rid));
    let ov = cal2
        .children_named("VEVENT")
        .find(|e| e.prop("RECURRENCE-ID").is_some())
        .unwrap();
    let rid = ov.prop("RECURRENCE-ID").unwrap();
    assert_eq!(
        (rid.param("TZID"), rid.value.as_str()),
        (Some("Europe/Berlin"), "20240122T090000")
    );
    assert!(ov.prop("RRULE").is_none());
    assert_eq!(ov.prop("SEQUENCE").unwrap().value, "2");
    assert!(ov.prop("ATTENDEE").is_some(), "override keeps attendees");

    // Editing the same instance again replaces the override instead of adding one.
    this.summary = "Moved again".into();
    update_object(
        &mut cal,
        &this,
        EditScope::This {
            recurrence_id: first_rid,
        },
    )
    .unwrap();
    assert_eq!(
        cal.children_named("VEVENT")
            .filter(|e| e.prop("RECURRENCE-ID").is_some())
            .count(),
        1
    );

    // Editing All without moving the start keeps the override and EXDATE.
    let mut all = input(
        "Renamed",
        "2024-01-15T08:00:00Z",
        "2024-01-15T09:00:00Z",
        Some("FREQ=WEEKLY;BYDAY=MO"),
    );
    all.alarms_minutes_before = vec![15];
    update_object(&mut cal, &all, EditScope::All).unwrap();
    let m = cal
        .children_named("VEVENT")
        .find(|e| e.prop("RECURRENCE-ID").is_none())
        .unwrap();
    assert_eq!(m.prop("SEQUENCE").unwrap().value, "3");
    assert_eq!(
        m.prop("DTSTART").unwrap().param("TZID"),
        Some("Europe/Berlin")
    );
    assert!(
        m.prop("X-APPLE-TRAVEL-ADVISORY-BEHAVIOR").is_some(),
        "unknown props survive"
    );
    assert_eq!(
        m.children[0].prop("X-APPLE-DEFAULT-ALARM").unwrap().value,
        "TRUE",
        "unchanged alarm kept"
    );
    assert!(m.prop("EXDATE").is_some());
    let occ = occurrences(
        &reparse(&cal),
        ms("2024-01-15T00:00:00Z"),
        ms("2024-02-06T00:00:00Z"),
        RIYADH,
    )
    .unwrap();
    let names: Vec<&str> = occ.iter().map(|o| o.summary.as_str()).collect();
    assert_eq!(names, ["Renamed", "Moved again", "Renamed"]);

    // Moving the series by an hour drops the now-stale override and EXDATE.
    let moved = input(
        "Renamed",
        "2024-01-15T09:00:00Z",
        "2024-01-15T10:00:00Z",
        Some("FREQ=WEEKLY;BYDAY=MO"),
    );
    update_object(&mut cal, &moved, EditScope::All).unwrap();
    assert_eq!(cal.children_named("VEVENT").count(), 1);
    let m = cal.children_named("VEVENT").next().unwrap();
    assert!(m.prop("EXDATE").is_none());
    assert_eq!(m.children_named("VALARM").count(), 1);
    assert_eq!(
        starts(
            &reparse(&cal),
            "2024-01-15T00:00:00Z",
            "2024-02-06T00:00:00Z",
            RIYADH
        ),
        [
            "2024-01-15T09:00:00Z",
            "2024-01-22T09:00:00Z",
            "2024-01-29T09:00:00Z",
            "2024-02-05T09:00:00Z"
        ]
    );

    // Dropping the rule makes it a single event.
    let single = input("Once", "2024-01-15T09:00:00Z", "2024-01-15T10:00:00Z", None);
    update_object(&mut cal, &single, EditScope::All).unwrap();
    let m = cal.children_named("VEVENT").next().unwrap();
    assert!(m.prop("RRULE").is_none());
    assert_eq!(
        m.prop("UID").unwrap().value,
        master.prop("UID").unwrap().value
    );
    assert_eq!(
        starts(
            &reparse(&cal),
            "2024-01-01T00:00:00Z",
            "2024-03-01T00:00:00Z",
            RIYADH
        ),
        ["2024-01-15T09:00:00Z"]
    );
}

#[test]
fn delete_occurrences() {
    let weekly = new_object(&input(
        "Gym",
        "2024-03-18T17:00:00Z",
        "2024-03-18T18:00:00Z",
        Some("FREQ=WEEKLY"),
    ));
    let mut cal = reparse(&weekly);
    update_object(
        &mut cal,
        &input(
            "Gym (late)",
            "2024-04-01T19:00:00Z",
            "2024-04-01T20:00:00Z",
            None,
        ),
        EditScope::This {
            recurrence_id: ms("2024-04-01T16:00:00Z"),
        },
    )
    .unwrap();
    delete_occurrence(&mut cal, ms("2024-04-01T16:00:00Z")).unwrap();
    assert_eq!(cal.children_named("VEVENT").count(), 1, "override dropped");
    let m = cal.children_named("VEVENT").next().unwrap();
    let ex = m.prop("EXDATE").unwrap();
    assert_eq!(
        (ex.param("TZID"), ex.value.as_str()),
        (Some("Europe/Berlin"), "20240401T180000")
    );
    assert_eq!(
        starts(
            &reparse(&cal),
            "2024-03-18T00:00:00Z",
            "2024-04-09T00:00:00Z",
            BERLIN
        ),
        [
            "2024-03-18T17:00:00Z",
            "2024-03-25T17:00:00Z",
            "2024-04-08T16:00:00Z"
        ]
    );

    // All-day series: the zone is inferred from the instance key, or given explicitly.
    for tz in [
        RIYADH,
        chrono_tz::America::Los_Angeles,
        chrono_tz::Asia::Kolkata,
        chrono_tz::Pacific::Auckland,
    ] {
        let mut daily = wrap("BEGIN:VEVENT\nUID:ad\nDTSTART;VALUE=DATE:20240101\nRRULE:FREQ=DAILY;COUNT=5\nEND:VEVENT");
        let occ = occurrences(
            &daily,
            ms("2023-12-01T00:00:00Z"),
            ms("2024-02-01T00:00:00Z"),
            tz,
        )
        .unwrap();
        delete_occurrence(&mut daily, occ[2].recurrence_id.unwrap()).unwrap();
        let ex = daily.children[0].prop("EXDATE").unwrap();
        assert_eq!(
            (ex.param("VALUE"), ex.value.as_str()),
            (Some("DATE"), "20240103"),
            "{tz}"
        );
        delete_occurrence_in(&mut daily, occ[3].recurrence_id.unwrap(), tz).unwrap();
        let left = occurrences(
            &daily,
            ms("2023-12-01T00:00:00Z"),
            ms("2024-02-01T00:00:00Z"),
            tz,
        )
        .unwrap();
        assert_eq!(left.len(), 3, "{tz}");
    }
}

#[test]
fn json_shapes() {
    let scope: EditScope = serde_json::from_str(r#"{"type":"this","recurrenceId":5}"#).unwrap();
    assert_eq!(scope, EditScope::This { recurrence_id: 5 });
    let all: EditScope = serde_json::from_str(r#"{"type":"all"}"#).unwrap();
    assert_eq!(all, EditScope::All);
    let input: EventInput = serde_json::from_str(
        r#"{"summary":"x","location":"","description":"","startMs":0,"endMs":1,"allDay":false,"tz":"UTC","rrule":null,"alarmsMinutesBefore":[]}"#,
    )
    .unwrap();
    assert_eq!(input.end_ms, 1);
    let cal = new_object(&input);
    let occ = occurrences(&cal, -1, 10, chrono_tz::UTC).unwrap();
    let json = serde_json::to_value(&occ[0]).unwrap();
    assert!(json.get("recurrenceId").is_some() && json.get("alarmsMinutesBefore").is_some());
}

#[test]
fn hostile_and_partial_objects() {
    let nasty = [
        "BEGIN:VEVENT\nUID:1\nDTSTART:20240101T100000Z\nDURATION:P99999999999999999D\nEND:VEVENT",
        "BEGIN:VEVENT\nUID:2\nDTSTART:20240101T100000Z\nDURATION:P9999999W\nRRULE:FREQ=DAILY;COUNT=3\nEND:VEVENT",
        "BEGIN:VEVENT\nUID:3\nDTSTART:garbage\nRRULE:FREQ=DAILY\nEND:VEVENT",
        "BEGIN:VEVENT\nUID:4\nDTSTART:20240101T100000Z\nRRULE:FREQ=MONTHLY;BYDAY=1ü\nEND:VEVENT",
        "BEGIN:VEVENT\nUID:5\nDTSTART:20240101T100000Z\nRRULE:FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30\nEND:VEVENT",
        "BEGIN:VEVENT\nUID:6\nDTSTART;VALUE=DATE:99991231\nDTEND;VALUE=DATE:00010101\nRRULE:FREQ=SECONDLY;INTERVAL=4294967295\nEND:VEVENT",
        "BEGIN:VEVENT\nUID:7\nDTSTART;TZID=:20240101T100000\nEXDATE:,,\nRDATE;VALUE=PERIOD:20240105T100000Z/PT1H\nRRULE:FREQ=WEEKLY;COUNT=2;BYSETPOS=400\nEND:VEVENT",
        "BEGIN:VTODO\nUID:8\nDUE:20240101T100000Z\nEND:VTODO",
        "BEGIN:VEVENT\nUID:9\nDTSTART:20240101T100000Z\nBEGIN:VALARM\nTRIGGER:-P99999999999W\nEND:VALARM\nBEGIN:VALARM\nTRIGGER;VALUE=DATE-TIME:xyz\nEND:VALARM\nEND:VEVENT",
    ];
    for body in nasty {
        let cal = wrap(body);
        let _ = occurrences(&cal, i64::MIN, i64::MAX, BERLIN);
        let _ = occurrences(
            &cal,
            ms("2023-01-01T00:00:00Z"),
            ms("2026-01-01T00:00:00Z"),
            BERLIN,
        );
        let _ = object_span(&cal, BERLIN);
    }
    let impossible = wrap(nasty[4]);
    let occ = occurrences(
        &impossible,
        ms("2023-01-01T00:00:00Z"),
        ms("2030-01-01T00:00:00Z"),
        BERLIN,
    )
    .unwrap();
    assert_eq!(occ.len(), 1, "only DTSTART itself");

    // An invitation to a single instance: override without its master.
    let orphan = wrap(
        "BEGIN:VEVENT\nUID:inv\nRECURRENCE-ID:20240110T100000Z\nDTSTART:20240110T110000Z\nDTEND:20240110T120000Z\nSUMMARY:Just this one\nEND:VEVENT",
    );
    let occ = occurrences(
        &orphan,
        ms("2024-01-10T00:00:00Z"),
        ms("2024-01-11T00:00:00Z"),
        BERLIN,
    )
    .unwrap();
    assert_eq!(occ.len(), 1);
    assert_eq!(occ[0].recurrence_id, Some(ms("2024-01-10T10:00:00Z")));
    assert!(occ[0].recurring);
    assert_eq!(
        object_span(&orphan, BERLIN),
        Some((ms("2024-01-10T11:00:00Z"), Some(ms("2024-01-10T12:00:00Z"))))
    );

    // An override whose instance was also EXDATE'd stays deleted.
    let both = wrap(
        "BEGIN:VEVENT\nUID:b\nDTSTART:20240101T100000Z\nRRULE:FREQ=DAILY;COUNT=3\nEXDATE:20240102T100000Z\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:b\nRECURRENCE-ID:20240102T100000Z\nDTSTART:20240102T150000Z\nEND:VEVENT",
    );
    assert_eq!(
        starts(
            &both,
            "2024-01-01T00:00:00Z",
            "2024-01-05T00:00:00Z",
            BERLIN
        ),
        ["2024-01-01T10:00:00Z", "2024-01-03T10:00:00Z"]
    );
}

#[test]
fn hostile_rules_stay_fast() {
    // Never matches, every second: only DTSTART, quickly.
    let secondly = wrap("BEGIN:VEVENT\nUID:s\nDTSTART:20240101T100000Z\nRRULE:FREQ=SECONDLY;BYMONTH=2;BYMONTHDAY=30\nEND:VEVENT");
    let t = std::time::Instant::now();
    let occ = occurrences(&secondly, ms("2024-01-01T00:00:00Z"), ms("2024-01-02T00:00:00Z"), BERLIN).unwrap();
    let _ = object_span(&secondly, BERLIN);
    assert!(t.elapsed() < std::time::Duration::from_millis(50), "{:?}", t.elapsed());
    assert_eq!(occ.len(), 1);
    assert_eq!(occ[0].start_ms, ms("2024-01-01T10:00:00Z"));
    let minutely = wrap("BEGIN:VEVENT\nUID:m\nDTSTART:20240101T100000Z\nRRULE:FREQ=MINUTELY;UNTIL=99991231T000000Z\nEND:VEVENT");
    assert_eq!(starts(&minutely, "2024-01-01T00:00:00Z", "2024-01-02T00:00:00Z", BERLIN), ["2024-01-01T10:00:00Z"]);

    // A rule bounded by UNTIL/COUNT that never matches: the series walks a
    // bounded number of periods, however many overrides need checking.
    let mut body = String::from("BEGIN:VEVENT\nUID:c\nDTSTART:20240101T100000Z\nRRULE:FREQ=HOURLY;COUNT=5;BYMONTH=2;BYMONTHDAY=30\nEND:VEVENT\n");
    for d in 1..=28 {
        body.push_str(&format!("BEGIN:VEVENT\nUID:c\nRECURRENCE-ID:202402{d:02}T100000Z\nDTSTART:202402{d:02}T110000Z\nEND:VEVENT\n"));
    }
    let cal = wrap(body.trim_end());
    let t = std::time::Instant::now();
    let occ = occurrences(&cal, ms("2024-01-01T00:00:00Z"), ms("2024-03-01T00:00:00Z"), BERLIN).unwrap();
    let span = object_span(&cal, BERLIN);
    assert!(t.elapsed() < std::time::Duration::from_secs(3), "{:?}", t.elapsed());
    assert_eq!(occ.len(), 1, "overrides of instances that don't exist are dropped");
    assert_eq!(span.map(|s| s.0), Some(ms("2024-01-01T10:00:00Z")));

    // Only the first RRULE counts.
    let two = wrap("BEGIN:VEVENT\nUID:t\nDTSTART:20240101T100000Z\nRRULE:FREQ=DAILY;COUNT=2\nRRULE:FREQ=HOURLY\nEND:VEVENT");
    assert_eq!(
        starts(&two, "2024-01-01T00:00:00Z", "2024-01-03T00:00:00Z", BERLIN),
        ["2024-01-01T10:00:00Z", "2024-01-02T10:00:00Z"]
    );
}

#[test]
fn hourly_across_spring_forward_has_unique_starts() {
    // 02:00 doesn't exist in New York on 2024-03-10; it maps to 07:00Z like 03:00 does.
    let cal = wrap("BEGIN:VEVENT\nUID:h\nDTSTART;TZID=America/New_York:20240310T000000\nDURATION:PT30M\nRRULE:FREQ=HOURLY;COUNT=5\nEND:VEVENT");
    let got = starts(&cal, "2024-03-10T00:00:00Z", "2024-03-11T00:00:00Z", chrono_tz::America::New_York);
    assert_eq!(
        got,
        ["2024-03-10T05:00:00Z", "2024-03-10T06:00:00Z", "2024-03-10T07:00:00Z", "2024-03-10T08:00:00Z"]
    );
    let occ = occurrences(&cal, ms("2024-03-10T00:00:00Z"), ms("2024-03-11T00:00:00Z"), BERLIN).unwrap();
    let keys: std::collections::HashSet<_> = occ.iter().map(|o| o.recurrence_id).collect();
    assert_eq!(keys.len(), occ.len());
}

#[test]
fn placeholder_and_absurd_alarms_are_ignored() {
    let cal = wrap(
        "BEGIN:VEVENT\nUID:a\nDTSTART:20241020T090000Z\nDTEND:20241020T100000Z\n\
BEGIN:VALARM\nACTION:NONE\nTRIGGER;VALUE=DATE-TIME:19760401T005545Z\nEND:VALARM\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER;VALUE=DATE-TIME:19760401T005545Z\nEND:VALARM\n\
BEGIN:VALARM\nACTION:DISPLAY\nTRIGGER;VALUE=DATE-TIME:20241019T090000Z\nEND:VALARM\n\
BEGIN:VALARM\nACTION:AUDIO\nTRIGGER:-PT10M\nEND:VALARM\nEND:VEVENT",
    );
    let occ = occurrences(&cal, ms("2024-10-20T00:00:00Z"), ms("2024-10-21T00:00:00Z"), BERLIN).unwrap();
    assert_eq!(occ[0].alarms_minutes_before, vec![1440, 10]);
}

#[test]
fn occurrence_range_boundaries() {
    let cal = wrap(
        "BEGIN:VEVENT\nUID:before\nDTSTART:20240101T090000Z\nDTEND:20240101T100000Z\nSUMMARY:ends at from\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:zero\nDTSTART:20240101T100000Z\nSUMMARY:zero length at from\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:after\nDTSTART:20240101T120000Z\nDTEND:20240101T130000Z\nSUMMARY:starts at to\nEND:VEVENT",
    );
    let occ = occurrences(&cal, ms("2024-01-01T10:00:00Z"), ms("2024-01-01T12:00:00Z"), BERLIN).unwrap();
    let names: Vec<&str> = occ.iter().map(|o| o.summary.as_str()).collect();
    assert_eq!(names, ["zero length at from"]);
    // A series instance starting exactly at `to` is excluded too.
    let daily = wrap("BEGIN:VEVENT\nUID:d\nDTSTART:20240101T100000Z\nDTEND:20240101T110000Z\nRRULE:FREQ=DAILY\nEND:VEVENT");
    assert_eq!(
        starts(&daily, "2024-01-02T10:30:00Z", "2024-01-03T10:00:00Z", BERLIN),
        ["2024-01-02T10:00:00Z"]
    );
}

#[test]
fn rrule_windows_follow_rfc_5545() {
    let daily = wrap("BEGIN:VEVENT\nUID:d\nDTSTART:20240101T090000Z\nRRULE:FREQ=DAILY;COUNT=10\nEND:VEVENT");
    let got = starts(&daily, "2024-01-08T00:00:00Z", "2024-01-31T00:00:00Z", BERLIN);
    assert_eq!(got.last().map(String::as_str), Some("2024-01-10T09:00:00Z"));
    assert_eq!(got.len(), 3);
    // Months without a 31st are skipped (RFC 5545 §3.3.10: invalid dates are ignored).
    let quarterly = wrap("BEGIN:VEVENT\nUID:q\nDTSTART:20240131T090000Z\nRRULE:FREQ=MONTHLY;INTERVAL=3\nEND:VEVENT");
    assert_eq!(
        starts(&quarterly, "2025-06-01T00:00:00Z", "2025-12-31T00:00:00Z", BERLIN),
        ["2025-07-31T09:00:00Z", "2025-10-31T09:00:00Z"]
    );
}

#[test]
fn spans_of_floating_and_all_day_events_are_padded() {
    let all_day = wrap("BEGIN:VEVENT\nUID:a\nDTSTART;VALUE=DATE:20240105\nDTEND;VALUE=DATE:20240106\nEND:VEVENT");
    let hours = 3_600_000;
    assert_eq!(
        object_span(&all_day, chrono_tz::UTC),
        Some((ms("2024-01-05T00:00:00Z") - 14 * hours, Some(ms("2024-01-06T00:00:00Z") + 14 * hours)))
    );
    // Shown in Kiritimati (UTC+14) or Samoa it still falls inside the stored span.
    let (lo, hi) = object_span(&all_day, chrono_tz::UTC).unwrap();
    for tz in [chrono_tz::Pacific::Kiritimati, chrono_tz::Pacific::Pago_Pago] {
        let o = &occurrences(&all_day, i64::MIN / 4, i64::MAX / 4, tz).unwrap()[0];
        assert!(o.start_ms >= lo && o.end_ms <= hi.unwrap(), "{tz}");
    }
}

#[test]
fn deleting_the_only_instance_of_an_invitation_leaves_no_event() {
    let mut orphan = wrap(
        "BEGIN:VEVENT\nUID:inv\nRECURRENCE-ID:20240110T100000Z\nDTSTART:20240110T110000Z\nDTEND:20240110T120000Z\nEND:VEVENT",
    );
    delete_occurrence_in(&mut orphan, ms("2024-01-10T10:00:00Z"), BERLIN).unwrap();
    assert!(orphan.children_named("VEVENT").next().is_none());
}
