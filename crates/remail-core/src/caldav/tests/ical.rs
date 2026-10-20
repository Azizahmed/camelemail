use crate::caldav::ical::{escape_text, unescape_text, Component, Property};

/// Apple-style object: folded lines, escapes, VTIMEZONE, VALARM, X- props, ATTENDEE params.
pub const APPLE_ICS: &str = "BEGIN:VCALENDAR\r
VERSION:2.0\r
PRODID:-//Apple Inc.//macOS 14.5//EN\r
CALSCALE:GREGORIAN\r
BEGIN:VTIMEZONE\r
TZID:Europe/Berlin\r
BEGIN:DAYLIGHT\r
TZOFFSETFROM:+0100\r
RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r
DTSTART:19810329T020000\r
TZNAME:CEST\r
TZOFFSETTO:+0200\r
END:DAYLIGHT\r
BEGIN:STANDARD\r
TZOFFSETFROM:+0200\r
RRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r
DTSTART:19961027T030000\r
TZNAME:CET\r
TZOFFSETTO:+0100\r
END:STANDARD\r
END:VTIMEZONE\r
BEGIN:VEVENT\r
CREATED:20240110T081500Z\r
UID:9F1A6C1E-5D35-4C5B-9B7A-1E2F3A4B5C6D\r
DTEND;TZID=Europe/Berlin:20240115T100000\r
TRANSP:OPAQUE\r
X-APPLE-TRAVEL-ADVISORY-BEHAVIOR:AUTOMATIC\r
SUMMARY:Weekly sync\\, planning \\; review\r
LAST-MODIFIED:20240110T081600Z\r
DTSTAMP:20240110T081600Z\r
DTSTART;TZID=Europe/Berlin:20240115T090000\r
SEQUENCE:1\r
RRULE:FREQ=WEEKLY;BYDAY=MO\r
DESCRIPTION:Agenda:\\n1. Status\\n2. Risks — please read the doc beforehand\r
  so we can keep this short. Ünïcödé text to force folding across multi-b\r
 yte characters.\r
ORGANIZER;CN=\"Doe, Jane\";EMAIL=jane@example.com:mailto:jane@example.com\r
ATTENDEE;CN=Bob;CUTYPE=INDIVIDUAL;PARTSTAT=ACCEPTED;ROLE=REQ-PARTICIPANT;X-NUM-GUESTS=0:mailto:bob@example.com\r
X-APPLE-STRUCTURED-LOCATION;VALUE=URI;X-ADDRESS=\"Unter den Linden 1, Berlin\";X-TITLE=HQ:geo:52.517,13.388\r
BEGIN:VALARM\r
X-WR-ALARMUID:1C2D3E4F\r
UID:1C2D3E4F\r
TRIGGER:-PT15M\r
ATTACH;VALUE=URI:Chord\r
ACTION:AUDIO\r
X-APPLE-DEFAULT-ALARM:TRUE\r
END:VALARM\r
END:VEVENT\r
END:VCALENDAR\r
";

#[test]
fn round_trip_keeps_everything() {
    let cal = Component::parse(APPLE_ICS).unwrap();
    let out = cal.to_ics();
    let again = Component::parse(&out).unwrap();
    assert_eq!(cal, again);
    assert_eq!(cal.children.len(), 2);
    let tz = &cal.children[0];
    assert_eq!(tz.name, "VTIMEZONE");
    assert_eq!(tz.children.len(), 2);
    let ev = &cal.children[1];
    assert_eq!(
        ev.prop("SUMMARY").unwrap().text(),
        "Weekly sync, planning ; review"
    );
    let desc = ev.prop("DESCRIPTION").unwrap().text();
    assert!(desc.starts_with("Agenda:\n1. Status\n2. Risks"));
    assert!(desc.contains(
        " so we can keep this short. Ünïcödé text to force folding across multi-byte characters."
    ));
    let org = ev.prop("ORGANIZER").unwrap();
    assert_eq!(org.param("cn"), Some("Doe, Jane"));
    let loc = ev.prop("X-APPLE-STRUCTURED-LOCATION").unwrap();
    assert_eq!(loc.param("X-ADDRESS"), Some("Unter den Linden 1, Berlin"));
    assert_eq!(loc.value, "geo:52.517,13.388");
    assert_eq!(
        ev.children[0].prop("X-APPLE-DEFAULT-ALARM").unwrap().value,
        "TRUE"
    );
    // Folded output: every physical line is at most 75 octets and ends in CRLF.
    for line in out.split("\r\n") {
        assert!(line.len() <= 75, "line too long: {line:?}");
    }
    assert!(out.ends_with("END:VCALENDAR\r\n"));
    assert!(!out.replace("\r\n", "").contains('\n'));
    assert!(
        out.contains("ORGANIZER;CN=\"Doe, Jane\";EMAIL=jane@example.com:mailto:jane@example.com")
    );
}

