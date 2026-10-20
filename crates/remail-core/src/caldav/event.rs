//! Events on top of the iCalendar DOM: recurrence expansion into concrete
//! occurrences, and creating / editing VEVENTs while keeping unknown data.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, Result};
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::caldav::ical::{Component, Property};
use crate::caldav::recur::{Freq, Recur, Until, MAX_PERIODS};
use crate::caldav::tz::{resolve_tzid, vtimezone, zone_from_vtimezone, Zone};

/// Maximum occurrences returned by one [`occurrences`] call.
pub const MAX_INSTANCES: usize = 3000;
/// Generation cap when walking a whole series (e.g. for [`object_span`]).
const MAX_SERIES: usize = 100_000;
const DAY_MS: i64 = 86_400_000;
/// Widest UTC offset: DATE and floating values can land this far either way
/// of their UTC reading, depending on the zone they are shown in.
const MAX_OFFSET_MS: i64 = 14 * 3_600_000;
/// Absolute VALARM triggers further ahead of the start are ignored.
const MAX_ABSOLUTE_LEAD_MINUTES: i64 = 366 * 1440;
/// Bound on duration components so date arithmetic cannot overflow.
const MAX_DURATION_UNITS: i64 = 10_000_000;
const PRODID: &str = "-//Remail//Remail Calendar//EN";

/// One concrete instance of an event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Occurrence {
    pub uid: String,
    /// Stable key of this instance: the RECURRENCE-ID / original start as epoch ms; None for non-recurring events.
    pub recurrence_id: Option<i64>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub all_day: bool,
    pub recurring: bool,
    pub summary: String,
    pub location: String,
    pub description: String,
    pub url: String,
    /// `""`, `TENTATIVE`, `CONFIRMED` or `CANCELLED`.
    pub status: String,
    /// Reminder offsets in minutes before the start (negative = after).
    pub alarms_minutes_before: Vec<i64>,
    pub organizer: Option<String>,
    pub attendees: Vec<Attendee>,
    /// The master's RRULE value, None if not recurring.
    pub rrule: Option<String>,
}

/// An ATTENDEE of an event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Attendee {
    pub email: String,
    pub name: String,
    pub partstat: String,
}

/// Fields the editor sets on an event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventInput {
    pub summary: String,
    pub location: String,
    pub description: String,
    /// For all-day events: local midnights, end exclusive.
    pub start_ms: i64,
    pub end_ms: i64,
    pub all_day: bool,
    /// IANA zone of the user, e.g. `Asia/Riyadh`.
    pub tz: String,
    /// e.g. `FREQ=WEEKLY;BYDAY=MO` (None = single).
    pub rrule: Option<String>,
    pub alarms_minutes_before: Vec<i64>,
}

/// Which part of a recurring event an edit applies to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EditScope {
    All,
    #[serde(rename_all = "camelCase")]
    This {
        recurrence_id: i64,
    },
}

/// A DATE or DATE-TIME value.
#[derive(Debug, Clone)]
enum Dt {
    Date(NaiveDate),
    Floating(NaiveDateTime),
    Utc(NaiveDateTime),
    Zoned(NaiveDateTime, Zone, String),
}

impl Dt {
    fn naive(&self) -> NaiveDateTime {
        match self {
            Dt::Date(d) => d.and_time(NaiveTime::MIN),
            Dt::Floating(t) | Dt::Utc(t) | Dt::Zoned(t, _, _) => *t,
        }
    }

    fn is_date(&self) -> bool {
        matches!(self, Dt::Date(_))
    }

    /// The zone this value's wall-clock time lives in; floating and DATE use `local`.
    fn zone(&self, local: &Zone) -> Zone {
        match self {
            Dt::Date(_) | Dt::Floating(_) => local.clone(),
            Dt::Utc(_) => Zone::Tz(Tz::UTC),
            Dt::Zoned(_, z, _) => z.clone(),
        }
    }

    fn ms(&self, local: &Zone) -> i64 {
        self.zone(local).utc_ms(self.naive())
    }
}

/// How a date-time is written back.
#[derive(Debug, Clone)]
enum Form {
    Date,
    Utc,
    Floating,
    Zoned(String, Zone),
}

impl Form {
    fn of(dt: &Dt) -> Form {
        match dt {
            Dt::Date(_) => Form::Date,
            Dt::Utc(_) => Form::Utc,
            Dt::Floating(_) => Form::Floating,
            Dt::Zoned(_, z, id) => Form::Zoned(id.clone(), z.clone()),
        }
    }

    /// Format an instant; DATE and floating values use `local`.
    fn value(&self, ms: i64, local: &Zone) -> String {
        match self {
            Form::Date => local.local(ms).format("%Y%m%d").to_string(),
            Form::Utc => Zone::Tz(Tz::UTC)
                .local(ms)
                .format("%Y%m%dT%H%M%SZ")
                .to_string(),
            Form::Floating => local.local(ms).format("%Y%m%dT%H%M%S").to_string(),
            Form::Zoned(_, z) => z.local(ms).format("%Y%m%dT%H%M%S").to_string(),
        }
    }

    fn prop(&self, name: &str, ms: i64, local: &Zone) -> Property {
        let p = Property::new(name, &self.value(ms, local));
        match self {
            Form::Date => p.with_param("VALUE", "DATE"),
            Form::Zoned(id, _) => p.with_param("TZID", id),
            _ => p,
        }
    }
}

/// Resolution context for one calendar object.
struct Ctx<'a> {
    cal: &'a Component,
    local: Zone,
    /// TZID → zone, so every value of a zone shares one (rule-caching) zone.
    zones: RefCell<HashMap<String, Option<Zone>>>,
}

