//! Time zone resolution for iCalendar TZIDs and VTIMEZONE generation.
//!
//! TZIDs are matched as IANA names (including vendor prefixes such as
//! `/mozilla.org/20050126_1/Europe/Berlin`), Windows zone names, and Outlook
//! display names. Unknown zones fall back to the object's own VTIMEZONE rules.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, Offset, TimeZone};
use chrono_tz::{OffsetComponents, OffsetName, Tz};

use crate::caldav::ical::{Component, Property};
use crate::caldav::recur::{month_len, Recur, Until};

/// A zone used to map wall-clock times to instants.
#[derive(Debug, Clone)]
pub enum Zone {
    /// A known IANA zone (UTC is `Tz::UTC`).
    Tz(Tz),
    /// Rules taken from an unrecognised VTIMEZONE.
    Rules(Arc<VtzRules>),
    /// A fixed offset in seconds east of UTC.
    Fixed(i32),
}

impl Zone {
    /// UTC epoch milliseconds of a wall-clock time. Ambiguous times take the
    /// first occurrence; times in a DST gap use the offset before the gap (RFC 5545).
    pub fn utc_ms(&self, local: NaiveDateTime) -> i64 {
        let offset = match self {
            Zone::Tz(tz) => match tz.from_local_datetime(&local).earliest() {
                Some(dt) => i64::from(dt.offset().fix().local_minus_utc()),
                None => (1..=48)
                    .find_map(|h| {
                        tz.from_local_datetime(&(local - Duration::minutes(30 * h)))
                            .earliest()
                    })
                    .map(|dt| i64::from(dt.offset().fix().local_minus_utc()))
                    .unwrap_or(0),
            },
            Zone::Rules(r) => i64::from(r.offset_for_local(local)),
            Zone::Fixed(off) => i64::from(*off),
        };
        local.and_utc().timestamp_millis() - offset * 1000
    }

    /// Wall-clock time of a UTC epoch-milliseconds instant (clamped to the
    /// years 1–9999).
    pub fn local(&self, ms: i64) -> NaiveDateTime {
        let utc = DateTime::from_timestamp_millis(ms.clamp(MIN_MS, MAX_MS))
            .unwrap_or_default()
            .naive_utc();
        match self {
            Zone::Tz(tz) => tz.from_utc_datetime(&utc).naive_local(),
            Zone::Rules(r) => utc + Duration::seconds(i64::from(r.offset_at(utc))),
            Zone::Fixed(off) => utc + Duration::seconds(i64::from(*off)),
        }
    }
}

/// 0001-01-01T00:00:00Z and 9999-12-31T23:59:59.999Z in epoch ms.
const MIN_MS: i64 = -62_135_596_800_000;
const MAX_MS: i64 = 253_402_300_799_999;

/// Resolve a TZID parameter to an IANA zone.
pub fn resolve_tzid(tzid: &str) -> Option<Tz> {
    let id = tzid.trim().trim_matches('"').trim();
    if id.is_empty() {
        return None;
    }
    if let Ok(tz) = id.parse::<Tz>() {
        return Some(tz);
    }
    if matches!(
        id.to_ascii_uppercase().as_str(),
        "Z" | "UTC" | "GMT" | "ETC/UTC" | "UNIVERSAL"
    ) {
        return Some(Tz::UTC);
    }
    let segments: Vec<&str> = id.split('/').filter(|s| !s.is_empty()).collect();
    for n in (1..=segments.len().min(3)).rev() {
        if let Ok(tz) = segments[segments.len() - n..].join("/").parse::<Tz>() {
            return Some(tz);
        }
    }
    let win = id.replace("Daylight Time", "Standard Time");
    if let Some((_, iana)) = WINDOWS_ZONES
        .iter()
        .find(|(w, _)| w.eq_ignore_ascii_case(&win))
    {
        return iana.parse().ok();
    }
    if id.starts_with("(UTC") || id.starts_with("(GMT") || id.contains(',') {
        let lower = id.to_ascii_lowercase();
        if let Some((_, iana)) = DISPLAY_NAMES
            .iter()
            .find(|(k, _)| lower.contains(&k.to_ascii_lowercase()))
        {
            return iana.parse().ok();
        }
    }
    None
}

