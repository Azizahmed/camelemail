//! Smart Inbox: sort incoming mail into Important (`primary`) and the
//! "Other" buckets (updates, promotions, social, forums) from headers,
//! sender and subject. Runs locally on every synced message; the AI
//! assistant can refine it (see `ai::classify_batch`).

use serde::{Deserialize, Serialize};

use crate::model::EmailAddress;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    #[default]
    Primary,
    Updates,
    Promotions,
    Social,
    Forums,
}

impl Category {
    pub const ALL: [Category; 5] = [Category::Primary, Category::Updates, Category::Promotions, Category::Social, Category::Forums];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Primary => "primary",
            Category::Updates => "updates",
            Category::Promotions => "promotions",
            Category::Social => "social",
            Category::Forums => "forums",
        }
    }

    pub fn parse(s: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.as_str().eq_ignore_ascii_case(s.trim()))
    }
}

/// Header facts the classifier needs, collected while parsing.
#[derive(Debug, Clone, Default)]
pub struct Signals {
    pub list_id: bool,
    pub list_post: bool,
    pub list_unsubscribe: bool,
    /// `Precedence: bulk | list | junk`.
    pub bulk_precedence: bool,
    /// `Auto-Submitted` other than `no`.
    pub auto_submitted: bool,
    /// Headers left by marketing platforms (campaign ids, Mailchimp, Klaviyo, …).
    pub marketing_platform: bool,
    /// Headers left by any bulk sender (SendGrid, SES, Mailgun, Postmark, …).
    pub bulk_platform: bool,
}

impl Signals {
    /// Inspect one header (name as received, value raw).
    pub fn observe(&mut self, name: &str, value: &str) {
        let n = name.to_ascii_lowercase();
        let v = value.trim().to_ascii_lowercase();
        match n.as_str() {
            "list-id" => self.list_id = true,
            // RFC 2369: `List-Post: NO` (optionally with a comment) means posting isn't allowed.
            "list-post" => self.list_post = v.split(|c: char| c.is_whitespace() || c == '(').next() != Some("no"),
            "list-unsubscribe" => self.list_unsubscribe = true,
            "precedence" => self.bulk_precedence |= matches!(v.as_str(), "bulk" | "list" | "junk"),
            "auto-submitted" => self.auto_submitted |= v != "no",
            "x-auto-response-suppress" | "feedback-id" | "x-report-abuse" | "x-abuse-report" => self.bulk_platform = true,
            _ => {}
        }
        if MARKETING_HEADERS.iter().any(|p| n.starts_with(p)) {
            self.marketing_platform = true;
            self.bulk_platform = true;
        } else if BULK_HEADERS.iter().any(|p| n.starts_with(p)) {
            self.bulk_platform = true;
        }
    }

    fn bulk(&self) -> bool {
        self.list_unsubscribe || self.bulk_precedence || self.list_id || self.bulk_platform
    }
}

const MARKETING_HEADERS: &[&str] = &[
    "x-campaign",
    "x-campaignid",
    "x-mailchimp",
    "x-mc-user",
    "x-klaviyo",
    "x-kmail",
    "x-marketo",
    "x-mktomail",
    "x-hubspot",
    "x-hs-",
    "x-braze",
    "x-iterable",
    "x-emarsys",
    "x-sfmc",
    "x-exacttarget",
    "x-rpcampaign",
    "x-csa-complaints",
    "x-mailer-recptid",
    "x-cmae",
    "x-sib-",
    "x-ac-",
    "x-omnisend",
    "x-customerio",
    "x-mailerlite",
    "x-convertkit",
];

const BULK_HEADERS: &[&str] = &[
    "x-sg-",
    "x-sendgrid",
    "x-mailgun",
    "x-mailjet",
    "x-ses-",
    "x-pm-message-id",
    "x-postmark",
    "x-mandrill",
    "x-msys-",
    "x-sparkpost",
    "x-amazon-mail",
    "x-zendesk",
    "x-github-reason",
    "x-gitlab",
    "x-jira",
    "x-atlassian",
    "x-google-sender-auth",
    "x-notifications",
];

