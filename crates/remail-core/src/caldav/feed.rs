//! iCal subscriptions: read-only `.ics` feeds (webcal links, Google's "secret
//! address in iCal format", Outlook's published calendars, holiday calendars).
//!
//! A feed is one VCALENDAR holding every event. It is split into one object
//! per UID (the master, its overrides and the time zones they use) so it is
//! stored, diffed and expanded exactly like a CalDAV calendar.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Result};
use reqwest::Url;

use super::event::duration_secs;
use super::ical::{Component, Property};

/// Bounds for the publisher's `REFRESH-INTERVAL` / `X-PUBLISHED-TTL`.
pub const MIN_REFRESH_SECS: i64 = 15 * 60;
pub const MAX_REFRESH_SECS: i64 = 24 * 3600;
pub const DEFAULT_REFRESH_SECS: i64 = 30 * 60;

/// One stored object of a feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedObject {
    /// `ics:<UID>`: stable across refreshes.
    pub href: String,
    /// Hash of `ics` (ignoring DTSTAMP), so unchanged events aren't rewritten.
    pub etag: String,
    pub ics: String,
}

#[derive(Debug, Clone)]
pub struct Feed {
    pub name: Option<String>,
    /// `#rrggbb`
    pub color: Option<String>,
    /// Seconds between refreshes the publisher asks for (clamped).
    pub refresh_secs: Option<i64>,
    pub objects: Vec<FeedObject>,
}

/// `webcal://` links and bare addresses become `https://`; only http(s) is allowed.
pub fn normalize_url(input: &str) -> Result<Url> {
    let s = input.trim();
    if s.is_empty() {
        bail!("Enter the calendar's iCal link");
    }
    let lower = s.to_ascii_lowercase();
    let full = if let Some(rest) = ["webcals://", "webcal://"].iter().find_map(|p| lower.starts_with(p).then(|| &s[p.len()..])) {
        format!("https://{rest}")
    } else if lower.starts_with("https://") || lower.starts_with("http://") {
        s.to_string()
    } else if lower.contains("://") {
        bail!("Use an https, http or webcal link");
    } else {
        format!("https://{s}")
    };
    let url = Url::parse(&full).map_err(|_| anyhow!("That doesn't look like a link"))?;
    match url.host_str() {
        Some(h) if !h.is_empty() => Ok(url),
        _ => bail!("That doesn't look like a link"),
    }
}

