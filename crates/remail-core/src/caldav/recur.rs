//! RFC 5545 recurrence rules (RRULE), expanded in local wall-clock time.
//!
//! Expansion works on naive date-times so DST is handled by the caller when it
//! maps each wall-clock instance to UTC in the event's zone.

use anyhow::{anyhow, bail, Result};
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

/// Recurrence frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Freq {
    Secondly,
    Minutely,
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// Upper bound of a rule as written in the RRULE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Until {
    Date(NaiveDate),
    /// UTC instant (`...Z`).
    Utc(NaiveDateTime),
    /// Floating date-time (interpreted in the event's zone).
    Local(NaiveDateTime),
}

/// A parsed RRULE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recur {
    pub freq: Freq,
    pub interval: u32,
    pub count: Option<u32>,
    pub until: Option<Until>,
    pub by_second: Vec<u32>,
    pub by_minute: Vec<u32>,
    pub by_hour: Vec<u32>,
    /// `(ordinal, weekday)`; ordinal 0 means every such weekday.
    pub by_day: Vec<(i32, Weekday)>,
    pub by_monthday: Vec<i32>,
    pub by_yearday: Vec<i32>,
    pub by_weekno: Vec<i32>,
    pub by_month: Vec<u32>,
    pub by_setpos: Vec<i32>,
    pub wkst: Weekday,
}

/// Safety limit on the number of periods walked by one expansion (or one
/// series, see [`Recur::expand_within`]).
pub const MAX_PERIODS: u32 = 50_000;

impl Recur {
    /// Parse an RRULE value such as `FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20250101T000000Z`.
    pub fn parse(value: &str) -> Result<Recur> {
        let value = value.trim();
        let value = value
            .strip_prefix("RRULE:")
            .or_else(|| value.strip_prefix("rrule:"))
            .unwrap_or(value);
        let mut r = Recur {
            freq: Freq::Daily,
            interval: 1,
            count: None,
            until: None,
            by_second: Vec::new(),
            by_minute: Vec::new(),
            by_hour: Vec::new(),
            by_day: Vec::new(),
            by_monthday: Vec::new(),
            by_yearday: Vec::new(),
            by_weekno: Vec::new(),
            by_month: Vec::new(),
            by_setpos: Vec::new(),
            wkst: Weekday::Mon,
        };
        let mut have_freq = false;
        for part in value.split(';').filter(|p| !p.trim().is_empty()) {
            let (k, v) = part
                .split_once('=')
                .ok_or_else(|| anyhow!("bad RRULE part {part:?}"))?;
            let v = v.trim();
            match k.trim().to_ascii_uppercase().as_str() {
                "FREQ" => {
                    have_freq = true;
                    r.freq = match v.to_ascii_uppercase().as_str() {
                        "SECONDLY" => Freq::Secondly,
                        "MINUTELY" => Freq::Minutely,
                        "HOURLY" => Freq::Hourly,
                        "DAILY" => Freq::Daily,
                        "WEEKLY" => Freq::Weekly,
                        "MONTHLY" => Freq::Monthly,
                        "YEARLY" => Freq::Yearly,
                        other => bail!("unknown FREQ {other}"),
                    }
                }
                "INTERVAL" => r.interval = v.parse::<u32>().unwrap_or(1).max(1),
                "COUNT" => r.count = v.parse().ok(),
                "UNTIL" => r.until = parse_until(v),
                "BYSECOND" => r.by_second = nums(v, 0, 60)?,
                "BYMINUTE" => r.by_minute = nums(v, 0, 59)?,
                "BYHOUR" => r.by_hour = nums(v, 0, 23)?,
                "BYMONTH" => r.by_month = nums(v, 1, 12)?,
                "BYMONTHDAY" => r.by_monthday = signed(v, 31)?,
                "BYYEARDAY" => r.by_yearday = signed(v, 366)?,
                "BYWEEKNO" => r.by_weekno = signed(v, 53)?,
                "BYSETPOS" => r.by_setpos = signed(v, 366)?,
                "WKST" => r.wkst = weekday(v).ok_or_else(|| anyhow!("bad WKST {v}"))?,
                "BYDAY" => {
                    for d in v.split(',').map(str::trim).filter(|d| !d.is_empty()) {
                        let split = d.len().saturating_sub(2);
                        if !d.is_char_boundary(split) {
                            bail!("bad BYDAY {d}");
                        }
                        let (ord, wd) = d.split_at(split);
                        let wd = weekday(wd).ok_or_else(|| anyhow!("bad BYDAY {d}"))?;
                        let ord = match ord.trim_start_matches('+') {
                            "" => 0,
                            n => n.parse::<i32>().map_err(|_| anyhow!("bad BYDAY {d}"))?,
                        };
                        r.by_day.push((ord, wd));
                    }
                }
                _ => {}
            }
        }
        if !have_freq {
            bail!("RRULE without FREQ");
        }
        Ok(r)
    }