const SOCIAL_DOMAINS: &[&str] = &[
    "facebookmail.com",
    "facebook.com",
    "instagram.com",
    "mail.instagram.com",
    "linkedin.com",
    "twitter.com",
    "x.com",
    "tiktok.com",
    "pinterest.com",
    "reddit.com",
    "redditmail.com",
    "snapchat.com",
    "tumblr.com",
    "quora.com",
    "discord.com",
    "discordapp.com",
    "whatsapp.com",
    "telegram.org",
    "threads.net",
    "meetup.com",
    "nextdoor.com",
    "strava.com",
    "goodreads.com",
    "bsky.app",
    "mastodon.social",
    "youtube.com",
    "twitch.tv",
    "vk.com",
];

const FORUM_DOMAINS: &[&str] = &["googlegroups.com", "groups.io", "discoursemail.com", "freelists.org", "lists.sourceforge.net"];

const PROMO_LOCALPARTS: &[&str] = &[
    "news", "newsletter", "newsletters", "marketing", "offers", "offer", "deals", "deal", "promo", "promos", "promotions",
    "sales", "sale", "shop", "store", "hello", "hi", "info", "team", "email", "mail", "mailer", "campaign", "rewards",
    "specials", "members", "membership", "club", "editor", "editors", "digest",
];

const UPDATE_LOCALPARTS: &[&str] = &[
    "noreply", "no-reply", "no_reply", "donotreply", "do-not-reply", "do_not_reply", "notifications", "notification",
    "notify", "alerts", "alert", "updates", "update", "billing", "receipts", "receipt", "invoice", "invoices", "orders",
    "order", "shipping", "account", "accounts", "security", "mailer-daemon", "postmaster", "system", "automated",
    "bounce", "bounces", "auto-confirm", "confirm", "verify", "service", "services", "reminders", "reminder",
    "calendar", "statements", "payments", "payment",
];

const PROMO_SUBJECT: &[&str] = &[
    "% off", "sale", "deal", "deals", "offer", "discount", "coupon", "promo", "free shipping", "limited time",
    "last chance", "new arrivals", "black friday", "cyber monday", "exclusive", "ends tonight", "flash", "save up to",
    "save $", "shop now", "don't miss", "dont miss", "just dropped", "special price", "% discount", "bogo", "clearance",
    "newsletter", "webinar", "giveaway",
];

const UPDATE_SUBJECT: &[&str] = &[
    "receipt", "invoice", "order", "shipped", "shipping", "delivery", "delivered", "your account", "verify",
    "verification", "confirm", "password", "sign-in", "sign in", "login", "log in", "security alert", "statement",
    "payment", "transaction", "subscription", "renewal", "reminder", "appointment", "booking", "reservation",
    "itinerary", "ticket", "one-time", "code", "otp", "2fa", "alert", "notification", "report", "summary", "welcome to",
];

fn domain_of(email: &str) -> String {
    email.rsplit('@').next().unwrap_or("").trim().to_ascii_lowercase()
}

fn local_of(email: &str) -> String {
    email.split('@').next().unwrap_or("").trim().to_ascii_lowercase()
}

fn domain_in(domain: &str, list: &[&str]) -> bool {
    list.iter().any(|d| domain == *d || domain.ends_with(&format!(".{d}")))
}

/// Local part matches a role word exactly or as a token (`news-us`, `team.acme`, `noreply+123`).
fn local_matches(local: &str, list: &[&str]) -> bool {
    let base = local.split('+').next().unwrap_or(local);
    list.contains(&base) || base.split(['.', '-', '_']).any(|t| list.contains(&t))
}

fn subject_has(subject: &str, list: &[&str]) -> bool {
    let s = subject.to_lowercase();
    list.iter().any(|k| {
        if k.chars().all(|c| c.is_ascii_alphanumeric()) {
            // Whole words only, so "sale" doesn't match "wholesale".
            s.split(|c: char| !c.is_alphanumeric()).any(|w| w == *k)
        } else {
            s.contains(k)
        }
    })
}