impl<'a> Ctx<'a> {
    fn new(cal: &'a Component, local: Zone) -> Ctx<'a> {
        Ctx {
            cal,
            local,
            zones: RefCell::new(HashMap::new()),
        }
    }

    /// A zone for a TZID if it can be resolved by name or VTIMEZONE.
    fn known_zone(&self, tzid: &str) -> Option<Zone> {
        if let Some(z) = self.zones.borrow().get(tzid) {
            return z.clone();
        }
        let zone = resolve_tzid(tzid).map(Zone::Tz).or_else(|| {
            let id = tzid.trim().trim_matches('"');
            self.cal
                .children_named("VTIMEZONE")
                .find(|v| {
                    v.prop("TZID")
                        .is_some_and(|p| p.value.trim().trim_matches('"') == id)
                })
                .and_then(zone_from_vtimezone)
        });
        self.zones
            .borrow_mut()
            .insert(tzid.to_string(), zone.clone());
        zone
    }

    fn zone(&self, tzid: &str) -> Zone {
        self.known_zone(tzid).unwrap_or_else(|| self.local.clone())
    }

    /// All values of a (possibly comma-separated) date property; PERIODs yield their start.
    fn values(&self, p: &Property) -> Vec<Dt> {
        let is_date = p
            .param("VALUE")
            .is_some_and(|v| v.eq_ignore_ascii_case("DATE"));
        let tz = p.param("TZID").map(|id| (self.zone(id), id));
        p.value
            .split(',')
            .filter_map(|v| {
                let v = v
                    .split('/')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .replace(['-', ':'], "");
                parse_dt_value(&v, is_date, tz.clone())
            })
            .collect()
    }

    fn dt(&self, ev: &Component, name: &str) -> Option<Dt> {
        ev.prop(name)
            .and_then(|p| self.values(p).into_iter().next())
    }
}

fn parse_dt_value(v: &str, is_date: bool, tz: Option<(Zone, &str)>) -> Option<Dt> {
    if is_date || v.len() == 8 {
        return NaiveDate::parse_from_str(v.get(..8)?, "%Y%m%d")
            .ok()
            .map(Dt::Date);
    }
    if let Some(utc) = v.strip_suffix(['Z', 'z']) {
        return parse_naive(utc).map(Dt::Utc);
    }
    let t = parse_naive(v)?;
    Some(match tz {
        Some((z, id)) => Dt::Zoned(t, z, id.trim_matches('"').to_string()),
        None => Dt::Floating(t),
    })
}

fn parse_naive(v: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M"))
        .ok()
}

/// An iCalendar duration (`PT1H`, `P1D`) in seconds, counting a day as 24 hours.
pub fn duration_secs(v: &str) -> Option<i64> {
    parse_duration(v).map(|d| d.days * 86_400 + d.secs)
}

/// A nominal duration (`P1W2DT3H`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Dur {
    days: i64,
    secs: i64,
}

fn parse_duration(v: &str) -> Option<Dur> {
    let v = v.trim();
    let (neg, v) = match v.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, v.strip_prefix('+').unwrap_or(v)),
    };
    let v = v.strip_prefix(['P', 'p'])?;
    let (mut days, mut secs, mut num, mut in_time, mut any) =
        (0i64, 0i64, String::new(), false, false);
    for c in v.chars() {
        match c.to_ascii_uppercase() {
            '0'..='9' => num.push(c),
            'T' => in_time = true,
            unit => {
                let n: i64 = num.parse().ok().filter(|n| *n <= MAX_DURATION_UNITS)?;
                num.clear();
                any = true;
                match (unit, in_time) {
                    ('W', _) => days += n * 7,
                    ('D', _) => days += n,
                    ('H', true) => secs += n * 3600,
                    ('M', true) => secs += n * 60,
                    ('S', true) => secs += n,
                    _ => return None,
                }
                if days > MAX_DURATION_UNITS || secs > MAX_DURATION_UNITS * 100 {
                    return None;
                }
            }
        }
    }
    if !any {
        return None;
    }
    Some(if neg {
        Dur {
            days: -days,
            secs: -secs,
        }
    } else {
        Dur { days, secs }
    })
}

fn format_minutes_trigger(minutes_before: i64) -> String {
    let (sign, m) = if minutes_before >= 0 {
        ("-", minutes_before)
    } else {
        ("", -minutes_before)
    };
    match m {
        0 => "PT0S".into(),
        m if m % 1440 == 0 => format!("{sign}P{}D", m / 1440),
        m => format!("{sign}PT{m}M"),
    }
}

/// Event length in the form needed to place each instance.
#[derive(Debug, Clone, Copy)]
enum Span {
    Days(i64),
    Exact(i64),
    Nominal(Dur),
}

/// One expanded instance (before overrides).
#[derive(Debug, Clone, Copy)]
struct Inst {
    naive: NaiveDateTime,
    start_ms: i64,
    end_ms: i64,
}

/// The recurrence set of a master VEVENT.
struct Series {
    start: Dt,
    zone: Zone,
    span: Span,
    /// The first RRULE (further ones are ignored); none for sub-hourly rules.
    rule: Option<Recur>,
    rdates: Vec<NaiveDateTime>,
    ex_ms: HashSet<i64>,
    ex_dates: HashSet<NaiveDate>,
    /// Periods the rule may still walk, over every expansion of this series.
    budget: Cell<u32>,
}

/// A RECURRENCE-ID or EXDATE as (epoch ms, DATE if it is one).
type RidPoint = (i64, Option<NaiveDate>);

/// Starts of a series' instances (ignoring overrides) around some RECURRENCE-IDs.
struct InstanceIndex {
    ms: HashSet<i64>,
    dates: HashSet<NaiveDate>,
}

impl InstanceIndex {
    fn contains(&self, rid_ms: i64, rid_date: Option<NaiveDate>) -> bool {
        self.ms.contains(&rid_ms) || rid_date.is_some_and(|d| self.dates.contains(&d))
    }
}