/// Resolve a TZID using a VTIMEZONE component's hints or rules.
pub fn zone_from_vtimezone(vtz: &Component) -> Option<Zone> {
    for hint in ["X-LIC-LOCATION", "X-WR-TIMEZONE", "X-MICROSOFT-CDO-TZID"] {
        if let Some(tz) = vtz.prop(hint).and_then(|p| resolve_tzid(&p.text())) {
            return Some(Zone::Tz(tz));
        }
    }
    VtzRules::from_component(vtz).map(|r| Zone::Rules(Arc::new(r)))
}

/// One STANDARD or DAYLIGHT observance.
#[derive(Debug, Clone)]
struct Observance {
    start: NaiveDateTime,
    from: i32,
    to: i32,
    rule: Option<Recur>,
    rdates: Vec<NaiveDateTime>,
}

/// Transitions (UTC onset, new offset), sorted.
type Transitions = Arc<Vec<(NaiveDateTime, i32)>>;

/// UTC offset rules of a VTIMEZONE.
#[derive(Debug)]
pub struct VtzRules {
    observances: Vec<Observance>,
    /// Year → transitions around it (expanding the rules for every lookup is slow).
    years: Mutex<HashMap<i32, Transitions>>,
}

impl VtzRules {
    /// Parse STANDARD/DAYLIGHT sub-components; `None` if there are none usable.
    pub fn from_component(vtz: &Component) -> Option<VtzRules> {
        let mut observances = Vec::new();
        for c in &vtz.children {
            if c.name != "STANDARD" && c.name != "DAYLIGHT" {
                continue;
            }
            let Some(to) = c.prop("TZOFFSETTO").and_then(|p| parse_offset(&p.value)) else {
                continue;
            };
            let from = c
                .prop("TZOFFSETFROM")
                .and_then(|p| parse_offset(&p.value))
                .unwrap_or(to);
            let start = c
                .prop("DTSTART")
                .and_then(|p| parse_naive(&p.value))
                .unwrap_or_default();
            let rule = c.prop("RRULE").and_then(|p| Recur::parse(&p.value).ok());
            let rdates = c
                .props("RDATE")
                .flat_map(|p| {
                    p.value
                        .split(',')
                        .filter_map(parse_naive)
                        .collect::<Vec<_>>()
                })
                .collect();
            observances.push(Observance {
                start,
                from,
                to,
                rule,
                rdates,
            });
        }
        (!observances.is_empty()).then(|| VtzRules {
            observances,
            years: Mutex::new(HashMap::new()),
        })
    }

    /// Transitions (UTC onset, new offset) for years around `year`, sorted.
    fn transitions(&self, year: i32) -> Transitions {
        if let Some(t) = self.years.lock().unwrap().get(&year) {
            return t.clone();
        }
        let t = Arc::new(self.expand_transitions(year));
        let mut years = self.years.lock().unwrap();
        if years.len() >= 256 {
            years.clear();
        }
        years.insert(year, t.clone());
        t
    }

    fn expand_transitions(&self, year: i32) -> Vec<(NaiveDateTime, i32)> {
        let lo = NaiveDate::from_ymd_opt(year - 1, 1, 1)
            .unwrap_or(NaiveDate::MIN)
            .and_time(chrono::NaiveTime::MIN);
        let hi = NaiveDate::from_ymd_opt(year + 1, 12, 31)
            .unwrap_or(NaiveDate::MAX)
            .and_time(chrono::NaiveTime::MIN);
        let mut out = Vec::new();
        for o in &self.observances {
            let to_utc = |local: NaiveDateTime| local - Duration::seconds(i64::from(o.from));
            match &o.rule {
                Some(rule) => {
                    let until = rule.until.map(|u| match u {
                        Until::Date(d) => d.and_hms_opt(23, 59, 59).unwrap_or_default(),
                        Until::Utc(t) => t + Duration::seconds(i64::from(o.from)),
                        Until::Local(t) => t,
                    });
                    rule.expand(o.start, until, Some((lo, hi)), |t| {
                        if t >= lo {
                            out.push((to_utc(t), o.to));
                        }
                        true
                    });
                }
                None => out.push((to_utc(o.start), o.to)),
            }
            out.extend(o.rdates.iter().map(|&t| (to_utc(t), o.to)));
        }
        out.sort();
        out
    }