#[test]
fn google_and_outlook_quirks() {
    // LF-only line endings, BOM, lowercase names, tab folding, trailing junk.
    let ics = "\u{feff}begin:vcalendar\nversion:2.0\nprodid:-//Google Inc//Google Calendar 70.9054//EN\nX-WR-CALNAME:Work\nbegin:vevent\ndtstart:20240301T090000Z\ndtend:20240301T100000Z\nsummary:Long\n\tline\nuid:abc@google.com\nX-MICROSOFT-CDO-BUSYSTATUS:BUSY\nend:vevent\nend:vcalendar\ngarbage after the end\n";
    let cal = Component::parse(ics).unwrap();
    assert_eq!(cal.name, "VCALENDAR");
    let ev = &cal.children[0];
    assert_eq!(ev.name, "VEVENT");
    assert_eq!(ev.prop("SUMMARY").unwrap().text(), "Longline");
    assert_eq!(ev.prop("uid").unwrap().value, "abc@google.com");
    assert_eq!(ev.prop("X-MICROSOFT-CDO-BUSYSTATUS").unwrap().value, "BUSY");
    let out = cal.to_ics();
    assert!(out.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
    assert!(!out.contains("garbage"));
}

#[test]
fn unclosed_and_bare_components() {
    let cal = Component::parse("BEGIN:VEVENT\nUID:x\nSUMMARY:no wrapper\n").unwrap();
    assert_eq!(cal.name, "VCALENDAR");
    assert_eq!(cal.children[0].prop("UID").unwrap().value, "x");
    assert!(Component::parse("").is_err());
    assert!(Component::parse("hello world").is_err());
}

#[test]
fn text_escaping() {
    let raw = "a,b;c\\d\ne";
    let esc = escape_text(raw);
    assert_eq!(esc, "a\\,b\\;c\\\\d\\ne");
    assert_eq!(unescape_text(&esc), raw);
    assert_eq!(unescape_text("x\\Ny"), "x\ny");
    let p = Property::new_text("SUMMARY", "Lunch, then; talk");
    assert_eq!(p.value, "Lunch\\, then\\; talk");
    assert_eq!(p.text(), "Lunch, then; talk");
}

#[test]
fn folding_is_utf8_safe() {
    let mut cal = Component::new("VCALENDAR");
    let mut ev = Component::new("VEVENT");
    let text = "مرحبا بالعالم ".repeat(20);
    ev.props.push(Property::new_text("SUMMARY", &text));
    cal.children.push(ev);
    let out = cal.to_ics();
    for line in out.split("\r\n") {
        assert!(line.len() <= 75);
    }
    let back = Component::parse(&out).unwrap();
    assert_eq!(back.children[0].prop("SUMMARY").unwrap().text(), text);
}

#[test]
fn set_and_remove_props() {
    let mut ev = Component::new("VEVENT");
    ev.props.push(Property::new("ATTENDEE", "mailto:a@x"));
    ev.props.push(Property::new("SUMMARY", "one"));
    ev.props.push(Property::new("ATTENDEE", "mailto:b@x"));
    ev.set_prop(Property::new("ATTENDEE", "mailto:c@x"));
    assert_eq!(ev.props("ATTENDEE").count(), 1);
    assert_eq!(ev.props[0].value, "mailto:c@x");
    ev.remove_props("summary");
    assert!(ev.prop("SUMMARY").is_none());
}

#[test]
fn deep_nesting_is_cut_off() {
    // ~1 MB of nested BEGIN lines (well under the feed size cap) must not overflow
    // the stack while parsing, cloning, writing or dropping.
    let n = 100_000;
    let ics = format!(
        "BEGIN:VCALENDAR\nVERSION:2.0\n{}{}BEGIN:VEVENT\nUID:after\nSUMMARY:Still here\nEND:VEVENT\nEND:VCALENDAR\n",
        "BEGIN:X\nSUMMARY:deep\n".repeat(n),
        "END:X\n".repeat(n)
    );
    let cal = Component::parse(&ics).unwrap();
    let mut depth = 0;
    let mut c = &cal;
    while let Some(child) = c.children.first() {
        depth += 1;
        c = child;
    }
    assert!(depth < crate::caldav::ical::MAX_DEPTH, "{depth}");
    let copy = cal.clone();
    let out = copy.to_ics();
    assert!(out.len() < 4096);
    // What follows the skipped part is still read.
    let ev = cal.children_named("VEVENT").next().expect("event after the deep part");
    assert_eq!(ev.prop("SUMMARY").unwrap().text(), "Still here");
    drop(copy);
    drop(cal);

    // Unterminated nesting.
    let open = format!("BEGIN:VCALENDAR\n{}", "BEGIN:X\n".repeat(n));
    let cal = Component::parse(&open).unwrap();
    let _ = cal.clone().to_ics();
}
