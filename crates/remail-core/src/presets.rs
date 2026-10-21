//! Provider presets and the time presets for snooze / send later.

use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Timelike, Weekday};
use serde::Serialize;

use crate::model::{Security, ServerConfig};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPreset {
    pub id: &'static str,
    pub name: &'static str,
    /// Lowercase email domains that select this preset automatically.
    pub domains: &'static [&'static str],
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
    pub password_help: &'static str,
    pub password_help_url: Option<&'static str>,
    /// Signs in through this OAuth provider instead of a password.
    pub oauth: Option<&'static str>,
}

/// The provider families an admin can allow or block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Gmail,
    Outlook,
    Other,
}

impl ProviderKind {
    pub fn name(self) -> &'static str {
        match self {
            ProviderKind::Gmail => "Gmail",
            ProviderKind::Outlook => "Outlook and Microsoft 365",
            ProviderKind::Other => "Other providers",
        }
    }
}

const GMAIL_DOMAINS: &[&str] = &["gmail.com", "googlemail.com"];

/// Which family an account belongs to, by its address or its IMAP server
/// (Google Workspace and Microsoft 365 use their own domains).
pub fn provider_kind(email: &str, imap_host: &str) -> ProviderKind {
    let domain = email.rsplit('@').next().unwrap_or("").trim().to_ascii_lowercase();
    let host = imap_host.trim().to_ascii_lowercase();
    if GMAIL_DOMAINS.contains(&domain.as_str()) || host == "imap.gmail.com" || host == "imap.googlemail.com" {
        ProviderKind::Gmail
    } else if MICROSOFT_DOMAINS.contains(&domain.as_str()) || host == "outlook.office365.com" || host.ends_with(".outlook.com") {
        ProviderKind::Outlook
    } else {
        ProviderKind::Other
    }
}

fn server(host: &str, port: u16, security: Security) -> ServerConfig {
    ServerConfig { host: host.to_string(), port, security }
}

/// Personal Microsoft account domains (Outlook.com, Hotmail, Live, MSN).
pub const MICROSOFT_DOMAINS: &[&str] = &[
    "outlook.com", "hotmail.com", "live.com", "msn.com", "passport.com", "windowslive.com", "outlook.sa", "outlook.fr",
    "outlook.de", "outlook.es", "outlook.it", "outlook.jp", "outlook.com.au", "outlook.com.br", "outlook.in", "outlook.ie",
    "outlook.be", "outlook.dk", "outlook.at", "outlook.cl", "outlook.com.ar", "outlook.co.th", "outlook.com.tr",
    "outlook.com.vn", "outlook.my", "outlook.ph", "outlook.sg", "outlook.co.id", "outlook.co.il", "outlook.kr",
    "outlook.cz", "outlook.pt", "outlook.sk", "outlook.lv", "outlook.hu", "outlook.com.gr", "hotmail.co.uk", "hotmail.fr",
    "hotmail.de", "hotmail.es", "hotmail.it", "hotmail.ca", "hotmail.com.au", "hotmail.co.jp", "hotmail.nl", "hotmail.be",
    "hotmail.se", "hotmail.no", "hotmail.dk", "hotmail.fi", "hotmail.com.br", "hotmail.com.ar", "hotmail.com.mx",
    "hotmail.co.za", "hotmail.co.in", "hotmail.gr", "hotmail.com.tr", "hotmail.ch", "hotmail.at", "live.co.uk", "live.fr",
    "live.de", "live.nl", "live.it", "live.ca", "live.com.au", "live.jp", "live.se", "live.be", "live.dk", "live.no",
    "live.at", "live.ch", "live.ie", "live.in", "live.com.mx", "live.cl", "live.com.ar", "live.com.pt", "live.ru",
    "msn.co.uk",
];

/// Microsoft's IMAP and SMTP servers. Personal accounts send through
/// `smtp-mail.outlook.com`, Microsoft 365 accounts through `smtp.office365.com`.
pub fn microsoft_servers(personal: bool) -> (ServerConfig, ServerConfig) {
    let smtp = if personal { "smtp-mail.outlook.com" } else { "smtp.office365.com" };
    (server("outlook.office365.com", 993, Security::Tls), server(smtp, 587, Security::Starttls))
}