/// Classify one message.
pub fn classify(s: &Signals, from: &EmailAddress, subject: &str) -> Category {
    let domain = domain_of(&from.email);
    let local = local_of(&from.email);

    if domain_in(&domain, SOCIAL_DOMAINS) {
        return Category::Social;
    }
    if domain_in(&domain, FORUM_DOMAINS) || (s.list_id && s.list_post && !s.marketing_platform) {
        return Category::Forums;
    }
    let bulk = s.bulk();
    let promo_sender = local_matches(&local, PROMO_LOCALPARTS);
    let update_sender = local_matches(&local, UPDATE_LOCALPARTS);
    if s.marketing_platform || (bulk && (subject_has(subject, PROMO_SUBJECT) || (promo_sender && !subject_has(subject, UPDATE_SUBJECT)))) {
        return Category::Promotions;
    }
    if s.auto_submitted || update_sender || bulk {
        return Category::Updates;
    }
    if subject_has(subject, PROMO_SUBJECT) && promo_sender {
        return Category::Promotions;
    }
    Category::Primary
}

/// Words that introduce a one-time code (whole words; Arabic matched as phrases).
const OTP_WORDS: &[&str] = &[
    "verification code", "verify code", "security code", "one-time code", "one-time password", "one time password",
    "one-time passcode", "otp", "passcode", "login code", "log-in code", "sign-in code", "sign in code",
    "confirmation code", "authentication code", "auth code", "2fa code", "two-factor", "temporary code", "magic code",
    "código de verificación", "code de vérification", "bestätigungscode", "رمز التحقق", "رمز التأكيد",
    "كلمة المرور لمرة واحدة", "رمز الدخول", "الرمز السري",
];
/// Everyday phrases that only count when the code follows right after them.
const OTP_WEAK_WORDS: &[&str] = &["your code is", "your code:", "access code", "pin code", "pin:", "code:"];

fn word_regex(words: &[&str]) -> regex::Regex {
    let alts: Vec<String> = words.iter().map(|w| regex::escape(w)).collect();
    regex::Regex::new(&format!(r"(?i)(?:^|[^\p{{L}}\p{{N}}])(?:{})(?:$|[^\p{{L}}\p{{N}}])", alts.join("|"))).expect("valid regex")
}

/// A one-time code email (OTP, sign-in or verification code): a code word
/// with a 4–8 character code close to it (years and the like don't count).
pub fn is_one_time_code(subject: &str, body_start: &str) -> bool {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static STRONG: Lazy<Regex> = Lazy::new(|| word_regex(OTP_WORDS));
    static WEAK: Lazy<Regex> = Lazy::new(|| word_regex(OTP_WEAK_WORDS));
    static CODE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?:^|[^\w-])(\d{4,8}|\d{3}[ -]\d{3}|[A-Z0-9]{6,8})(?:$|[^\w-])").expect("valid regex"));
    let text = format!("{subject}\n{body_start}");
    let strong: Vec<(usize, usize)> = STRONG.find_iter(&text).map(|m| (m.start(), m.end())).collect();
    let weak: Vec<(usize, usize)> = WEAK.find_iter(&text).map(|m| (m.start(), m.end())).collect();
    if strong.is_empty() && weak.is_empty() {
        return false;
    }
    CODE.captures_iter(&text).filter_map(|c| c.get(1)).any(|m| {
        let code = m.as_str();
        // Codes have digits; a year is not a code.
        if !code.chars().any(char::is_numeric) || (code.len() == 4 && (code.starts_with("19") || code.starts_with("20"))) {
            return false;
        }
        let after = |(_, end): &(usize, usize)| m.start() >= *end && m.start() - end <= 120;
        let before = |(start, _): &(usize, usize)| *start >= m.end() && start - m.end() <= 60;
        let right_after = |(_, end): &(usize, usize)| m.start() >= *end && m.start() - end <= 3;
        strong.iter().any(|w| after(w) || before(w)) || weak.iter().any(right_after)
    })
}