/// Read a feed and split it into objects.
pub fn parse_feed(body: &str) -> Result<Feed> {
    let head = body.trim_start_matches('\u{feff}').trim_start();
    if !head.get(..15).is_some_and(|h| h.eq_ignore_ascii_case("BEGIN:VCALENDAR")) {
        if head.starts_with('<') {
            bail!("This link opens a web page, not an iCal calendar. Copy the calendar's iCal (.ics) address instead.");
        }
        bail!("This link doesn't point to an iCal calendar.");
    }
    // A cut-off download would otherwise delete the events it lost.
    if !head.to_ascii_uppercase().contains("END:VCALENDAR") {
        bail!("The calendar download was incomplete; Remail will try again later.");
    }
    let cal = Component::parse(head)?;
    let text = |names: &[&str]| {
        names.iter().find_map(|n| cal.prop(n)).map(|p| p.text().trim().to_string()).filter(|s| !s.is_empty())
    };
    let name = text(&["X-WR-CALNAME", "NAME"]);
    let color = text(&["X-APPLE-CALENDAR-COLOR", "COLOR"]).and_then(|c| hex_color(&c));
    let refresh_secs = ["REFRESH-INTERVAL", "X-PUBLISHED-TTL"]
        .iter()
        .find_map(|n| cal.prop(n))
        .and_then(|p| duration_secs(&p.value))
        .filter(|s| *s > 0)
        .map(|s| s.clamp(MIN_REFRESH_SECS, MAX_REFRESH_SECS));

    // VEVENTs grouped by UID (a series with its overrides), in feed order.
    // Events without a UID stand alone, and so does every further event
    // that reuses a UID which already has its main event (sloppy feeds do).
    let mut groups: Vec<(String, Vec<&Component>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for ev in cal.children_named("VEVENT") {
        let uid = ev.prop("UID").map(|p| p.text().trim().to_string()).unwrap_or_default();
        let mut key = if uid.is_empty() { format!("nouid-{}", fnv64(&ev.to_ics())) } else { uid };
        let is_master = ev.prop("RECURRENCE-ID").is_none();
        let has_master = |g: &[&Component]| g.iter().any(|e| e.prop("RECURRENCE-ID").is_none());
        if is_master && index.get(&key).is_some_and(|&i| has_master(&groups[i].1)) {
            let base = key.clone();
            let mut n = 2;
            while index.get(&key).is_some_and(|&i| has_master(&groups[i].1)) {
                key = format!("{base}#{n}");
                n += 1;
            }
        }
        match index.get(&key) {
            Some(&i) => groups[i].1.push(ev),
            None => {
                index.insert(key.clone(), groups.len());
                groups.push((key, vec![ev]));
            }
        }
    }
    let zones: Vec<&Component> = cal.children_named("VTIMEZONE").collect();
    let objects = groups
        .into_iter()
        .map(|(key, events)| {
            let mut obj = Component::new("VCALENDAR");
            obj.props.push(Property::new("VERSION", "2.0"));
            obj.props.push(Property::new("PRODID", "-//Remail//iCal subscription//EN"));
            let used: HashSet<String> = events.iter().flat_map(|e| tzids(e)).collect();
            for z in &zones {
                if z.prop("TZID").is_some_and(|p| used.contains(clean_tzid(&p.value))) {
                    obj.children.push((*z).clone());
                }
            }
            obj.children.extend(events.into_iter().cloned());
            let ics = obj.to_ics();
            FeedObject { href: format!("ics:{key}"), etag: stable_hash(&ics), ics }
        })
        .collect();
    Ok(Feed { name, color, refresh_secs, objects })
}

/// TZIDs referenced by a component and its children (e.g. VALARMs).
fn tzids(c: &Component) -> Vec<String> {
    let mut out: Vec<String> = c.props.iter().filter_map(|p| p.param("TZID")).map(|t| clean_tzid(t).to_string()).collect();
    for child in &c.children {
        out.extend(tzids(child));
    }
    out
}

fn clean_tzid(t: &str) -> &str {
    t.trim().trim_matches('"')
}

/// `#RRGGBB` or `#RRGGBBAA` → `#rrggbb`.
fn hex_color(c: &str) -> Option<String> {
    let hex = c.strip_prefix('#')?;
    ((hex.len() == 6 || hex.len() == 8) && hex.chars().all(|ch| ch.is_ascii_hexdigit())).then(|| format!("#{}", hex[..6].to_ascii_lowercase()))
}

/// FNV-1a: a stable content hash (unlike std's hasher, it doesn't change between builds).
pub fn fnv64(s: &str) -> String {
    format!("{:016x}", fnv_bytes(0xcbf2_9ce4_8422_2325, s.as_bytes()))
}

fn fnv_bytes(mut h: u64, bytes: &[u8]) -> u64 {
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Hash of iCalendar text without its DTSTAMP lines: some publishers (Google)
/// stamp every event with the download time, which isn't a change.
pub fn stable_hash(ics: &str) -> String {
    let mut h = 0xcbf2_9ce4_8422_2325;
    for line in ics.split('\n') {
        if line.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("DTSTAMP")) {
            continue;
        }
        h = fnv_bytes(h, line.as_bytes());
        h = fnv_bytes(h, b"\n");
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\nX-WR-CALNAME:Team\r\nX-APPLE-CALENDAR-COLOR:#34C759FF\r\nX-PUBLISHED-TTL:PT5M\r\n\
BEGIN:VTIMEZONE\r\nTZID:Asia/Riyadh\r\nBEGIN:STANDARD\r\nDTSTART:19700101T000000\r\nTZOFFSETFROM:+0300\r\nTZOFFSETTO:+0300\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n\
BEGIN:VTIMEZONE\r\nTZID:Europe/Paris\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:standup@x\r\nDTSTART;TZID=Asia/Riyadh:20261019T093000\r\nDTEND;TZID=Asia/Riyadh:20261019T094500\r\nRRULE:FREQ=DAILY;COUNT=5\r\nSUMMARY:Standup\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:standup@x\r\nRECURRENCE-ID;TZID=Asia/Riyadh:20261021T093000\r\nDTSTART;TZID=Asia/Riyadh:20261021T110000\r\nDTEND;TZID=Asia/Riyadh:20261021T111500\r\nSUMMARY:Standup (moved)\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:holiday@x\r\nDTSTART;VALUE=DATE:20261225\r\nSUMMARY:Holiday\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nDTSTART:20261101T100000Z\r\nSUMMARY:No uid\r\nEND:VEVENT\r\n\
BEGIN:VTODO\r\nUID:todo@x\r\nSUMMARY:Ignored\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";

    #[test]
    fn urls() {
        assert_eq!(normalize_url(" webcal://p01-calendars.icloud.com/published/2/abc ").unwrap().as_str(), "https://p01-calendars.icloud.com/published/2/abc");
        assert_eq!(normalize_url("WEBCALS://example.com/a.ics").unwrap().as_str(), "https://example.com/a.ics");
        assert_eq!(normalize_url("calendar.google.com/calendar/ical/x/basic.ics").unwrap().scheme(), "https");
        assert_eq!(normalize_url("http://192.168.1.5:8080/cal.ics").unwrap().as_str(), "http://192.168.1.5:8080/cal.ics");
        assert!(normalize_url("ftp://example.com/a.ics").is_err());
        assert!(normalize_url("file:///etc/passwd").is_err());
        assert!(normalize_url("").is_err());
    }

    #[test]
    fn splits_by_uid_with_used_zones() {
        let f = parse_feed(&format!("\u{feff}\n{FEED}")).unwrap();
        assert_eq!(f.name.as_deref(), Some("Team"));
        assert_eq!(f.color.as_deref(), Some("#34c759"));
        assert_eq!(f.refresh_secs, Some(MIN_REFRESH_SECS), "PT5M is clamped up");
        assert_eq!(f.objects.len(), 3, "series + override together, holiday, the UID-less event; no VTODO");
        let standup = &f.objects[0];
        assert_eq!(standup.href, "ics:standup@x");
        let c = Component::parse(&standup.ics).unwrap();
        assert_eq!(c.children_named("VEVENT").count(), 2);
        let zones: Vec<_> = c.children_named("VTIMEZONE").map(|z| z.prop("TZID").unwrap().value.clone()).collect();
        assert_eq!(zones, ["Asia/Riyadh"], "only the zone it uses");
        assert_eq!(Component::parse(&f.objects[1].ics).unwrap().children_named("VTIMEZONE").count(), 0);
        assert!(f.objects[2].href.starts_with("ics:nouid-"));
        // Same feed, same objects: nothing to rewrite on refresh.
        assert_eq!(parse_feed(FEED).unwrap().objects, f.objects);
    }

    #[test]
    fn expands_like_caldav_objects() {
        let f = parse_feed(FEED).unwrap();
        let c = Component::parse(&f.objects[0].ics).unwrap();
        let from = chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
        let mut occ = super::super::event::occurrences(&c, from, from + 60 * 86_400_000, chrono_tz::UTC).unwrap();
        occ.sort_by_key(|o| o.start_ms);
        let titles: Vec<_> = occ.iter().map(|o| o.summary.as_str()).collect();
        assert_eq!(titles, ["Standup", "Standup", "Standup (moved)", "Standup", "Standup"]);
    }

    #[test]
    fn reused_uids_and_timestamps() {
        let ev = |uid: &str, rid: &str, title: &str, stamp: &str| {
            format!("BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:{stamp}\r\n{rid}DTSTART:20261020T100000Z\r\nSUMMARY:{title}\r\nEND:VEVENT\r\n")
        };
        let feed = |stamp: &str| {
            format!(
                "BEGIN:VCALENDAR\r\n{}{}{}{}END:VCALENDAR\r\n",
                ev("same", "", "First", stamp),
                ev("same", "", "Second", stamp),
                ev("same", "RECURRENCE-ID:20261020T100000Z\r\n", "Override", stamp),
                ev("same", "", "Third", stamp)
            )
        };
        let f = parse_feed(&feed("20261019T080000Z")).unwrap();
        let hrefs: Vec<_> = f.objects.iter().map(|o| o.href.as_str()).collect();
        assert_eq!(hrefs, ["ics:same", "ics:same#2", "ics:same#3"], "each event that reuses a UID is kept");
        assert_eq!(Component::parse(&f.objects[0].ics).unwrap().children_named("VEVENT").count(), 2, "the override joins the first");
        // Only DTSTAMP changed: same hashes, nothing to rewrite.
        let g = parse_feed(&feed("20261019T093000Z")).unwrap();
        assert_eq!(f.objects.iter().map(|o| &o.etag).collect::<Vec<_>>(), g.objects.iter().map(|o| &o.etag).collect::<Vec<_>>());
        assert_eq!(stable_hash(&feed("20261019T080000Z")), stable_hash(&feed("20261019T093000Z")));
        assert_ne!(stable_hash(&feed("x")), stable_hash(&feed("x").replace("First", "1st")));
    }

    #[test]
    fn rejects_non_calendars() {
        let e = parse_feed("<!DOCTYPE html><html>Google Calendar</html>").unwrap_err().to_string();
        assert!(e.contains("web page"), "{e}");
        assert!(parse_feed("{\"error\":1}").is_err());
        let cut = parse_feed("BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:a\nDTSTART:20261020T100000Z\nSUMM").unwrap_err().to_string();
        assert!(cut.contains("incomplete"), "{cut}");
        let empty = parse_feed("BEGIN:VCALENDAR\nVERSION:2.0\nEND:VCALENDAR\n").unwrap();
        assert!(empty.objects.is_empty() && empty.name.is_none() && empty.refresh_secs.is_none());
    }

    #[test]
    fn refresh_and_colour_hints() {
        let f = parse_feed("BEGIN:VCALENDAR\nREFRESH-INTERVAL;VALUE=DURATION:P1W\nCOLOR:turquoise\nNAME:Holidays\nEND:VCALENDAR\n").unwrap();
        assert_eq!(f.refresh_secs, Some(MAX_REFRESH_SECS));
        assert_eq!(f.color, None, "CSS colour names aren't used");
        assert_eq!(f.name.as_deref(), Some("Holidays"));
        assert_eq!(fnv64("a"), "af63dc4c8601ec8c");
    }
}