/// Gmail and Google Workspace, signed in with Google (XOAUTH2).
pub fn google_servers() -> (ServerConfig, ServerConfig) {
    (server("imap.gmail.com", 993, Security::Tls), server("smtp.gmail.com", 465, Security::Tls))
}

/// Common IMAP providers, Gmail (app password over IMAP) and
/// Microsoft (Outlook.com / Hotmail / Microsoft 365, signed in with OAuth).
pub fn provider_presets() -> Vec<ProviderPreset> {
    vec![
        ProviderPreset {
            id: "outlook",
            name: "Outlook.com / Hotmail",
            domains: MICROSOFT_DOMAINS,
            imap: server("outlook.office365.com", 993, Security::Tls),
            smtp: server("smtp-mail.outlook.com", 587, Security::Starttls),
            password_help: "Microsoft no longer accepts passwords from mail apps. Use Sign in with Microsoft.",
            password_help_url: None,
            oauth: Some(crate::oauth::MICROSOFT),
        },
        ProviderPreset {
            id: "gmail",
            name: "Gmail",
            domains: GMAIL_DOMAINS,
            imap: server("imap.gmail.com", 993, Security::Tls),
            smtp: server("smtp.gmail.com", 465, Security::Tls),
            password_help: "Turn on 2-Step Verification, then create an app password for Remail.",
            oauth: None,
            password_help_url: Some("https://myaccount.google.com/apppasswords"),
        },
        ProviderPreset {
            id: "icloud",
            name: "iCloud Mail",
            domains: &["icloud.com", "me.com", "mac.com"],
            imap: server("imap.mail.me.com", 993, Security::Tls),
            smtp: server("smtp.mail.me.com", 587, Security::Starttls),
            password_help: "Use an app-specific password generated from your Apple Account.",
            oauth: None,
            password_help_url: Some("https://account.apple.com/account/manage"),
        },
        ProviderPreset {
            id: "yahoo",
            name: "Yahoo Mail",
            domains: &["yahoo.com", "ymail.com", "rocketmail.com"],
            imap: server("imap.mail.yahoo.com", 993, Security::Tls),
            smtp: server("smtp.mail.yahoo.com", 465, Security::Tls),
            password_help: "Generate an app password in Yahoo Account Security.",
            oauth: None,
            password_help_url: Some("https://login.yahoo.com/account/security"),
        },
        ProviderPreset {
            id: "fastmail",
            name: "Fastmail",
            domains: &["fastmail.com", "fastmail.fm"],
            imap: server("imap.fastmail.com", 993, Security::Tls),
            smtp: server("smtp.fastmail.com", 465, Security::Tls),
            password_help: "Create an app password with IMAP and SMTP access.",
            oauth: None,
            password_help_url: Some("https://app.fastmail.com/settings/security/integrations"),
        },
        ProviderPreset {
            id: "zoho",
            name: "Zoho Mail",
            domains: &["zoho.com", "zohomail.com"],
            imap: server("imap.zoho.com", 993, Security::Tls),
            smtp: server("smtp.zoho.com", 465, Security::Tls),
            password_help: "Enable IMAP access in Zoho Mail settings first.",
            oauth: None,
            password_help_url: Some("https://mail.zoho.com/zm/#settings/all/mailaccounts"),
        },
        ProviderPreset {
            id: "aol",
            name: "AOL Mail",
            domains: &["aol.com"],
            imap: server("imap.aol.com", 993, Security::Tls),
            smtp: server("smtp.aol.com", 465, Security::Tls),
            password_help: "Generate an app password in your AOL account security settings.",
            password_help_url: None,
            oauth: None,
        },
        ProviderPreset {
            id: "gmx",
            name: "GMX",
            domains: &["gmx.com", "gmx.net", "gmx.de"],
            imap: server("imap.gmx.net", 993, Security::Tls),
            smtp: server("mail.gmx.net", 587, Security::Starttls),
            password_help: "Enable IMAP/POP3 access in GMX settings.",
            password_help_url: None,
            oauth: None,
        },
    ]
}