    /// True if the rule recurs forever.
    pub fn is_infinite(&self) -> bool {
        self.count.is_none() && self.until.is_none()
    }

    /// Walk instances in order, starting with `dtstart` itself.
    ///
    /// `until` is the rule's UNTIL already mapped to the event's wall-clock time.
    /// `window` (start, end) lets expansion skip periods that cannot reach the
    /// window when no COUNT is set and stop once past the end. `visit` returns
    /// false to stop.
    pub fn expand(
        &self,
        dtstart: NaiveDateTime,
        until: Option<NaiveDateTime>,
        window: Option<(NaiveDateTime, NaiveDateTime)>,
        visit: impl FnMut(NaiveDateTime) -> bool,
    ) {
        let mut budget = MAX_PERIODS;
        self.expand_within(dtstart, until, window, &mut budget, visit);
    }

    /// [`Self::expand`] walking at most `budget` periods, which it uses up: a
    /// rule whose filters never match (`BYMONTH=2;BYMONTHDAY=30`) stops there
    /// even though it emits nothing.
    pub fn expand_within(
        &self,
        dtstart: NaiveDateTime,
        until: Option<NaiveDateTime>,
        window: Option<(NaiveDateTime, NaiveDateTime)>,
        budget: &mut u32,
        mut visit: impl FnMut(NaiveDateTime) -> bool,
    ) {
        let mut emitted: u32 = 0;
        let past_until = |t: NaiveDateTime| until.is_some_and(|u| t > u);
        if self.count == Some(0) || past_until(dtstart) {
            return;
        }
        if !visit(dtstart) {
            return;
        }
        emitted += 1;
        if self.count.is_some_and(|c| emitted >= c) {
            return;
        }
        let start_k = match (window, self.count) {
            (Some((ws, _)), None) => self.skip_periods(dtstart, ws),
            _ => 0,
        };
        let interval = i64::from(self.interval);
        for k in start_k.. {
            if *budget == 0 {
                return;
            }
            *budget -= 1;
            let Some(period) = self.period_start(dtstart, k * interval) else {
                return;
            };
            // Yearly BYWEEKNO periods may start a few days before their first day.
            let earliest = period
                .checked_sub_signed(Duration::days(7))
                .unwrap_or(period);
            if window.is_some_and(|(_, we)| earliest > we) || until.is_some_and(|u| earliest > u) {
                return;
            }
            let mut set = self.candidates(dtstart, period);
            set.sort();
            set.dedup();
            if !self.by_setpos.is_empty() {
                set = setpos(&set, &self.by_setpos);
            }
            for t in set {
                if t <= dtstart {
                    continue;
                }
                if past_until(t) {
                    return;
                }
                if let Some((_, we)) = window {
                    if t > we {
                        return;
                    }
                }
                if !visit(t) {
                    return;
                }
                emitted += 1;
                if self.count.is_some_and(|c| emitted >= c) {
                    return;
                }
            }
        }
    }

    /// Number of whole periods that can be skipped before `ws`.
    fn skip_periods(&self, dtstart: NaiveDateTime, ws: NaiveDateTime) -> i64 {
        if ws <= dtstart {
            return 0;
        }
        let span = ws - dtstart;
        let units = match self.freq {
            Freq::Yearly => i64::from(ws.year() - dtstart.year()),
            Freq::Monthly => {
                i64::from(ws.year() - dtstart.year()) * 12 + i64::from(ws.month())
                    - i64::from(dtstart.month())
            }
            Freq::Weekly => span.num_days() / 7,
            Freq::Daily => span.num_days(),
            Freq::Hourly => span.num_hours(),
            Freq::Minutely => span.num_minutes(),
            Freq::Secondly => span.num_seconds(),
        };
        (units / i64::from(self.interval) - 1).max(0)
    }