impl Series {
    fn new(ctx: &Ctx, ev: &Component, with_exdates: bool) -> Option<Series> {
        let start = ctx
            .dt(ev, "DTSTART")
            .or_else(|| ctx.dt(ev, "RECURRENCE-ID"))?;
        let zone = start.zone(&ctx.local);
        let span = event_span(ctx, ev, &start);
        // Every-second / every-minute rules aren't calendar events; showing
        // just the first is safer than walking millions of instances.
        let rule = ev
            .prop("RRULE")
            .and_then(|p| Recur::parse(&p.value).ok())
            .filter(|r| r.freq >= Freq::Hourly);
        let to_naive = |dt: &Dt| match dt {
            Dt::Date(d) => d.and_time(NaiveTime::MIN),
            Dt::Floating(t) => *t,
            other => zone.local(other.ms(&ctx.local)),
        };
        let rdates = ev
            .props("RDATE")
            .flat_map(|p| ctx.values(p))
            .map(|d| to_naive(&d))
            .collect();
        let (mut ex_ms, mut ex_dates) = (HashSet::new(), HashSet::new());
        if with_exdates {
            for d in ev.props("EXDATE").flat_map(|p| ctx.values(p)) {
                match d {
                    Dt::Date(date) => {
                        ex_dates.insert(date);
                    }
                    Dt::Floating(t) => {
                        ex_ms.insert(zone.utc_ms(t));
                    }
                    other => {
                        ex_ms.insert(other.ms(&ctx.local));
                    }
                }
            }
        }
        Some(Series {
            start,
            zone,
            span,
            rule,
            rdates,
            ex_ms,
            ex_dates,
            budget: Cell::new(MAX_PERIODS),
        })
    }

    fn recurring(&self) -> bool {
        self.rule.is_some() || !self.rdates.is_empty()
    }

    fn infinite(&self) -> bool {
        self.rule.as_ref().is_some_and(Recur::is_infinite)
    }

    /// Has the rule walked as far as this series may (so its list may be cut short)?
    fn exhausted(&self) -> bool {
        self.budget.get() == 0
    }

    /// DATE and floating series move with the zone they are shown in.
    fn floats(&self) -> bool {
        matches!(self.start, Dt::Date(_) | Dt::Floating(_))
    }

    fn place(&self, naive: NaiveDateTime) -> Inst {
        let start_ms = self.zone.utc_ms(naive);
        let end_ms = match self.span {
            Span::Days(n) => self.zone.utc_ms(add_days(naive, n)),
            Span::Exact(ms) => start_ms + ms,
            Span::Nominal(d) => self.zone.utc_ms(add_days(naive, d.days)) + d.secs * 1000,
        };
        Inst {
            naive,
            start_ms,
            end_ms,
        }
    }

    fn excluded(&self, inst: &Inst) -> bool {
        self.ex_ms.contains(&inst.start_ms) || self.ex_dates.contains(&inst.naive.date())
    }

    fn until_naive(&self, u: Until) -> NaiveDateTime {
        match u {
            Until::Date(d) if self.start.is_date() => d.and_time(NaiveTime::MIN),
            Until::Date(d) => d.and_hms_opt(23, 59, 59).unwrap_or_default(),
            Until::Utc(t) => self.zone.local(t.and_utc().timestamp_millis()),
            Until::Local(t) => t,
        }
    }

    /// Instances overlapping `[from, to)` (all if `None`), sorted, at most `cap`.
    fn instances(&self, range: Option<(i64, i64)>, cap: usize) -> Vec<Inst> {
        let window = range.map(|(from, to)| {
            let longest = match self.span {
                Span::Days(n) => n * DAY_MS,
                Span::Exact(ms) => ms,
                Span::Nominal(d) => d.days * DAY_MS + d.secs * 1000,
            };
            let ws = self.zone.local(from.saturating_sub(longest.max(0))) - Duration::days(2);
            let we = self.zone.local(to) + Duration::days(2);
            (ws, we)
        });
        let start = self.start.naive();
        let mut naives: Vec<NaiveDateTime> = vec![start];
        if let Some(rule) = &self.rule {
            let until = rule.until.map(|u| self.until_naive(u));
            let mut seen = 0usize;
            let mut budget = self.budget.get();
            rule.expand_within(start, until, window, &mut budget, |t| {
                seen += 1;
                if window.is_none_or(|(ws, _)| t >= ws) {
                    naives.push(if self.start.is_date() {
                        t.date().and_time(NaiveTime::MIN)
                    } else {
                        t
                    });
                }
                seen < MAX_SERIES
            });
            self.budget.set(budget);
        }
        naives.extend(self.rdates.iter().copied());
        naives.sort();
        naives.dedup();
        let mut out = Vec::new();
        let mut last_start = None;
        for n in naives {
            let inst = self.place(n);
            // Two wall-clock times can be one instant (02:00 and 03:00 when the
            // clocks spring forward): keep one, so instance keys stay unique.
            if last_start.replace(inst.start_ms) == Some(inst.start_ms) {
                continue;
            }
            if self.excluded(&inst) {
                continue;
            }
            if let Some((from, to)) = range {
                if !overlaps(inst.start_ms, inst.end_ms, from, to) {
                    continue;
                }
            }
            out.push(inst);
            if out.len() >= cap {
                break;
            }
        }
        out
    }

    /// The instances (ignoring overrides) around the RECURRENCE-IDs `rids`
    /// (epoch ms), expanded once for all of them.
    fn index(&self, rids: &[i64]) -> InstanceIndex {
        let mut index = InstanceIndex {
            ms: HashSet::new(),
            dates: HashSet::new(),
        };
        let (Some(lo), Some(hi)) = (rids.iter().min(), rids.iter().max()) else {
            return index;
        };
        let range = (lo.saturating_sub(DAY_MS), hi.saturating_add(DAY_MS));
        for i in self.instances(Some(range), MAX_SERIES) {
            index.ms.insert(i.start_ms);
            index.dates.insert(i.naive.date());
        }
        index
    }
}

/// `t + days`, saturating at the representable range.
fn add_days(t: NaiveDateTime, days: i64) -> NaiveDateTime {
    Duration::try_days(days)
        .and_then(|d| t.checked_add_signed(d))
        .unwrap_or(t)
}

fn overlaps(start: i64, end: i64, from: i64, to: i64) -> bool {
    start < to && (end > from || (end <= start && start >= from))
}