pub fn preset_for_email(email: &str) -> Option<ProviderPreset> {
    let domain = email.rsplit('@').next()?.trim().to_lowercase();
    provider_presets().into_iter().find(|p| p.domains.contains(&domain.as_str()))
}

pub fn preset_for_host(host: &str) -> Option<ProviderPreset> {
    let host = host.trim().to_lowercase();
    provider_presets().into_iter().find(|p| p.imap.host == host)
}

/// Best-guess server settings for an address with no preset: `imap.<domain>`
/// on 993/TLS and `smtp.<domain>` on 465/TLS.
pub fn guess_servers(email: &str) -> (ServerConfig, ServerConfig) {
    if let Some(p) = preset_for_email(email) {
        return (p.imap, p.smtp);
    }
    let domain = email.rsplit('@').next().unwrap_or("").trim().to_lowercase();
    (server(&format!("imap.{domain}"), 993, Security::Tls), smtp_from_imap_host(&format!("imap.{domain}")))
}

/// SMTP defaults from an IMAP host: preset match first, else swap a leading
/// `imap.` for `smtp.`, port 465 TLS.
pub fn smtp_from_imap_host(imap_host: &str) -> ServerConfig {
    if let Some(p) = preset_for_host(imap_host) {
        return p.smtp;
    }
    let h = imap_host.trim();
    let host = if h.len() > 5 && h[..5].eq_ignore_ascii_case("imap.") {
        format!("smtp.{}", &h[5..])
    } else {
        h.to_string()
    };
    server(&host, 465, Security::Tls)
}

// ----------------------------------------------------------------------------
// Time presets
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimePreset {
    pub id: &'static str,
    pub label: &'static str,
    /// Epoch millis.
    pub at: i64,
}

/// User-adjustable snooze times.
#[derive(Debug, Clone)]
pub struct SnoozeTimes {
    /// Allowed: 60, 120, 180, 240, 480.
    pub later_minutes: i64,
    pub afternoon: NaiveTime,
    pub evening: NaiveTime,
    pub morning: NaiveTime,
    pub tomorrow: NaiveTime,
    pub day_after: NaiveTime,
    pub saturday: NaiveTime,
    pub monday: NaiveTime,
}

fn hm(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

impl Default for SnoozeTimes {
    fn default() -> Self {
        Self {
            later_minutes: 120,
            afternoon: hm(15, 0),
            evening: hm(18, 0),
            morning: hm(8, 0),
            tomorrow: hm(10, 0),
            day_after: hm(11, 0),
            saturday: hm(10, 0),
            monday: hm(10, 0),
        }
    }
}

pub const ALLOWED_LATER_MINUTES: [i64; 5] = [60, 120, 180, 240, 480];
/// Scheduled sends must be at least this far in the future.
pub const MIN_SCHEDULE_LEAD_MS: i64 = 30_000;

/// Parses `HH:MM` (24h).
pub fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    let (h, m) = s.trim().split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    NaiveTime::from_hms_opt(h.parse().ok()?, m.parse().ok()?, 0)
}

fn at_time<Tz: TimeZone>(day: &DateTime<Tz>, t: NaiveTime) -> DateTime<Tz> {
    let tz = day.timezone();
    let naive = day.date_naive().and_time(t);
    tz.from_local_datetime(&naive)
        .earliest()
        // DST gap: fall back to an hour later.
        .unwrap_or_else(|| tz.from_local_datetime(&(naive + Duration::hours(1))).earliest().unwrap())
}

/// The next `weekday` strictly after `now`'s date (never today).
fn next_weekday<Tz: TimeZone>(now: &DateTime<Tz>, weekday: Weekday) -> DateTime<Tz> {
    let today = now.weekday().num_days_from_monday() as i64;
    let target = weekday.num_days_from_monday() as i64;
    let mut diff = (target - today + 7) % 7;
    if diff == 0 {
        diff = 7;
    }
    now.clone() + Duration::days(diff)
}