    /// Offset (seconds east of UTC) in effect at a UTC time.
    pub fn offset_at(&self, utc: NaiveDateTime) -> i32 {
        let transitions = self.transitions(utc.year());
        if let Some((_, off)) = transitions.iter().rev().find(|(t, _)| *t <= utc) {
            return *off;
        }
        let earliest = self.observances.iter().min_by_key(|o| o.start);
        earliest.map(|o| o.from).unwrap_or(0)
    }

    /// Offset to use for a wall-clock time (first occurrence if ambiguous).
    fn offset_for_local(&self, local: NaiveDateTime) -> i32 {
        let mut offsets: Vec<i32> = self
            .observances
            .iter()
            .flat_map(|o| [o.from, o.to])
            .collect();
        offsets.sort_unstable_by(|a, b| b.cmp(a));
        offsets.dedup();
        for &off in &offsets {
            if self.offset_at(local - Duration::seconds(i64::from(off))) == off {
                return off;
            }
        }
        // In a gap: the offset before it, in effect when read with the largest
        // offset (a fixed 12 h misses the gap in zones east of UTC+12).
        let largest = offsets.first().copied().unwrap_or(0);
        self.offset_at(local - Duration::seconds(i64::from(largest)))
    }
}

/// Parse `+HHMM[SS]` / `-HHMM[SS]` into seconds.
fn parse_offset(v: &str) -> Option<i32> {
    let v = v.trim();
    let (sign, digits) = match v.as_bytes().first()? {
        b'+' => (1, &v[1..]),
        b'-' => (-1, &v[1..]),
        _ => (1, v),
    };
    if digits.len() < 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let h: i32 = digits[0..2].parse().ok()?;
    let m: i32 = digits[2..4].parse().ok()?;
    let s: i32 = digits.get(4..6).and_then(|s| s.parse().ok()).unwrap_or(0);
    Some(sign * (h * 3600 + m * 60 + s))
}

fn parse_naive(v: &str) -> Option<NaiveDateTime> {
    let v = v.trim().trim_end_matches(['Z', 'z']);
    NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S")
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(v, "%Y%m%d")
                .ok()
                .map(|d| d.and_time(chrono::NaiveTime::MIN))
        })
}

fn fmt_offset(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.unsigned_abs();
    if !a.is_multiple_of(60) {
        format!("{sign}{:02}{:02}{:02}", a / 3600, a / 60 % 60, a % 60)
    } else {
        format!("{sign}{:02}{:02}", a / 3600, a / 60 % 60)
    }
}

/// One UTC offset change of an IANA zone.
struct Transition {
    /// Wall-clock time of the change in the old offset.
    local: NaiveDateTime,
    from: i32,
    to: i32,
    dst: bool,
    name: Option<String>,
}

impl Transition {
    fn component(&self, rrule: Option<String>) -> Component {
        let mut c = Component::new(if self.dst { "DAYLIGHT" } else { "STANDARD" });
        c.props.push(Property::new(
            "DTSTART",
            &self.local.format("%Y%m%dT%H%M%S").to_string(),
        ));
        c.props
            .push(Property::new("TZOFFSETFROM", &fmt_offset(self.from)));
        c.props
            .push(Property::new("TZOFFSETTO", &fmt_offset(self.to)));
        if let Some(r) = rrule {
            c.props.push(Property::new("RRULE", &r));
        }
        if let Some(name) = &self.name {
            c.props.push(Property::new_text("TZNAME", name));
        }
        c
    }
}