/// Length of an event from DTEND / DURATION (RFC 5545 defaults when missing).
fn event_span(ctx: &Ctx, ev: &Component, start: &Dt) -> Span {
    if let Some(end) = ctx.dt(ev, "DTEND") {
        return match (start, &end) {
            (Dt::Date(s), Dt::Date(e)) => Span::Days((*e - *s).num_days().max(1)),
            (Dt::Date(_), _) => Span::Days(1),
            _ => Span::Exact((end.ms(&ctx.local) - start.ms(&ctx.local)).max(0)),
        };
    }
    if let Some(d) = ev.prop("DURATION").and_then(|p| parse_duration(&p.value)) {
        return match start {
            Dt::Date(_) => Span::Days((d.days + d.secs / 86_400).max(1)),
            _ if d.days < 0 || d.secs < 0 => Span::Exact(0),
            _ => Span::Nominal(d),
        };
    }
    if start.is_date() {
        Span::Days(1)
    } else {
        Span::Exact(0)
    }
}

/// UID of the object (of its first VEVENT).
pub fn object_uid(cal: &Component) -> Option<String> {
    let comp = cal
        .children_named("VEVENT")
        .next()
        .or_else(|| cal.children.iter().find(|c| c.name != "VTIMEZONE"))?;
    comp.prop("UID")
        .map(|p| p.text().trim().to_string())
        .filter(|s| !s.is_empty())
}

/// VEVENTs grouped by UID: (uid, master, overrides).
fn groups(cal: &Component) -> Vec<(String, Option<&Component>, Vec<&Component>)> {
    let mut out: Vec<(String, Option<&Component>, Vec<&Component>)> = Vec::new();
    for ev in cal.children_named("VEVENT") {
        let uid = ev
            .prop("UID")
            .map(|p| p.text().trim().to_string())
            .unwrap_or_default();
        let idx = match out.iter().position(|(u, _, _)| *u == uid) {
            Some(i) => i,
            None => {
                out.push((uid, None, Vec::new()));
                out.len() - 1
            }
        };
        let group = &mut out[idx];
        if ev.prop("RECURRENCE-ID").is_some() {
            group.2.push(ev);
        } else if group.1.is_none() {
            group.1 = Some(ev);
        }
    }
    out
}

fn status_of(ev: &Component) -> String {
    ev.prop("STATUS")
        .map(|p| p.text().trim().to_ascii_uppercase())
        .unwrap_or_default()
}

fn strip_mailto(v: &str) -> String {
    let v = v.trim();
    match v.get(..7) {
        Some(p) if p.eq_ignore_ascii_case("mailto:") => v[7..].to_string(),
        _ => v.to_string(),
    }
}