    /// Start of the `n`-th period (in units of the frequency) after dtstart's.
    fn period_start(&self, dtstart: NaiveDateTime, n: i64) -> Option<NaiveDateTime> {
        let date = dtstart.date();
        let midnight = NaiveTime::MIN;
        Some(match self.freq {
            Freq::Yearly => {
                let y = i64::from(date.year()) + n;
                NaiveDate::from_ymd_opt(i32::try_from(y).ok().filter(|y| *y < 10_000)?, 1, 1)?
                    .and_time(midnight)
            }
            Freq::Monthly => {
                let m0 = i64::from(date.year()) * 12 + i64::from(date.month0()) + n;
                let y = i32::try_from(m0.div_euclid(12))
                    .ok()
                    .filter(|y| *y < 10_000)?;
                NaiveDate::from_ymd_opt(y, u32::try_from(m0.rem_euclid(12)).ok()? + 1, 1)?
                    .and_time(midnight)
            }
            Freq::Weekly => {
                let back = (7 + date.weekday().num_days_from_monday()
                    - self.wkst.num_days_from_monday())
                    % 7;
                let ws = date.checked_sub_signed(Duration::days(i64::from(back)))?;
                ws.checked_add_signed(Duration::try_days(n.checked_mul(7)?)?)?
                    .and_time(midnight)
            }
            Freq::Daily => date
                .checked_add_signed(Duration::try_days(n)?)?
                .and_time(midnight),
            Freq::Hourly => {
                let h = dtstart.date().and_hms_opt(dtstart.hour(), 0, 0)?;
                h.checked_add_signed(Duration::try_hours(n)?)?
            }
            Freq::Minutely => {
                let m = dtstart
                    .date()
                    .and_hms_opt(dtstart.hour(), dtstart.minute(), 0)?;
                m.checked_add_signed(Duration::try_minutes(n)?)?
            }
            Freq::Secondly => dtstart
                .with_nanosecond(0)?
                .checked_add_signed(Duration::try_seconds(n)?)?,
        })
        .filter(|t| t.year() < 10_000)
    }

    /// All instances (before BYSETPOS) inside the period starting at `period`.
    fn candidates(&self, dtstart: NaiveDateTime, period: NaiveDateTime) -> Vec<NaiveDateTime> {
        let no_day_rules = self.by_weekno.is_empty()
            && self.by_yearday.is_empty()
            && self.by_monthday.is_empty()
            && self.by_day.is_empty();
        let mut by_month = self.by_month.clone();
        let mut by_monthday = self.by_monthday.clone();
        let mut by_day = self.by_day.clone();
        if no_day_rules {
            match self.freq {
                Freq::Yearly => {
                    if by_month.is_empty() {
                        by_month.push(dtstart.month());
                    }
                    by_monthday.push(dtstart.day() as i32);
                }
                Freq::Monthly => by_monthday.push(dtstart.day() as i32),
                Freq::Weekly => by_day.push((0, dtstart.weekday())),
                _ => {}
            }
        }
        let pdate = period.date();
        let days: Vec<NaiveDate> = match self.freq {
            Freq::Yearly if !self.by_weekno.is_empty() => {
                let (a, b) = (
                    week1_start(pdate.year(), self.wkst),
                    week1_start(pdate.year() + 1, self.wkst),
                );
                a.iter_days().take_while(|d| *d < b).collect()
            }
            Freq::Yearly => {
                let months: Vec<u32> = if by_month.is_empty() {
                    (1..=12).collect()
                } else {
                    by_month.clone()
                };
                let mut v = Vec::new();
                for m in months {
                    if let Some(first) = NaiveDate::from_ymd_opt(pdate.year(), m, 1) {
                        v.extend(first.iter_days().take_while(|d| d.month() == m));
                    }
                }
                v
            }
            Freq::Monthly => pdate
                .iter_days()
                .take_while(|d| d.month() == pdate.month())
                .collect(),
            Freq::Weekly => pdate.iter_days().take(7).collect(),
            _ => vec![pdate],
        };
        let ordinal_in_month =
            self.freq == Freq::Monthly || (self.freq == Freq::Yearly && !by_month.is_empty());
        let ordinals_apply =
            matches!(self.freq, Freq::Monthly | Freq::Yearly) && self.by_weekno.is_empty();
        let days = days.into_iter().filter(|d| {
            (by_month.is_empty() || by_month.contains(&d.month()))
                && (self.by_weekno.is_empty() || self.freq != Freq::Yearly || {
                    let (wn, total) = week_number(*d, pdate.year(), self.wkst);
                    self.by_weekno
                        .iter()
                        .any(|&w| w == wn || w == wn - total - 1)
                })
                && (self.by_yearday.is_empty() || {
                    let len = if d.leap_year() { 366 } else { 365 };
                    let yd = d.ordinal() as i32;
                    self.by_yearday
                        .iter()
                        .any(|&n| n == yd || n == yd - len - 1)
                })
                && (by_monthday.is_empty() || {
                    let len = month_len(d.year(), d.month()) as i32;
                    let md = d.day() as i32;
                    by_monthday.iter().any(|&n| n == md || n == md - len - 1)
                })
                && (by_day.is_empty()
                    || by_day.iter().any(|&(ord, wd)| {
                        d.weekday() == wd
                            && (ord == 0
                                || !ordinals_apply
                                || weekday_ordinal_matches(*d, ord, ordinal_in_month))
                    }))
        });
        let list = |v: &Vec<u32>, dflt: u32| if v.is_empty() { vec![dflt] } else { v.clone() };
        let mut out = Vec::new();
        for d in days {
            let (hours, minutes, seconds) = match self.freq {
                Freq::Hourly => (
                    vec![period.hour()],
                    list(&self.by_minute, dtstart.minute()),
                    list(&self.by_second, dtstart.second()),
                ),
                Freq::Minutely => (
                    vec![period.hour()],
                    vec![period.minute()],
                    list(&self.by_second, dtstart.second()),
                ),
                Freq::Secondly => (
                    vec![period.hour()],
                    vec![period.minute()],
                    vec![period.second()],
                ),
                _ => (
                    list(&self.by_hour, dtstart.hour()),
                    list(&self.by_minute, dtstart.minute()),
                    list(&self.by_second, dtstart.second()),
                ),
            };
            for &h in &hours {
                if !self.by_hour.is_empty() && !self.by_hour.contains(&h) {
                    continue;
                }
                for &m in &minutes {
                    if !self.by_minute.is_empty() && !self.by_minute.contains(&m) {
                        continue;
                    }
                    for &s in &seconds {
                        if !self.by_second.is_empty() && !self.by_second.contains(&s) {
                            continue;
                        }
                        if let Some(t) = d.and_hms_opt(h, m, s.min(59)) {
                            out.push(t);
                        }
                    }
                }
            }
        }
        out
    }
}