/// A yearly `BYMONTH;BYDAY` rule matching every transition, if there is one.
fn yearly_rule(ts: &[&Transition], years: usize) -> Option<String> {
    let first = ts.first()?;
    if ts.len() != years {
        return None;
    }
    let same = ts.iter().all(|t| {
        t.local.month() == first.local.month()
            && t.local.time() == first.local.time()
            && t.local.weekday() == first.local.weekday()
            && (t.from, t.to, &t.name) == (first.from, first.to, &first.name)
    });
    if !same {
        return None;
    }
    let nth = |t: &Transition| (t.local.day() as i32 - 1) / 7 + 1;
    let is_last = |t: &Transition| t.local.day() + 7 > month_len(t.local.year(), t.local.month());
    let ord = if ts.iter().all(|t| is_last(t)) {
        -1
    } else if ts.iter().all(|t| nth(t) == nth(first)) {
        nth(first)
    } else {
        return None;
    };
    let wd = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"]
        [first.local.weekday().num_days_from_monday() as usize];
    Some(format!(
        "FREQ=YEARLY;BYMONTH={};BYDAY={ord}{wd}",
        first.local.month()
    ))
}

/// Build a VTIMEZONE for an IANA zone covering `from_year..=to_year`.
///
/// Regular DST zones get one RRULE per observance (valid beyond the range);
/// irregular ones get an explicit observance per transition.
pub fn vtimezone(tzid: &str, tz: Tz, from_year: i32, to_year: i32) -> Component {
    let mut vtz = Component::new("VTIMEZONE");
    vtz.props.push(Property::new("TZID", tzid));
    vtz.props.push(Property::new("X-LIC-LOCATION", tz.name()));
    let at = |t: NaiveDateTime| tz.offset_from_utc_datetime(&t);
    let key = |t: NaiveDateTime| (at(t).fix().local_minus_utc(), at(t).dst_offset());
    let begin = NaiveDate::from_ymd_opt(from_year, 1, 1)
        .unwrap_or_default()
        .and_time(chrono::NaiveTime::MIN);
    let end = NaiveDate::from_ymd_opt(to_year + 1, 1, 1)
        .unwrap_or_default()
        .and_time(chrono::NaiveTime::MIN);
    let mut transitions = Vec::new();
    let mut t = begin;
    while t < end {
        let next = t + Duration::days(1);
        if key(next) != key(t) {
            let (mut lo, mut hi) = (t, next);
            while hi - lo > Duration::seconds(1) {
                let mid = lo + (hi - lo) / 2;
                if key(mid) == key(lo) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let (from, off) = (key(lo).0, at(hi));
            transitions.push(Transition {
                local: hi + Duration::seconds(i64::from(from)),
                from,
                to: off.fix().local_minus_utc(),
                dst: !off.dst_offset().is_zero(),
                name: off.abbreviation().map(str::to_string),
            });
        }
        t = next;
    }
    let base = at(begin);
    let baseline = Transition {
        local: NaiveDate::from_ymd_opt(1970, 1, 1)
            .unwrap_or_default()
            .and_time(chrono::NaiveTime::MIN),
        from: base.fix().local_minus_utc(),
        to: base.fix().local_minus_utc(),
        dst: !base.dst_offset().is_zero(),
        name: base.abbreviation().map(str::to_string),
    };
    vtz.children.push(baseline.component(None));
    let years = usize::try_from(to_year - from_year + 1).unwrap_or(0);
    let (dst, std): (Vec<&Transition>, Vec<&Transition>) = transitions.iter().partition(|t| t.dst);
    match (yearly_rule(&dst, years), yearly_rule(&std, years)) {
        (Some(d), Some(s)) => {
            vtz.children.push(dst[0].component(Some(d)));
            vtz.children.push(std[0].component(Some(s)));
        }
        _ => vtz
            .children
            .extend(transitions.iter().map(|t| t.component(None))),
    }
    vtz
}

/// Windows zone names (CLDR windowsZones, territory 001) to IANA.
pub const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("Dateline Standard Time", "Etc/GMT+12"),
    ("UTC-11", "Etc/GMT+11"),
    ("Aleutian Standard Time", "America/Adak"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("Marquesas Standard Time", "Pacific/Marquesas"),
    ("Alaskan Standard Time", "America/Anchorage"),
    ("UTC-09", "Etc/GMT+9"),
    ("Pacific Standard Time (Mexico)", "America/Tijuana"),
    ("UTC-08", "Etc/GMT+8"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("Mountain Standard Time (Mexico)", "America/Mazatlan"),
    ("Mountain Standard Time", "America/Denver"),
    ("Yukon Standard Time", "America/Whitehorse"),
    ("Central America Standard Time", "America/Guatemala"),
    ("Central Standard Time", "America/Chicago"),
    ("Easter Island Standard Time", "Pacific/Easter"),
    ("Central Standard Time (Mexico)", "America/Mexico_City"),
    ("Mexico Standard Time", "America/Mexico_City"),
    ("Mexico Standard Time 2", "America/Chihuahua"),
    ("Canada Central Standard Time", "America/Regina"),
    ("SA Pacific Standard Time", "America/Bogota"),
    ("Eastern Standard Time (Mexico)", "America/Cancun"),
    ("Eastern Standard Time", "America/New_York"),
    ("Haiti Standard Time", "America/Port-au-Prince"),
    ("Cuba Standard Time", "America/Havana"),
    ("US Eastern Standard Time", "America/Indiana/Indianapolis"),
    ("Turks And Caicos Standard Time", "America/Grand_Turk"),
    ("Paraguay Standard Time", "America/Asuncion"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("Venezuela Standard Time", "America/Caracas"),
    ("Central Brazilian Standard Time", "America/Cuiaba"),
    ("SA Western Standard Time", "America/La_Paz"),
    ("Pacific SA Standard Time", "America/Santiago"),
    ("Newfoundland Standard Time", "America/St_Johns"),
    ("Tocantins Standard Time", "America/Araguaina"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("SA Eastern Standard Time", "America/Cayenne"),
    ("Argentina Standard Time", "America/Argentina/Buenos_Aires"),
    ("Greenland Standard Time", "America/Nuuk"),
    ("Montevideo Standard Time", "America/Montevideo"),
    ("Magallanes Standard Time", "America/Punta_Arenas"),
    ("Saint Pierre Standard Time", "America/Miquelon"),
    ("Bahia Standard Time", "America/Bahia"),
    ("UTC-02", "Etc/GMT+2"),
    ("Mid-Atlantic Standard Time", "Etc/GMT+2"),
    ("Azores Standard Time", "Atlantic/Azores"),
    ("Cape Verde Standard Time", "Atlantic/Cape_Verde"),
    ("UTC", "Etc/UTC"),
    ("Coordinated Universal Time", "Etc/UTC"),
    ("GMT Standard Time", "Europe/London"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("Sao Tome Standard Time", "Africa/Sao_Tome"),
    ("Morocco Standard Time", "Africa/Casablanca"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("W. Central Africa Standard Time", "Africa/Lagos"),
    ("Jordan Standard Time", "Asia/Amman"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("Middle East Standard Time", "Asia/Beirut"),
    ("Egypt Standard Time", "Africa/Cairo"),
    ("E. Europe Standard Time", "Europe/Chisinau"),
    ("Syria Standard Time", "Asia/Damascus"),
    ("West Bank Standard Time", "Asia/Hebron"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("FLE Standard Time", "Europe/Kyiv"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("Jerusalem Standard Time", "Asia/Jerusalem"),
    ("South Sudan Standard Time", "Africa/Juba"),
    ("Kaliningrad Standard Time", "Europe/Kaliningrad"),
    ("Sudan Standard Time", "Africa/Khartoum"),
    ("Libya Standard Time", "Africa/Tripoli"),
    ("Namibia Standard Time", "Africa/Windhoek"),
    ("Arabic Standard Time", "Asia/Baghdad"),
    ("Turkey Standard Time", "Europe/Istanbul"),
    ("Arab Standard Time", "Asia/Riyadh"),
    ("Belarus Standard Time", "Europe/Minsk"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("E. Africa Standard Time", "Africa/Nairobi"),
    ("Volgograd Standard Time", "Europe/Volgograd"),
    ("Iran Standard Time", "Asia/Tehran"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("Astrakhan Standard Time", "Europe/Astrakhan"),
    ("Azerbaijan Standard Time", "Asia/Baku"),
    ("Russia Time Zone 3", "Europe/Samara"),
    ("Mauritius Standard Time", "Indian/Mauritius"),
    ("Saratov Standard Time", "Europe/Saratov"),
    ("Georgian Standard Time", "Asia/Tbilisi"),
    ("Caucasus Standard Time", "Asia/Yerevan"),
    ("Armenian Standard Time", "Asia/Yerevan"),
    ("Afghanistan Standard Time", "Asia/Kabul"),
    ("West Asia Standard Time", "Asia/Tashkent"),
    ("Ekaterinburg Standard Time", "Asia/Yekaterinburg"),
    ("Pakistan Standard Time", "Asia/Karachi"),
    ("Qyzylorda Standard Time", "Asia/Qyzylorda"),
    ("India Standard Time", "Asia/Kolkata"),
    ("Sri Lanka Standard Time", "Asia/Colombo"),
    ("Nepal Standard Time", "Asia/Kathmandu"),
    ("Central Asia Standard Time", "Asia/Almaty"),
    ("Bangladesh Standard Time", "Asia/Dhaka"),
    ("Omsk Standard Time", "Asia/Omsk"),
    ("Myanmar Standard Time", "Asia/Yangon"),
    ("SE Asia Standard Time", "Asia/Bangkok"),
    ("Altai Standard Time", "Asia/Barnaul"),
    ("W. Mongolia Standard Time", "Asia/Hovd"),
    ("North Asia Standard Time", "Asia/Krasnoyarsk"),
    ("N. Central Asia Standard Time", "Asia/Novosibirsk"),
    ("Tomsk Standard Time", "Asia/Tomsk"),
    ("China Standard Time", "Asia/Shanghai"),
    ("North Asia East Standard Time", "Asia/Irkutsk"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("Malay Peninsula Standard Time", "Asia/Kuala_Lumpur"),
    ("W. Australia Standard Time", "Australia/Perth"),
    ("Taipei Standard Time", "Asia/Taipei"),
    ("Ulaanbaatar Standard Time", "Asia/Ulaanbaatar"),
    ("Aus Central W. Standard Time", "Australia/Eucla"),
    ("Transbaikal Standard Time", "Asia/Chita"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("North Korea Standard Time", "Asia/Pyongyang"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("Yakutsk Standard Time", "Asia/Yakutsk"),
    ("Cen. Australia Standard Time", "Australia/Adelaide"),
    ("AUS Central Standard Time", "Australia/Darwin"),
    ("E. Australia Standard Time", "Australia/Brisbane"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("West Pacific Standard Time", "Pacific/Port_Moresby"),
    ("Tasmania Standard Time", "Australia/Hobart"),
    ("Vladivostok Standard Time", "Asia/Vladivostok"),
    ("Lord Howe Standard Time", "Australia/Lord_Howe"),
    ("Bougainville Standard Time", "Pacific/Bougainville"),
    ("Russia Time Zone 10", "Asia/Srednekolymsk"),
    ("Magadan Standard Time", "Asia/Magadan"),
    ("Norfolk Standard Time", "Pacific/Norfolk"),
    ("Sakhalin Standard Time", "Asia/Sakhalin"),
    ("Central Pacific Standard Time", "Pacific/Guadalcanal"),
    ("Russia Time Zone 11", "Asia/Kamchatka"),
    ("Kamchatka Standard Time", "Asia/Kamchatka"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
    ("UTC+12", "Etc/GMT-12"),
    ("Fiji Standard Time", "Pacific/Fiji"),
    ("Chatham Islands Standard Time", "Pacific/Chatham"),
    ("UTC+13", "Etc/GMT-13"),
    ("Tonga Standard Time", "Pacific/Tongatapu"),
    ("Samoa Standard Time", "Pacific/Apia"),
    ("Line Islands Standard Time", "Pacific/Kiritimati"),
];

/// Keywords of Outlook display names like `(UTC+01:00) Amsterdam, Berlin, …`.
const DISPLAY_NAMES: &[(&str, &str)] = &[
    ("Amsterdam", "Europe/Berlin"),
    ("Brussels", "Europe/Paris"),
    ("Sarajevo", "Europe/Warsaw"),
    ("Belgrade", "Europe/Budapest"),
    ("Dublin", "Europe/London"),
    ("London", "Europe/London"),
    ("Athens", "Europe/Bucharest"),
    ("Helsinki", "Europe/Kyiv"),
    ("Istanbul", "Europe/Istanbul"),
    ("Moscow", "Europe/Moscow"),
    ("Cairo", "Africa/Cairo"),
    ("Jerusalem", "Asia/Jerusalem"),
    ("Kuwait", "Asia/Riyadh"),
    ("Baghdad", "Asia/Baghdad"),
    ("Abu Dhabi", "Asia/Dubai"),
    ("Tehran", "Asia/Tehran"),
    ("Islamabad", "Asia/Karachi"),
    ("Chennai", "Asia/Kolkata"),
    ("Bangkok", "Asia/Bangkok"),
    ("Beijing", "Asia/Shanghai"),
    ("Singapore", "Asia/Singapore"),
    ("Osaka", "Asia/Tokyo"),
    ("Seoul", "Asia/Seoul"),
    ("Canberra", "Australia/Sydney"),
    ("Auckland", "Pacific/Auckland"),
    ("Brasilia", "America/Sao_Paulo"),
    ("Buenos Aires", "America/Argentina/Buenos_Aires"),
    ("Atlantic Time", "America/Halifax"),
    ("Eastern Time", "America/New_York"),
    ("Central Time", "America/Chicago"),
    ("Mountain Time", "America/Denver"),
    ("Arizona", "America/Phoenix"),
    ("Pacific Time", "America/Los_Angeles"),
    ("Alaska", "America/Anchorage"),
    ("Hawaii", "Pacific/Honolulu"),
    ("Coordinated Universal Time", "Etc/UTC"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").unwrap()
    }

    fn utc(z: &Zone, local: &str) -> String {
        DateTime::from_timestamp_millis(z.utc_ms(at(local)))
            .unwrap()
            .format("%Y-%m-%dT%H:%MZ")
            .to_string()
    }

    fn rules(vtimezone: &str) -> Zone {
        let cal = Component::parse(&format!("BEGIN:VCALENDAR\n{vtimezone}\nEND:VCALENDAR")).unwrap();
        let z = zone_from_vtimezone(&cal.children[0]).unwrap();
        assert!(matches!(z, Zone::Rules(_)), "no location hints: rules are used");
        z
    }

    #[test]
    fn resolves_windows_and_outlook_names() {
        let ny = Some(chrono_tz::America::New_York);
        assert_eq!(resolve_tzid("Eastern Daylight Time"), ny);
        assert_eq!(resolve_tzid("(UTC-05:00) Eastern Time (US & Canada)"), ny);
        assert_eq!(resolve_tzid("/mozilla.org/20050126_1/America/New_York"), ny);
        assert_eq!(resolve_tzid("Customized Time Zone"), None);
        assert_eq!(resolve_tzid(""), None);
    }

    #[test]
    fn ambiguous_and_missing_wall_times() {
        let ny = Zone::Tz(chrono_tz::America::New_York);
        // The first of the two 01:30s; a time in the gap uses the offset before it.
        assert_eq!(utc(&ny, "2024-11-03T01:30"), "2024-11-03T05:30Z");
        assert_eq!(utc(&ny, "2024-03-10T02:30"), "2024-03-10T07:30Z");
    }

    #[test]
    fn custom_vtimezone_rules() {
        let sydney = rules(
            "BEGIN:VTIMEZONE\nTZID:Customized Time Zone\n\
BEGIN:STANDARD\nDTSTART:20080406T030000\nTZOFFSETFROM:+1100\nTZOFFSETTO:+1000\nRRULE:FREQ=YEARLY;BYMONTH=4;BYDAY=1SU\nEND:STANDARD\n\
BEGIN:DAYLIGHT\nDTSTART:20081005T020000\nTZOFFSETFROM:+1000\nTZOFFSETTO:+1100\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=1SU\nEND:DAYLIGHT\nEND:VTIMEZONE",
        );
        assert_eq!(utc(&sydney, "2024-01-15T09:00"), "2024-01-14T22:00Z");
        assert_eq!(utc(&sydney, "2024-07-15T09:00"), "2024-07-14T23:00Z");
        assert_eq!(utc(&sydney, "2024-04-07T02:30"), "2024-04-06T15:30Z");
        assert_eq!(utc(&sydney, "2024-10-06T02:30"), "2024-10-05T16:30Z");
        assert_eq!(sydney.local(sydney.utc_ms(at("2024-07-15T09:00"))), at("2024-07-15T09:00"));

        // Outlook writes its rules as starting in 1601.
        let eastern = rules(
            "BEGIN:VTIMEZONE\nTZID:Customized Time Zone 1\n\
BEGIN:STANDARD\nDTSTART:16010101T020000\nTZOFFSETFROM:-0400\nTZOFFSETTO:-0500\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=11\nEND:STANDARD\n\
BEGIN:DAYLIGHT\nDTSTART:16010101T020000\nTZOFFSETFROM:-0500\nTZOFFSETTO:-0400\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=2SU;BYMONTH=3\nEND:DAYLIGHT\nEND:VTIMEZONE",
        );
        assert_eq!(utc(&eastern, "2024-01-15T12:00"), "2024-01-15T17:00Z");
        assert_eq!(utc(&eastern, "2024-07-01T12:00"), "2024-07-01T16:00Z");
        assert_eq!(utc(&eastern, "2024-03-10T03:30"), "2024-03-10T07:30Z");
        assert_eq!(utc(&eastern, "2024-11-03T01:30"), "2024-11-03T05:30Z");
    }

    #[test]
    fn generated_vtimezones_match_the_zone_database() {
        for name in [
            "Europe/Berlin",
            "Australia/Sydney",
            "Asia/Riyadh",
            "America/Sao_Paulo",
            "Asia/Tehran",
            "Pacific/Chatham",
            "Asia/Kathmandu",
        ] {
            let tz: Tz = name.parse().unwrap();
            let vtz = vtimezone(name, tz, 2023, 2027);
            let ours = Zone::Rules(Arc::new(VtzRules::from_component(&vtz).unwrap()));
            let theirs = Zone::Tz(tz);
            let mut t = at("2023-01-01T00:00");
            while t < at("2028-01-01T00:00") {
                let ms = t.and_utc().timestamp_millis();
                assert_eq!(ours.local(ms), theirs.local(ms), "{name} at {t}Z");
                for wall in [t + Duration::minutes(30), t + Duration::minutes(45)] {
                    assert_eq!(ours.utc_ms(wall), theirs.utc_ms(wall), "{name} at {wall} local");
                }
                t += Duration::hours(1);
            }
        }
    }

    #[test]
    fn offsets() {
        assert_eq!(parse_offset("+0545"), Some(20700));
        assert_eq!(parse_offset("-0330"), Some(-12600));
        assert_eq!(parse_offset("+053"), None);
        assert_eq!(parse_offset("ü000"), None);
    }

    #[test]
    fn far_instants_are_clamped() {
        let z = Zone::Tz(Tz::UTC);
        assert_eq!(z.local(i64::MAX).year(), 9999);
        assert_eq!(z.local(i64::MIN).year(), 1);
    }
}