/// Start of the local hour. Subtracts rather than setting fields, which would
/// fail on an ambiguous local time (the hour repeated when clocks go back).
fn floor_to_hour<Tz: TimeZone>(t: DateTime<Tz>) -> DateTime<Tz> {
    let into_hour = Duration::minutes(t.minute() as i64) + Duration::seconds(t.second() as i64) + Duration::nanoseconds(t.nanosecond() as i64);
    t - into_hour
}

pub fn snooze_presets_at<Tz: TimeZone>(now: DateTime<Tz>, times: &SnoozeTimes) -> Vec<TimePreset> {
    let later_minutes =
        if ALLOWED_LATER_MINUTES.contains(&times.later_minutes) { times.later_minutes } else { 120 };
    let mut out = Vec::new();
    let later = floor_to_hour(now.clone() + Duration::minutes(later_minutes));
    out.push(TimePreset { id: "later_today", label: "Later today", at: later.timestamp_millis() });

    let afternoon = at_time(&now, times.afternoon);
    let evening = at_time(&now, times.evening);
    if afternoon > now {
        out.push(TimePreset { id: "afternoon", label: "This afternoon", at: afternoon.timestamp_millis() });
    } else if evening > now {
        out.push(TimePreset { id: "evening", label: "This evening", at: evening.timestamp_millis() });
    } else {
        let morning = at_time(&(now.clone() + Duration::days(1)), times.morning);
        out.push(TimePreset { id: "morning", label: "Tomorrow morning", at: morning.timestamp_millis() });
    }

    let tomorrow = at_time(&(now.clone() + Duration::days(1)), times.tomorrow);
    out.push(TimePreset { id: "tomorrow", label: "Tomorrow", at: tomorrow.timestamp_millis() });
    let day_after = at_time(&(now.clone() + Duration::days(2)), times.day_after);
    out.push(TimePreset { id: "day_after", label: "Day after tomorrow", at: day_after.timestamp_millis() });
    let weekend = at_time(&next_weekday(&now, Weekday::Sat), times.saturday);
    out.push(TimePreset { id: "this_weekend", label: "This weekend", at: weekend.timestamp_millis() });
    let next_week = at_time(&next_weekday(&now, Weekday::Mon), times.monday);
    out.push(TimePreset { id: "next_week", label: "Next week", at: next_week.timestamp_millis() });
    out
}

pub fn snooze_presets(times: &SnoozeTimes) -> Vec<TimePreset> {
    snooze_presets_at(Local::now(), times)
}

/// Send-later presets: "Later today" (now + 4h on the hour, capped at 18:00,
/// only before 17:00), "Tomorrow morning" 08:00, "Monday morning" 08:00
/// (not offered on Mondays).
pub fn send_later_presets_at<Tz: TimeZone>(now: DateTime<Tz>) -> Vec<TimePreset> {
    let mut out = Vec::new();
    if now.hour() < 17 {
        let hour = (now.hour() + 4).min(18);
        let t = at_time(&now, hm(hour, 0));
        out.push(TimePreset { id: "later-today", label: "Later today", at: t.timestamp_millis() });
    }
    let tomorrow = at_time(&(now.clone() + Duration::days(1)), hm(8, 0));
    out.push(TimePreset { id: "tomorrow-morning", label: "Tomorrow morning", at: tomorrow.timestamp_millis() });
    if now.weekday() != Weekday::Mon {
        let monday = at_time(&next_weekday(&now, Weekday::Mon), hm(8, 0));
        out.push(TimePreset { id: "monday-morning", label: "Monday morning", at: monday.timestamp_millis() });
    }
    out
}