/// Parse an `UNTIL` value.
fn parse_until(v: &str) -> Option<Until> {
    if let Some(utc) = v.strip_suffix(['Z', 'z']) {
        return NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S")
            .ok()
            .map(Until::Utc);
    }
    if v.len() == 8 {
        return NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(Until::Date);
    }
    NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S")
        .ok()
        .map(Until::Local)
}

fn nums(v: &str, min: u32, max: u32) -> Result<Vec<u32>> {
    v.split(',')
        .map(|n| {
            n.trim()
                .parse::<u32>()
                .ok()
                .filter(|n| (min..=max).contains(n))
                .ok_or_else(|| anyhow!("bad number {n}"))
        })
        .collect()
}

fn signed(v: &str, max: i32) -> Result<Vec<i32>> {
    v.split(',')
        .map(|n| {
            n.trim()
                .trim_start_matches('+')
                .parse::<i32>()
                .ok()
                .filter(|n| *n != 0 && n.abs() <= max)
                .ok_or_else(|| anyhow!("bad number {n}"))
        })
        .collect()
}

/// Parse a two-letter weekday (`MO`…`SU`).
pub fn weekday(s: &str) -> Option<Weekday> {
    Some(match s.trim().to_ascii_uppercase().as_str() {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

/// Days in a month.
pub fn month_len(year: i32, month: u32) -> u32 {
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    };
    match (next, NaiveDate::from_ymd_opt(year, month, 1)) {
        (Some(n), Some(f)) => (n - f).num_days() as u32,
        _ => 31,
    }
}

/// Does `d` match `ord` (e.g. 2 = second, -1 = last) among its weekday in the month or year?
fn weekday_ordinal_matches(d: NaiveDate, ord: i32, in_month: bool) -> bool {
    let (index, total) = if in_month {
        let len = month_len(d.year(), d.month());
        (
            (d.day() - 1) / 7 + 1,
            (d.day() - 1) / 7 + 1 + (len - d.day()) / 7,
        )
    } else {
        let len = if d.leap_year() { 366 } else { 365 };
        (
            (d.ordinal() - 1) / 7 + 1,
            (d.ordinal() - 1) / 7 + 1 + (len - d.ordinal()) / 7,
        )
    };
    let (index, total) = (index as i32, total as i32);
    if ord > 0 {
        index == ord
    } else {
        index - total - 1 == ord
    }
}

/// First day of week 1 (the first week with at least four days in `year`).
fn week1_start(year: i32, wkst: Weekday) -> NaiveDate {
    let Some(jan1) = NaiveDate::from_ymd_opt(year, 1, 1) else {
        return NaiveDate::MIN;
    };
    let offset = (7 + jan1.weekday().num_days_from_monday() - wkst.num_days_from_monday()) % 7;
    let moved = if offset <= 3 {
        jan1.checked_sub_signed(Duration::days(i64::from(offset)))
    } else {
        jan1.checked_add_signed(Duration::days(i64::from(7 - offset)))
    };
    moved.unwrap_or(jan1)
}

/// Week number of `d` within `year` and the number of weeks in that year.
fn week_number(d: NaiveDate, year: i32, wkst: Weekday) -> (i32, i32) {
    let (a, b) = (week1_start(year, wkst), week1_start(year + 1, wkst));
    (
        ((d - a).num_days() / 7 + 1) as i32,
        ((b - a).num_days() / 7) as i32,
    )
}

fn setpos(set: &[NaiveDateTime], pos: &[i32]) -> Vec<NaiveDateTime> {
    let n = set.len() as i32;
    let mut out: Vec<NaiveDateTime> = pos
        .iter()
        .filter_map(|&p| {
            let i = if p > 0 { p - 1 } else { n + p };
            (0..n).contains(&i).then(|| set[i as usize])
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").unwrap()
    }

    /// The first `n` instances (DTSTART included) as `YYYY-MM-DD HH`.
    fn first(rule: &str, start: &str, n: usize) -> Vec<String> {
        let r = Recur::parse(rule).unwrap();
        let mut out = Vec::new();
        r.expand(at(start), None, None, |t| {
            out.push(t.format("%Y-%m-%d %H").to_string());
            out.len() < n
        });
        out
    }

    fn days(v: &[String]) -> Vec<&str> {
        v.iter().map(|s| &s[..10]).collect()
    }

    #[test]
    fn wkst_changes_biweekly_instances() {
        // RFC 5545 §3.8.5.3 examples.
        let mo = first("FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=MO", "1997-08-05T09:00", 10);
        assert_eq!(days(&mo), ["1997-08-05", "1997-08-10", "1997-08-19", "1997-08-24"]);
        let su = first("FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=SU", "1997-08-05T09:00", 10);
        assert_eq!(days(&su), ["1997-08-05", "1997-08-17", "1997-08-19", "1997-08-31"]);
    }

    #[test]
    fn yearly_ordinal_weekday() {
        let got = first("FREQ=YEARLY;BYDAY=20MO", "1997-05-19T09:00", 3);
        assert_eq!(days(&got), ["1997-05-19", "1998-05-18", "1999-05-17"]);
    }

    #[test]
    fn iso_week_numbers() {
        // Week 53 exists in 2020 and 2026 only (of 2020–2026).
        let got = first("FREQ=YEARLY;BYWEEKNO=53;BYDAY=MO", "2020-12-28T09:00", 2);
        assert_eq!(days(&got), ["2020-12-28", "2026-12-28"]);
        // Week 1 can start in the previous calendar year.
        let got = first("FREQ=YEARLY;BYWEEKNO=1;BYDAY=MO", "2024-12-30T09:00", 3);
        assert_eq!(days(&got), ["2024-12-30", "2025-12-29", "2027-01-04"]);
    }

    #[test]
    fn hourly_interval_crosses_midnight() {
        let got = first("FREQ=HOURLY;INTERVAL=5;COUNT=6", "2024-01-01T23:00", 10);
        let hours: Vec<&str> = got.iter().map(|s| &s[11..]).collect();
        assert_eq!(hours, ["23", "04", "09", "14", "19", "00"]);
        assert_eq!(got.last().unwrap(), "2024-01-03 00");
    }

    #[test]
    fn a_shared_budget_bounds_rules_that_never_match() {
        let r = Recur::parse("FREQ=DAILY;COUNT=3;BYMONTH=2;BYMONTHDAY=30").unwrap();
        let mut budget = 1000;
        let mut seen = 0;
        r.expand_within(at("2024-01-01T09:00"), None, None, &mut budget, |_| {
            seen += 1;
            true
        });
        assert_eq!((seen, budget), (1, 0));
        // Spent: a second walk emits only DTSTART.
        r.expand_within(at("2024-01-01T09:00"), None, None, &mut budget, |_| {
            seen += 1;
            true
        });
        assert_eq!(seen, 2);
    }
}