/// Best guess without headers (rows synced before classification existed
/// and whose raw message isn't cached).
pub fn classify_basic(from: &EmailAddress, subject: &str, has_unsubscribe: bool) -> Category {
    let s = Signals { list_unsubscribe: has_unsubscribe, ..Default::default() };
    classify(&s, from, subject)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(headers: &[(&str, &str)]) -> Signals {
        let mut s = Signals::default();
        for (n, v) in headers {
            s.observe(n, v);
        }
        s
    }

    fn addr(e: &str) -> EmailAddress {
        EmailAddress::new("", e)
    }

    #[test]
    fn personal_mail_is_primary() {
        assert_eq!(classify(&Signals::default(), &addr("sara@acme.com"), "Lunch tomorrow?"), Category::Primary);
        assert_eq!(classify(&Signals::default(), &addr("bob@gmail.com"), "Re: contract"), Category::Primary);
    }

    #[test]
    fn marketing_is_promotions() {
        let s = sig(&[("List-Unsubscribe", "<https://x/u>"), ("X-Mailchimp-Campaign", "1")]);
        assert_eq!(classify(&s, &addr("hello@shop.com"), "Hi"), Category::Promotions);
        let s = sig(&[("List-Unsubscribe", "<https://x/u>")]);
        assert_eq!(classify(&s, &addr("news@brand.com"), "Our autumn collection"), Category::Promotions);
        assert_eq!(classify(&s, &addr("orders@brand.com"), "50% off everything"), Category::Promotions);
    }

    #[test]
    fn transactional_is_updates() {
        assert_eq!(classify(&Signals::default(), &addr("no-reply@bank.com"), "Your statement is ready"), Category::Updates);
        let s = sig(&[("Auto-Submitted", "auto-generated")]);
        assert_eq!(classify(&s, &addr("ci@build.dev"), "Build failed"), Category::Updates);
        let s = sig(&[("X-SES-Outgoing", "1"), ("List-Unsubscribe", "<mailto:u@x>")]);
        assert_eq!(classify(&s, &addr("account@service.io"), "Your password was changed"), Category::Updates);
    }

    #[test]
    fn social_and_forums() {
        assert_eq!(classify(&Signals::default(), &addr("notification@facebookmail.com"), "x"), Category::Social);
        assert_eq!(classify(&Signals::default(), &addr("messages-noreply@linkedin.com"), "x"), Category::Social);
        let s = sig(&[("List-Id", "<dev.lists.example.org>"), ("List-Post", "<mailto:dev@lists.example.org>")]);
        assert_eq!(classify(&s, &addr("someone@example.org"), "[dev] RFC: new API"), Category::Forums);
        let s = sig(&[("List-Id", "<desktop.gnome.org>"), ("List-Post", "<mailto:desktop-devel-list@gnome.org>")]);
        assert_eq!(classify(&s, &addr("dev@gnome.org"), "Release notes"), Category::Forums);
        let s = sig(&[("List-Id", "<news.example.org>"), ("List-Post", "NO (posting not allowed)")]);
        assert_ne!(classify(&s, &addr("dev@example.org"), "Release notes"), Category::Forums);
    }

    #[test]
    fn one_time_codes() {
        assert!(is_one_time_code("Your verification code", "Use 482913 to sign in. It expires in 10 minutes."));
        assert!(is_one_time_code("123-456 is your Instagram code", "Your code: 123 456"));
        assert!(is_one_time_code("رمز التحقق", "رمز التحقق الخاص بك هو 7741"));
        assert!(!is_one_time_code("Lunch tomorrow?", "Let's meet at 1230 near the office"));
        assert!(!is_one_time_code("Your verification code", "Click the link to verify your account"));
        assert!(!is_one_time_code("Re: refactor", "Your code is ready for review – PR 4821"));
        assert!(!is_one_time_code("Footprint report 2026", "Hotpot dinner at 7. Room 4821."));
        assert!(!is_one_time_code("Status", "The OTP feature shipped in 2025, see the docs."));
        assert!(is_one_time_code("Sign in to Acme", "Your code is 739204"));
        // Arabic-Indic digits.
        assert!(is_one_time_code("رمز التحقق", "رمزك ٤٨٢٩١٣"));
    }

    #[test]
    fn whole_word_subject_match() {
        assert!(!subject_has("Wholesale pricing question", PROMO_SUBJECT));
        assert!(subject_has("Big SALE today", PROMO_SUBJECT));
    }
}