pub fn send_later_presets() -> Vec<TimePreset> {
    send_later_presets_at(Local::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn find<'a>(p: &'a [TimePreset], id: &str) -> Option<&'a TimePreset> {
        p.iter().find(|x| x.id == id)
    }

    #[test]
    fn send_later_rules() {
        // Wednesday 09:37 -> later today 13:00, tomorrow 08:00, Monday 08:00.
        let p = send_later_presets_at(utc(2026, 10, 14, 9, 37));
        assert_eq!(find(&p, "later-today").unwrap().at, utc(2026, 10, 14, 13, 0).timestamp_millis());
        assert_eq!(find(&p, "tomorrow-morning").unwrap().at, utc(2026, 10, 15, 8, 0).timestamp_millis());
        assert_eq!(find(&p, "monday-morning").unwrap().at, utc(2026, 10, 19, 8, 0).timestamp_millis());
        // 16:59 caps at 18:00; 17:00 hides "later today".
        let p = send_later_presets_at(utc(2026, 10, 14, 16, 59));
        assert_eq!(find(&p, "later-today").unwrap().at, utc(2026, 10, 14, 18, 0).timestamp_millis());
        assert!(find(&send_later_presets_at(utc(2026, 10, 14, 17, 0)), "later-today").is_none());
        // Mondays don't offer "Monday morning".
        assert!(find(&send_later_presets_at(utc(2026, 10, 19, 9, 0)), "monday-morning").is_none());
    }

    #[test]
    fn snooze_rules() {
        let t = SnoozeTimes::default();
        // Saturday 10:30.
        let now = utc(2026, 10, 17, 10, 30);
        let p = snooze_presets_at(now, &t);
        assert_eq!(find(&p, "later_today").unwrap().at, utc(2026, 10, 17, 12, 0).timestamp_millis());
        assert_eq!(find(&p, "afternoon").unwrap().at, utc(2026, 10, 17, 15, 0).timestamp_millis());
        assert_eq!(find(&p, "tomorrow").unwrap().at, utc(2026, 10, 18, 10, 0).timestamp_millis());
        assert_eq!(find(&p, "day_after").unwrap().at, utc(2026, 10, 19, 11, 0).timestamp_millis());
        // Today is Saturday -> next Saturday.
        assert_eq!(find(&p, "this_weekend").unwrap().at, utc(2026, 10, 24, 10, 0).timestamp_millis());
        assert_eq!(find(&p, "next_week").unwrap().at, utc(2026, 10, 19, 10, 0).timestamp_millis());
        // Evening fallback chain.
        let p = snooze_presets_at(utc(2026, 10, 17, 16, 0), &t);
        assert!(find(&p, "evening").is_some() && find(&p, "afternoon").is_none());
        let p = snooze_presets_at(utc(2026, 10, 17, 19, 0), &t);
        assert_eq!(find(&p, "morning").unwrap().at, utc(2026, 10, 18, 8, 0).timestamp_millis());
    }

    #[test]
    fn later_today_when_clocks_go_back() {
        use chrono_tz::America::New_York;
        // 01:10 EDT; an hour later it is 01:10 again (EST), an ambiguous local time.
        let now = New_York.from_utc_datetime(&utc(2026, 11, 1, 5, 10).naive_utc());
        let t = SnoozeTimes { later_minutes: 60, ..SnoozeTimes::default() };
        let p = snooze_presets_at(now, &t);
        assert_eq!(find(&p, "later_today").unwrap().at, utc(2026, 11, 1, 6, 0).timestamp_millis());
    }

    #[test]
    fn smtp_derivation_and_presets() {
        assert_eq!(smtp_from_imap_host("imap.example.com").host, "smtp.example.com");
        assert_eq!(smtp_from_imap_host("mail.example.com").host, "mail.example.com");
        assert_eq!(smtp_from_imap_host("imap.mail.me.com").port, 587);
        assert_eq!(preset_for_email("x@GMAIL.com").unwrap().id, "gmail");
        assert_eq!(guess_servers("a@corp.test").0.host, "imap.corp.test");
        assert_eq!(parse_hhmm("08:30"), Some(hm(8, 30)));
        assert_eq!(parse_hhmm("8:30"), None);
    }

    #[test]
    fn provider_families() {
        assert_eq!(provider_kind("a@gmail.com", ""), ProviderKind::Gmail);
        assert_eq!(provider_kind("a@company.com", "imap.gmail.com"), ProviderKind::Gmail);
        assert_eq!(provider_kind("a@hotmail.com", ""), ProviderKind::Outlook);
        assert_eq!(provider_kind("a@company.com", "outlook.office365.com"), ProviderKind::Outlook);
        assert_eq!(provider_kind("a@icloud.com", "imap.mail.me.com"), ProviderKind::Other);
    }
}