/// Reminder offsets (minutes before start) of an event's VALARMs.
fn alarms(ev: &Component, start_ms: i64, end_ms: i64) -> Vec<i64> {
    let mut out = Vec::new();
    for alarm in ev.children_named("VALARM") {
        // Apple's placeholder (`ACTION:NONE`, triggering in 1976) is no reminder.
        if alarm
            .prop("ACTION")
            .is_some_and(|a| a.value.trim().eq_ignore_ascii_case("NONE"))
        {
            continue;
        }
        let Some(trigger) = alarm.prop("TRIGGER") else {
            continue;
        };
        let is_abs = trigger
            .param("VALUE")
            .is_some_and(|v| v.eq_ignore_ascii_case("DATE-TIME"))
            || (!trigger
                .value
                .trim_start_matches(['-', '+'])
                .starts_with(['P', 'p'])
                && trigger.value.len() >= 15);
        let minutes = if is_abs {
            match parse_dt_value(trigger.value.trim(), false, None) {
                Some(Dt::Utc(t)) | Some(Dt::Floating(t)) => {
                    let m = (start_ms - t.and_utc().timestamp_millis()) / 60_000;
                    // A fixed time over a year ahead of the event isn't a reminder for it.
                    if m > MAX_ABSOLUTE_LEAD_MINUTES {
                        continue;
                    }
                    m
                }
                _ => continue,
            }
        } else {
            let Some(d) = parse_duration(&trigger.value) else {
                continue;
            };
            let offset_ms = (d.days * 86_400 + d.secs) * 1000;
            let related_end = trigger
                .param("RELATED")
                .is_some_and(|v| v.eq_ignore_ascii_case("END"));
            let base = if related_end { end_ms } else { start_ms };
            (start_ms - (base + offset_ms)) / 60_000
        };
        if !out.contains(&minutes) {
            out.push(minutes);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn make_occurrence(
    ev: &Component,
    uid: &str,
    recurrence_id: Option<i64>,
    start_ms: i64,
    end_ms: i64,
    all_day: bool,
    recurring: bool,
    rrule: Option<String>,
) -> Occurrence {
    let text = |name: &str| ev.prop(name).map(|p| p.text()).unwrap_or_default();
    Occurrence {
        uid: uid.to_string(),
        recurrence_id,
        start_ms,
        end_ms,
        all_day,
        recurring,
        summary: text("SUMMARY"),
        location: text("LOCATION"),
        description: text("DESCRIPTION"),
        url: ev
            .prop("URL")
            .map(|p| p.value.trim().to_string())
            .unwrap_or_default(),
        status: status_of(ev),
        alarms_minutes_before: alarms(ev, start_ms, end_ms),
        organizer: ev
            .prop("ORGANIZER")
            .map(|p| strip_mailto(&p.value))
            .filter(|s| !s.is_empty()),
        attendees: ev
            .props("ATTENDEE")
            .map(|p| Attendee {
                email: strip_mailto(&p.value),
                name: p.param("CN").unwrap_or("").to_string(),
                partstat: p
                    .param("PARTSTAT")
                    .unwrap_or("NEEDS-ACTION")
                    .to_ascii_uppercase(),
            })
            .collect(),
        rrule,
    }
}

/// An override's RECURRENCE-ID as (epoch ms, DATE if it is one).
fn rid_of(ctx: &Ctx, series_zone: &Zone, ev: &Component) -> Option<(i64, Option<NaiveDate>)> {
    let rid = ctx.dt(ev, "RECURRENCE-ID")?;
    Some(match rid {
        Dt::Date(d) => (series_zone.utc_ms(d.and_time(NaiveTime::MIN)), Some(d)),
        Dt::Floating(t) => (series_zone.utc_ms(t), None),
        other => (other.ms(&ctx.local), None),
    })
}

/// All instances of every VEVENT in one calendar object overlapping `[from_ms, to_ms)`.
///
/// RRULE/RDATE/EXDATE are applied, RECURRENCE-ID overrides replace their
/// instances (including ones moved into or out of the range) and cancelled
/// instances are dropped. DATE values and floating times use `local`.
pub fn occurrences(
    cal: &Component,
    from_ms: i64,
    to_ms: i64,
    local: Tz,
) -> Result<Vec<Occurrence>> {
    let ctx = Ctx::new(cal, Zone::Tz(local));
    let mut out = Vec::new();
    for (uid, master, overrides) in groups(cal) {
        let series = master.and_then(|m| Series::new(&ctx, m, true));
        let series_zone = series
            .as_ref()
            .map(|s| s.zone.clone())
            .unwrap_or_else(|| ctx.local.clone());
        let rrule = master
            .and_then(|m| m.prop("RRULE"))
            .map(|p| p.value.trim().to_string());
        let rids: Vec<(i64, Option<NaiveDate>, &Component)> = overrides
            .iter()
            .filter_map(|ev| rid_of(&ctx, &series_zone, ev).map(|(ms, d)| (ms, d, *ev)))
            .collect();
        if let (Some(m), Some(s)) = (master, &series) {
            if status_of(m) != "CANCELLED" {
                let recurring = s.recurring();
                let cap = MAX_INSTANCES.saturating_sub(out.len());
                for inst in s.instances(Some((from_ms, to_ms)), cap) {
                    let overridden = rids.iter().any(|(ms, d, _)| {
                        *ms == inst.start_ms || d.is_some_and(|d| d == inst.naive.date())
                    });
                    if overridden {
                        continue;
                    }
                    let rid = recurring.then_some(inst.start_ms);
                    let occ = make_occurrence(
                        m,
                        &uid,
                        rid,
                        inst.start_ms,
                        inst.end_ms,
                        s.start.is_date(),
                        recurring,
                        rrule.clone(),
                    );
                    out.push(occ);
                }
            }
        }
        let index = match &series {
            Some(s) if !rids.is_empty() => {
                Some(s.index(&rids.iter().map(|(ms, _, _)| *ms).collect::<Vec<_>>()))
            }
            _ => None,
        };
        for (rid_ms, rid_date, ev) in rids {
            if out.len() >= MAX_INSTANCES {
                break;
            }
            if status_of(ev) == "CANCELLED" || master.is_some_and(|m| status_of(m) == "CANCELLED") {
                continue;
            }
            if let Some(s) = &series {
                if s.ex_ms.contains(&rid_ms) || rid_date.is_some_and(|d| s.ex_dates.contains(&d)) {
                    continue;
                }
                if !index.as_ref().is_some_and(|i| i.contains(rid_ms, rid_date)) {
                    continue;
                }
            }
            let Some((start, start_ms, end_ms)) = override_times(&ctx, ev, series.as_ref(), rid_ms)
            else {
                continue;
            };
            if overlaps(start_ms, end_ms, from_ms, to_ms) {
                out.push(make_occurrence(
                    ev,
                    &uid,
                    Some(rid_ms),
                    start_ms,
                    end_ms,
                    start.is_date(),
                    true,
                    rrule.clone(),
                ));
            }
        }
    }
    out.sort_by_key(|o| (o.start_ms, o.end_ms));
    Ok(out)
}

/// Start value, start and end ms of an override (defaults from its master).
fn override_times(
    ctx: &Ctx,
    ev: &Component,
    series: Option<&Series>,
    rid_ms: i64,
) -> Option<(Dt, i64, i64)> {
    let start = ctx
        .dt(ev, "DTSTART")
        .or_else(|| ctx.dt(ev, "RECURRENCE-ID"))?;
    let start_ms = if ev.prop("DTSTART").is_some() {
        start.ms(&ctx.local)
    } else {
        rid_ms
    };
    let has_len = ev.prop("DTEND").is_some() || ev.prop("DURATION").is_some();
    let span = match series {
        Some(s) if !has_len => s.span,
        _ => event_span(ctx, ev, &start),
    };
    let zone = start.zone(&ctx.local);
    let end_ms = match span {
        Span::Days(n) => zone.utc_ms(add_days(start.naive(), n)),
        Span::Exact(ms) => start_ms + ms,
        Span::Nominal(d) => zone.utc_ms(add_days(start.naive(), d.days)) + d.secs * 1000,
    };
    Some((start, start_ms, end_ms))
}

/// Earliest start and latest end (None = recurs forever) of the object.
/// The master VEVENT's DTSTART as epoch ms: editing "all events" from one
/// occurrence moves the series by that occurrence's change.
pub fn master_start(cal: &Component, local: Tz) -> Option<i64> {
    let ctx = Ctx::new(cal, Zone::Tz(local));
    let ev = cal.children.get(master_index(cal)?)?;
    ctx.dt(ev, "DTSTART").map(|d| d.ms(&ctx.local))
}

pub fn object_span(cal: &Component, local: Tz) -> Option<(i64, Option<i64>)> {
    let ctx = Ctx::new(cal, Zone::Tz(local));
    let mut lo: Option<i64> = None;
    let mut hi: Option<i64> = Some(i64::MIN);
    let mut push = |s: i64, e: Option<i64>| {
        lo = Some(lo.map_or(s, |l| l.min(s)));
        hi = match (hi, e) {
            (Some(h), Some(e)) => Some(h.max(e).max(s)),
            _ => None,
        };
    };
    // DATE and floating values are padded: they move with the zone they're shown in.
    let pad = |floats: bool| if floats { MAX_OFFSET_MS } else { 0 };
    for (_, master, overrides) in groups(cal) {
        let series = master.and_then(|m| Series::new(&ctx, m, true));
        if let Some(s) = &series {
            let first = s.start.ms(&ctx.local);
            let pad = pad(s.floats());
            if s.infinite() {
                let rdate_min = s.rdates.iter().map(|t| s.zone.utc_ms(*t)).min();
                push(rdate_min.map_or(first, |r| r.min(first)) - pad, None);
            } else {
                let all = s.instances(None, MAX_SERIES);
                let start = all.first().map_or(first, |i| i.start_ms.min(first));
                let end = if all.len() >= MAX_SERIES || s.exhausted() {
                    None
                } else {
                    Some(all.iter().map(|i| i.end_ms).max().unwrap_or(start) + pad)
                };
                push(start - pad, end);
            }
        }
        let zone = series
            .as_ref()
            .map(|s| s.zone.clone())
            .unwrap_or_else(|| ctx.local.clone());
        for ev in overrides {
            let Some((rid, _)) = rid_of(&ctx, &zone, ev) else {
                continue;
            };
            if let Some((start, s, e)) = override_times(&ctx, ev, series.as_ref(), rid) {
                let pad = pad(matches!(start, Dt::Date(_) | Dt::Floating(_)));
                push(s - pad, Some(e + pad));
            }
        }
    }
    lo.map(|l| (l, hi.filter(|h| *h != i64::MIN)))
}

fn now_utc() -> String {
    Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

fn input_zone(input: &EventInput) -> Zone {
    Zone::Tz(resolve_tzid(&input.tz).unwrap_or(Tz::UTC))
}

fn normalize_rrule(r: &Option<String>) -> Option<String> {
    r.as_ref()
        .map(|r| r.trim().trim_start_matches("RRULE:").trim().to_string())
        .filter(|r| !r.is_empty())
}

fn alarm_component(minutes_before: i64) -> Component {
    let mut a = Component::new("VALARM");
    a.props.push(Property::new("ACTION", "DISPLAY"));
    a.props.push(Property::new_text("DESCRIPTION", "Reminder"));
    a.props.push(Property::new(
        "TRIGGER",
        &format_minutes_trigger(minutes_before),
    ));
    a
}

/// Set a TEXT property unless unchanged; empty removes it.
fn set_text(ev: &mut Component, name: &str, text: &str) {
    if ev.prop(name).map(|p| p.text()).unwrap_or_default() == text {
        return;
    }
    if text.is_empty() {
        ev.remove_props(name);
        return;
    }
    let mut p = Property::new_text(name, text);
    if let Some(old) = ev.prop(name) {
        p.params = old
            .params
            .iter()
            .filter(|(n, _)| n != "ALTREP")
            .cloned()
            .collect();
    }
    ev.set_prop(p);
}

/// Times as the form will write them: DATE values for all-day input (end exclusive).
fn input_times(input: &EventInput, zone: &Zone) -> (i64, i64) {
    if !input.all_day {
        return (input.start_ms, input.end_ms.max(input.start_ms));
    }
    let start = zone.local(input.start_ms).date();
    let end_local = zone.local(input.end_ms);
    let mut end = end_local.date();
    if end_local.time() != NaiveTime::MIN {
        end += Duration::days(1);
    }
    if end <= start {
        end = start + Duration::days(1);
    }
    (
        zone.utc_ms(start.and_time(NaiveTime::MIN)),
        zone.utc_ms(end.and_time(NaiveTime::MIN)),
    )
}

/// Write the editable fields of `input` onto a VEVENT using `form` for date-times.
fn apply_input(ev: &mut Component, input: &EventInput, form: &Form, zone: &Zone) {
    set_text(ev, "SUMMARY", &input.summary);
    set_text(ev, "LOCATION", &input.location);
    let old_desc = ev.prop("DESCRIPTION").map(|p| p.text()).unwrap_or_default();
    if old_desc != input.description {
        ev.remove_props("X-ALT-DESC");
    }
    set_text(ev, "DESCRIPTION", &input.description);
    let (start, end) = input_times(input, zone);
    ev.set_prop(form.prop("DTSTART", start, zone));
    ev.remove_props("DURATION");
    ev.set_prop(form.prop("DTEND", end, zone));
    let current = alarms(ev, start, end);
    if current != input.alarms_minutes_before {
        ev.children.retain(|c| c.name != "VALARM");
        ev.children.extend(
            input
                .alarms_minutes_before
                .iter()
                .map(|m| alarm_component(*m)),
        );
    }
}

/// Bump SEQUENCE and refresh DTSTAMP / LAST-MODIFIED.
fn touch(ev: &mut Component, min_sequence: i64) {
    let seq = sequence(ev).max(min_sequence) + 1;
    ev.set_prop(Property::new("SEQUENCE", &seq.to_string()));
    let now = now_utc();
    ev.set_prop(Property::new("DTSTAMP", &now));
    ev.set_prop(Property::new("LAST-MODIFIED", &now));
}

fn sequence(ev: &Component) -> i64 {
    ev.prop("SEQUENCE")
        .and_then(|p| p.value.trim().parse().ok())
        .unwrap_or(0)
}

/// Add a VTIMEZONE for `form`'s zone if the calendar has none with that TZID.
fn ensure_vtimezone(cal: &mut Component, form: &Form, start_ms: i64) {
    let Form::Zoned(id, Zone::Tz(tz)) = form else {
        return;
    };
    if cal.children_named("VTIMEZONE").any(|v| {
        v.prop("TZID")
            .is_some_and(|p| p.value.trim_matches('"') == id)
    }) {
        return;
    }
    let now_year = Utc::now().year();
    let start_year = Zone::Tz(*tz).local(start_ms).year();
    let vtz = vtimezone(
        id,
        *tz,
        start_year.min(now_year),
        now_year.max(start_year) + 1,
    );
    let pos = cal
        .children
        .iter()
        .position(|c| c.name != "VTIMEZONE")
        .unwrap_or(cal.children.len());
    cal.children.insert(pos, vtz);
}

/// Form for a timed series in the user's zone (UTC when the zone is UTC).
fn zoned_form(input: &EventInput) -> Form {
    match resolve_tzid(&input.tz) {
        Some(tz) if tz != Tz::UTC => Form::Zoned(tz.name().to_string(), Zone::Tz(tz)),
        _ => Form::Utc,
    }
}

/// New VCALENDAR with one VEVENT built from `input`.
pub fn new_object(input: &EventInput) -> Component {
    let zone = input_zone(input);
    let rrule = normalize_rrule(&input.rrule);
    let form = match (input.all_day, &rrule) {
        (true, _) => Form::Date,
        (false, Some(_)) => zoned_form(input),
        (false, None) => Form::Utc,
    };
    let mut cal = Component::new("VCALENDAR");
    cal.props.push(Property::new("VERSION", "2.0"));
    cal.props.push(Property::new("PRODID", PRODID));
    cal.props.push(Property::new("CALSCALE", "GREGORIAN"));
    let now = now_utc();
    let mut ev = Component::new("VEVENT");
    ev.props.push(Property::new(
        "UID",
        &format!("{}@remail", uuid::Uuid::new_v4()),
    ));
    ev.props.push(Property::new("DTSTAMP", &now));
    ev.props.push(Property::new("CREATED", &now));
    ev.props.push(Property::new("LAST-MODIFIED", &now));
    ev.props.push(Property::new("SEQUENCE", "0"));
    apply_input(&mut ev, input, &form, &zone);
    if let Some(r) = rrule {
        ev.props.push(Property::new("RRULE", &r));
    }
    cal.children.push(ev);
    ensure_vtimezone(&mut cal, &form, input.start_ms);
    cal
}

fn master_index(cal: &Component) -> Option<usize> {
    cal.children
        .iter()
        .position(|c| c.name == "VEVENT" && c.prop("RECURRENCE-ID").is_none())
}

/// Index of the override of `uid` whose RECURRENCE-ID is `rid_ms`.
fn override_index(
    cal: &Component,
    uid: &str,
    series_zone: &Zone,
    local: &Zone,
    rid_ms: i64,
) -> Option<usize> {
    let ctx = Ctx::new(cal, local.clone());
    cal.children.iter().position(|c| {
        c.name == "VEVENT"
            && c.prop("UID")
                .map(|p| p.text().trim().to_string())
                .unwrap_or_default()
                == uid
            && rid_of(&ctx, series_zone, c).is_some_and(|(ms, _)| ms == rid_ms)
    })
}

fn uid_of(ev: &Component) -> String {
    ev.prop("UID")
        .map(|p| p.text().trim().to_string())
        .unwrap_or_default()
}

/// Edit an event in place, keeping everything not touched by `input`.
///
/// `All` edits the master (dropping overrides/EXDATEs that no longer match a
/// moved start or changed rule); `This` adds or replaces the override for one
/// instance. SEQUENCE, DTSTAMP and LAST-MODIFIED are updated.
pub fn update_object(cal: &mut Component, input: &EventInput, scope: EditScope) -> Result<()> {
    let zone = input_zone(input);
    match scope {
        EditScope::All => update_all(cal, input, &zone),
        EditScope::This { recurrence_id } => update_this(cal, input, &zone, recurrence_id),
    }
}

fn update_all(cal: &mut Component, input: &EventInput, zone: &Zone) -> Result<()> {
    let idx = master_index(cal)
        .or_else(|| cal.children.iter().position(|c| c.name == "VEVENT"))
        .ok_or_else(|| anyhow!("calendar object has no VEVENT"))?;
    let rrule = normalize_rrule(&input.rrule);
    let (old_start, old_form, old_rrule) = {
        let ctx = Ctx::new(cal, zone.clone());
        let ev = &cal.children[idx];
        let start = ctx.dt(ev, "DTSTART");
        let form = match ev.prop("DTSTART").and_then(|p| p.param("TZID")) {
            Some(id) => ctx
                .known_zone(id)
                .map(|z| Form::Zoned(id.trim_matches('"').to_string(), z)),
            None => start.as_ref().map(Form::of),
        };
        let rule = ev.prop("RRULE").map(|p| p.value.trim().to_string());
        (start.as_ref().map(|s| s.ms(zone)), form, rule)
    };
    let form = if input.all_day {
        Form::Date
    } else {
        match old_form {
            Some(f @ Form::Zoned(..)) => f,
            _ if rrule.is_some() => zoned_form(input),
            _ => Form::Utc,
        }
    };
    let is_override = cal.children[idx].prop("RECURRENCE-ID").is_some();
    let uid = uid_of(&cal.children[idx]);
    {
        let ev = &mut cal.children[idx];
        apply_input(ev, input, &form, zone);
        if !is_override {
            match &rrule {
                Some(r) => ev.set_prop(Property::new("RRULE", r)),
                None => {
                    for name in ["RRULE", "RDATE", "EXDATE", "EXRULE"] {
                        ev.remove_props(name);
                    }
                }
            }
        }
        touch(ev, 0);
    }
    ensure_vtimezone(cal, &form, input.start_ms);
    if is_override {
        return Ok(());
    }
    if rrule.is_none() {
        cal.children.retain(|c| {
            !(c.name == "VEVENT" && c.prop("RECURRENCE-ID").is_some() && uid_of(c) == uid)
        });
        return Ok(());
    }
    let new_start = input_times(input, zone).0;
    if old_start != Some(new_start) || old_rrule != rrule {
        prune_stale(cal, &uid, zone);
    }
    Ok(())
}

/// Drop EXDATE values and overrides that are no longer instances of the master.
fn prune_stale(cal: &mut Component, uid: &str, local: &Zone) {
    let Some(idx) = master_index(cal) else { return };
    let (keep_ex, stale): (Vec<Option<Property>>, Vec<usize>) = {
        let ctx = Ctx::new(cal, local.clone());
        let master = &cal.children[idx];
        let Some(series) = Series::new(&ctx, master, false) else {
            return;
        };
        // (epoch ms, DATE) of one EXDATE value.
        let ex_point = |p: &Property, v: &str| {
            let one = Property {
                value: v.to_string(),
                ..p.clone()
            };
            ctx.values(&one).first().map(|d| match d {
                Dt::Date(date) => (d.ms(&series.zone), Some(*date)),
                Dt::Floating(t) => (series.zone.utc_ms(*t), None),
                other => (other.ms(local), None),
            })
        };
        let overrides: Vec<(usize, Option<RidPoint>)> = cal
            .children
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.name == "VEVENT" && c.prop("RECURRENCE-ID").is_some() && uid_of(c) == uid
            })
            .map(|(i, c)| (i, rid_of(&ctx, &series.zone, c)))
            .collect();
        let mut points: Vec<i64> = master
            .props
            .iter()
            .filter(|p| p.name == "EXDATE")
            .flat_map(|p| p.value.split(',').filter_map(|v| ex_point(p, v)))
            .map(|(ms, _)| ms)
            .collect();
        points.extend(overrides.iter().filter_map(|(_, r)| r.map(|(ms, _)| ms)));
        let index = series.index(&points);
        let keep_ex = master
            .props
            .iter()
            .map(|p| {
                if p.name != "EXDATE" {
                    return None;
                }
                let kept: Vec<&str> = p
                    .value
                    .split(',')
                    .filter(|v| ex_point(p, v).is_some_and(|(ms, d)| index.contains(ms, d)))
                    .collect();
                Some(Property {
                    value: kept.join(","),
                    ..p.clone()
                })
            })
            .collect();
        let stale = overrides
            .into_iter()
            .filter(|(_, r)| !r.is_some_and(|(ms, d)| index.contains(ms, d)))
            .map(|(i, _)| i)
            .collect();
        (keep_ex, stale)
    };
    let master = &mut cal.children[idx];
    let mut new_props = Vec::with_capacity(master.props.len());
    for (p, kept) in master.props.drain(..).zip(keep_ex) {
        match kept {
            None => new_props.push(p),
            Some(k) if !k.value.is_empty() => new_props.push(k),
            Some(_) => {}
        }
    }
    master.props = new_props;
    for i in stale.into_iter().rev() {
        cal.children.remove(i);
    }
}

fn update_this(cal: &mut Component, input: &EventInput, zone: &Zone, rid_ms: i64) -> Result<()> {
    let master_idx = master_index(cal);
    let (uid, master_form, series_zone, master_seq) = match master_idx {
        Some(i) => {
            let ctx = Ctx::new(cal, zone.clone());
            let m = &cal.children[i];
            let start = ctx
                .dt(m, "DTSTART")
                .ok_or_else(|| anyhow!("master VEVENT has no DTSTART"))?;
            (uid_of(m), Form::of(&start), start.zone(zone), sequence(m))
        }
        None => {
            let ev = cal
                .children
                .iter()
                .find(|c| c.name == "VEVENT")
                .ok_or_else(|| anyhow!("calendar object has no VEVENT"))?;
            (uid_of(ev), Form::Utc, zone.clone(), 0)
        }
    };
    let existing = override_index(cal, &uid, &series_zone, zone, rid_ms);
    let idx = match (existing, master_idx) {
        (Some(i), _) => i,
        (None, Some(m)) => {
            let mut ev = cal.children[m].clone();
            for name in ["RRULE", "RDATE", "EXDATE", "EXRULE"] {
                ev.remove_props(name);
            }
            let rid = master_form.prop("RECURRENCE-ID", rid_ms, zone);
            let pos = ev
                .props
                .iter()
                .position(|p| p.name == "DTSTART")
                .map_or(ev.props.len(), |p| p + 1);
            ev.props.insert(pos, rid);
            cal.children.insert(m + 1, ev);
            m + 1
        }
        (None, None) => return Err(anyhow!("no instance {rid_ms} in this event")),
    };
    let form = match (&master_form, input.all_day) {
        (_, true) => Form::Date,
        (Form::Date, false) => Form::Utc,
        (f, false) => f.clone(),
    };
    let ev = &mut cal.children[idx];
    apply_input(ev, input, &form, zone);
    touch(ev, master_seq);
    Ok(())
}

/// Remove one instance of a recurring event (EXDATE on the master, override dropped).
///
/// DATE and floating series need the zone the `recurrence_id` was computed in;
/// it is inferred from the instance's time of day (the app uses
/// [`delete_occurrence_in`], which knows the zone).
#[cfg(test)]
pub fn delete_occurrence(cal: &mut Component, recurrence_id: i64) -> Result<()> {
    use chrono::Timelike;
    let guess = master_index(cal)
        .and_then(|i| {
            let ctx = Ctx::new(cal, Zone::Tz(Tz::UTC));
            ctx.dt(&cal.children[i], "DTSTART")
        })
        .map(|start| {
            let tod = i64::from(start.naive().time().num_seconds_from_midnight()) * 1000;
            let mut off = (tod - recurrence_id).rem_euclid(DAY_MS);
            if off >= 14 * 3_600_000 {
                off -= DAY_MS;
            }
            Zone::Fixed((off / 1000) as i32)
        })
        .unwrap_or(Zone::Tz(Tz::UTC));
    delete_with_zone(cal, recurrence_id, &guess)
}

/// Remove one instance of a recurring event; `local` is the zone passed to [`occurrences`].
///
/// Removing the only instance of an object without a master (an invitation
/// to one occurrence) leaves no VEVENT: delete the resource instead of writing it.
pub fn delete_occurrence_in(cal: &mut Component, recurrence_id: i64, local: Tz) -> Result<()> {
    delete_with_zone(cal, recurrence_id, &Zone::Tz(local))
}

fn delete_with_zone(cal: &mut Component, rid_ms: i64, local: &Zone) -> Result<()> {
    let Some(m) = master_index(cal) else {
        let uid = object_uid(cal).unwrap_or_default();
        let before = cal.children.len();
        if let Some(i) = override_index(cal, &uid, local, local, rid_ms) {
            cal.children.remove(i);
        }
        if cal.children.len() == before {
            return Err(anyhow!("no instance {rid_ms} in this event"));
        }
        return Ok(());
    };
    let (uid, form, series_zone) = {
        let ctx = Ctx::new(cal, local.clone());
        let master = &cal.children[m];
        let start = ctx
            .dt(master, "DTSTART")
            .ok_or_else(|| anyhow!("master VEVENT has no DTSTART"))?;
        (uid_of(master), Form::of(&start), start.zone(local))
    };
    if let Some(i) = override_index(cal, &uid, &series_zone, local, rid_ms) {
        cal.children.remove(i);
    }
    let master = &mut cal.children[m];
    let ex = form.prop("EXDATE", rid_ms, local);
    let pos = master
        .props
        .iter()
        .rposition(|p| p.name == "EXDATE" || p.name == "RRULE")
        .map_or(master.props.len(), |p| p + 1);
    master.props.insert(pos, ex);
    touch(master, 0);
    Ok(())
}
