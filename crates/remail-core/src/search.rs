//! Gmail-style search syntax: `from:`, `to:`, `subject:`, `is:unread|read|starred`,
//! `has:attachment`, `in:<folder>`, `before:`, `after:`, `older_than:`,
//! `newer_than:`, `"quoted phrases"` and `-negation`.
//!
//! One parsed [`SearchQuery`] is translated two ways: to an FTS5/SQL filter
//! over the local cache and to an IMAP `UID SEARCH` command for the server.

use chrono::{NaiveDate, TimeZone, Utc};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchQuery {
    /// Free text (`negated`, `text`).
    pub text: Vec<(bool, String)>,
    pub from: Vec<(bool, String)>,
    pub to: Vec<(bool, String)>,
    pub subject: Vec<(bool, String)>,
    pub unread: Option<bool>,
    pub starred: Option<bool>,
    pub has_attachment: Option<bool>,
    /// `in:` folder/label name.
    pub folder: Option<String>,
    /// Epoch millis bounds.
    pub before: Option<i64>,
    pub after: Option<i64>,
}

impl SearchQuery {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
            && self.from.is_empty()
            && self.to.is_empty()
            && self.subject.is_empty()
            && self.unread.is_none()
            && self.starred.is_none()
            && self.has_attachment.is_none()
            && self.before.is_none()
            && self.after.is_none()
            && self.folder.is_none()
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Text { negated: bool, value: String, quoted: bool },
    Op { negated: bool, name: String, value: String },
}

fn tokenize(input: &str) -> Vec<Token> {
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    let read_until_ws = |i: &mut usize| {
        let start = *i;
        while *i < chars.len() && !chars[*i].is_whitespace() {
            *i += 1;
        }
        chars[start..*i].iter().collect::<String>()
    };
    let read_quoted = |i: &mut usize| {
        // `i` points at the opening quote.
        *i += 1;
        let start = *i;
        while *i < chars.len() && chars[*i] != '"' {
            *i += 1;
        }
        let s: String = chars[start..*i].iter().collect();
        if *i < chars.len() {
            *i += 1;
        }
        s
    };

    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let mut negated = false;
        if chars[i] == '-' && i + 1 < chars.len() && (chars[i + 1].is_alphanumeric() || chars[i + 1] == '"') {
            negated = true;
            i += 1;
        }
        if chars[i] == '"' {
            let value = read_quoted(&mut i);
            out.push(Token::Text { negated, value, quoted: true });
            continue;
        }
        // operator?
        let mut j = i;
        while j < chars.len() && (chars[j].is_ascii_alphabetic() || chars[j] == '_') {
            j += 1;
        }
        if j > i && j < chars.len() && chars[j] == ':' {
            let name: String = chars[i..j].iter().collect::<String>().to_lowercase();
            i = j + 1;
            let value = if i < chars.len() && chars[i] == '"' { read_quoted(&mut i) } else { read_until_ws(&mut i) };
            out.push(Token::Op { negated, name, value });
            continue;
        }
        let value = read_until_ws(&mut i);
        out.push(Token::Text { negated, value, quoted: false });
    }
    out
}

/// `YYYY/MM/DD`, `YYYY-MM-DD` or `MM/DD/YYYY` → UTC midnight millis.
fn parse_abs_date(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.split(['/', '-']).collect();
    if parts.len() != 3 {
        return None;
    }
    let nums: Vec<i64> = parts.iter().map(|p| p.parse().ok()).collect::<Option<_>>()?;
    let (y, m, d) = if parts[0].len() == 4 { (nums[0], nums[1], nums[2]) } else { (nums[2], nums[0], nums[1]) };
    let date = NaiveDate::from_ymd_opt(y as i32, m as u32, d as u32)?;
    Some(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?).timestamp_millis())
}

/// `7d`, `2w`, `3m`, `1y` → millis before `now`.
fn parse_relative(s: &str, now_ms: i64) -> Option<i64> {
    let s = s.trim().to_lowercase();
    let unit = s.chars().last()?;
    let n: i64 = s[..s.len() - unit.len_utf8()].parse().ok()?;
    let days = match unit {
        'd' => 1,
        'w' => 7,
        'm' => 30,
        'y' => 365,
        _ => return None,
    };
    now_ms.checked_sub(n.checked_mul(days)?.checked_mul(86_400_000)?)
}

/// Fold Arabic spelling variants for the full-text index and its queries:
/// drop harakat, superscript alef and tatweel (the tokenizer would split
/// words at them) and write every alef-with-hamza as a bare alef and alef
/// maqsura as yeh, so `شكرا` finds `شُكْرًا` and `اجتماع` finds `إجتماع`.
pub fn normalize_ar(s: &str) -> String {
    let folds = |c: char| matches!(c as u32, 0x0622 | 0x0623 | 0x0625 | 0x0640 | 0x0649 | 0x064B..=0x065F | 0x0670 | 0x0671);
    if !s.chars().any(folds) {
        return s.to_string();
    }
    s.chars()
        .filter_map(|c| match c {
            '\u{0622}' | '\u{0623}' | '\u{0625}' | '\u{0671}' => Some('\u{0627}'),
            '\u{0649}' => Some('\u{064A}'),
            '\u{0640}' | '\u{064B}'..='\u{065F}' | '\u{0670}' => None,
            c => Some(c),
        })
        .collect()
}

/// Text copied from an email view has an invisible Arabic letter mark
/// (U+061C) after word-final Arabic marks (see `fixFinalMarks` in the UI);
/// ignore it (and a ZWNJ in the same place).
fn strip_mark_joiners(s: &str) -> String {
    let is_mark = |c: char| matches!(c as u32, 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7 | 0x06E8 | 0x06EA..=0x06ED | 0x08D3..=0x08FF);
    let mut out = String::with_capacity(s.len());
    let mut after_mark = false;
    for c in s.chars() {
        if (c == '\u{61C}' || c == '\u{200C}') && after_mark {
            after_mark = false;
            continue;
        }
        after_mark = is_mark(c);
        out.push(c);
    }
    out
}

pub fn parse(input: &str, now_ms: i64) -> SearchQuery {
    let input = &strip_mark_joiners(input);
    let mut q = SearchQuery::default();
    let mut bare: Vec<String> = Vec::new();
    for tok in tokenize(input) {
        match tok {
            Token::Text { negated, value, quoted } => {
                if value.is_empty() {
                    continue;
                }
                if !negated && !quoted {
                    bare.push(value);
                } else {
                    q.text.push((negated, value));
                }
            }
            Token::Op { negated, name, value } => {
                if value.is_empty() {
                    continue;
                }
                let v = value.clone();
                match name.as_str() {
                    "from" => q.from.push((negated, v)),
                    "to" | "cc" | "bcc" => q.to.push((negated, v)),
                    "subject" => q.subject.push((negated, v)),
                    "is" => match v.to_lowercase().as_str() {
                        "unread" => q.unread = Some(!negated),
                        "read" => q.unread = Some(negated),
                        "starred" | "flagged" => q.starred = Some(!negated),
                        _ => {}
                    },
                    "has" => {
                        if v.eq_ignore_ascii_case("attachment") || v.eq_ignore_ascii_case("attachments") {
                            q.has_attachment = Some(!negated);
                        }
                    }
                    "in" | "label" if !negated => q.folder = Some(v),
                    "before" => q.before = parse_abs_date(&v).or(q.before),
                    "after" => q.after = parse_abs_date(&v).or(q.after),
                    "older_than" => q.before = parse_relative(&v, now_ms).or(q.before),
                    "newer_than" => q.after = parse_relative(&v, now_ms).or(q.after),
                    // Unknown operator: search its operand as text.
                    _ => q.text.push((negated, v)),
                }
            }
        }
    }
    if !bare.is_empty() {
        q.text.insert(0, (false, bare.join(" ")));
    }
    q
}

// ---------------------------------------------------------------------------
// IMAP
// ---------------------------------------------------------------------------

fn imap_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn imap_date(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms).single().map(|d| d.format("%-d-%b-%Y").to_string()).unwrap_or_default()
}

/// IMAP `SEARCH` criteria (keys are ANDed; repetition is allowed by RFC 3501).
/// `has:attachment` and `in:` are not expressible and are handled by the caller.
pub fn to_imap(q: &SearchQuery) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut push = |neg: bool, key: &str, val: &str| {
        let term = format!("{} {}", key, imap_quote(val));
        parts.push(if neg { format!("NOT {}", term) } else { term });
    };
    for (n, v) in &q.text {
        push(*n, "TEXT", v);
    }
    for (n, v) in &q.from {
        push(*n, "FROM", v);
    }
    for (n, v) in &q.to {
        push(*n, "TO", v);
    }
    for (n, v) in &q.subject {
        push(*n, "SUBJECT", v);
    }
    match q.unread {
        Some(true) => parts.push("UNSEEN".into()),
        Some(false) => parts.push("SEEN".into()),
        None => {}
    }
    match q.starred {
        Some(true) => parts.push("FLAGGED".into()),
        Some(false) => parts.push("UNFLAGGED".into()),
        None => {}
    }
    if let Some(b) = q.before {
        parts.push(format!("BEFORE {}", imap_date(b)));
    }
    if let Some(a) = q.after {
        parts.push(format!("SINCE {}", imap_date(a)));
    }
    if parts.is_empty() {
        "ALL".into()
    } else {
        parts.join(" ")
    }
}

// ---------------------------------------------------------------------------
// Local SQL
// ---------------------------------------------------------------------------

/// Quote a user string as an FTS5 phrase.
fn fts_phrase(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Words of the positive free-text terms that can match anything (have a
/// letter or digit).
pub fn text_words(q: &SearchQuery) -> Vec<String> {
    q.text.iter().filter(|(n, _)| !n).flat_map(|(_, v)| fts_words(v)).map(String::from).collect()
}

fn fts_words(v: &str) -> impl Iterator<Item = &str> {
    v.split_whitespace().filter(|w| w.chars().any(char::is_alphanumeric))
}

/// Build an FTS5 MATCH expression for the text terms (None when no positive
/// text term exists — FTS5 cannot express a purely negative query).
pub fn fts_match(q: &SearchQuery) -> Option<String> {
    let mut pos = Vec::new();
    let mut neg = Vec::new();
    for (n, v) in &q.text {
        // Each whitespace word becomes its own prefix-matched term so partial
        // words ("invo" → "invoice") still hit. Words with no letters or
        // digits ("-", "&") hold no tokens and would match nothing.
        let words: Vec<String> = fts_words(&normalize_ar(v)).map(|w| format!("{}*", fts_phrase(w))).collect();
        if words.is_empty() {
            continue;
        }
        let expr = words.join(" AND ");
        if *n {
            neg.push(format!("({expr})"));
        } else {
            pos.push(format!("({expr})"));
        }
    }
    if pos.is_empty() {
        return None;
    }
    let mut out = pos.join(" AND ");
    for n in neg {
        out = format!("{out} NOT {n}");
    }
    Some(out)
}

/// SQL predicate fragments (over table alias `m`) and their parameters for
/// everything except positive full-text terms.
pub fn sql_filters(q: &SearchQuery) -> (Vec<String>, Vec<String>) {
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    let like = |s: &str| format!("%{}%", s.to_lowercase().replace('%', "\\%").replace('_', "\\_"));
    for (n, v) in &q.from {
        clauses.push(format!(
            "{}(lower(m.from_email) LIKE ? ESCAPE '\\' OR lower(m.from_name) LIKE ? ESCAPE '\\')",
            if *n { "NOT " } else { "" }
        ));
        params.push(like(v));
        params.push(like(v));
    }
    for (n, v) in &q.to {
        clauses.push(format!(
            "{}(lower(m.to_json) LIKE ? ESCAPE '\\' OR lower(m.cc_json) LIKE ? ESCAPE '\\')",
            if *n { "NOT " } else { "" }
        ));
        params.push(like(v));
        params.push(like(v));
    }
    for (n, v) in &q.subject {
        clauses.push(format!("{}lower(m.subject) LIKE ? ESCAPE '\\'", if *n { "NOT " } else { "" }));
        params.push(like(v));
    }
    // Negated text without any positive term: fall back to LIKE on subject/snippet.
    if fts_match(q).is_none() {
        for (n, v) in q.text.iter().filter(|(n, _)| *n) {
            let _ = n;
            clauses.push("NOT (lower(m.subject) LIKE ? ESCAPE '\\' OR lower(m.snippet) LIKE ? ESCAPE '\\')".into());
            params.push(like(v));
            params.push(like(v));
        }
    }
    if let Some(u) = q.unread {
        clauses.push(format!("m.is_unread = {}", u as i32));
    }
    if let Some(s) = q.starred {
        clauses.push(format!("m.is_starred = {}", s as i32));
    }
    if let Some(a) = q.has_attachment {
        clauses.push(format!("m.has_attachments = {}", a as i32));
    }
    if let Some(b) = q.before {
        clauses.push(format!("m.date < {}", b));
    }
    if let Some(a) = q.after {
        clauses.push(format!("m.date >= {}", a));
    }
    (clauses, params)
}

#[cfg(test)]
mod tests {

    #[test]
    fn copied_arabic_joiners_are_ignored() {
        assert_eq!(strip_mark_joiners("شكراً\u{61C} جداً\u{200C}"), "شكراً جداً");
        // A Persian ZWNJ between letters stays.
        assert_eq!(strip_mark_joiners("می\u{200C}خواهم"), "می\u{200C}خواهم");
    }

    use super::*;

    const NOW: i64 = 1_760_000_000_000;

    #[test]
    fn parses_operators() {
        let q = parse(r#"invoice march from:alice -subject:spam is:unread has:attachment "exact phrase" -"no way" in:Sent"#, NOW);
        assert_eq!(q.text[0], (false, "invoice march".into()));
        assert!(q.text.contains(&(false, "exact phrase".into())));
        assert!(q.text.contains(&(true, "no way".into())));
        assert_eq!(q.from, vec![(false, "alice".into())]);
        assert_eq!(q.subject, vec![(true, "spam".into())]);
        assert_eq!(q.unread, Some(true));
        assert_eq!(q.has_attachment, Some(true));
        assert_eq!(q.folder.as_deref(), Some("Sent"));
    }

    #[test]
    fn dates_and_unknown_ops() {
        let q = parse("before:2024/02/01 after:01/15/2024 label:work-x cc:bob", NOW);
        assert_eq!(q.before, Some(1_706_745_600_000));
        assert_eq!(q.after, Some(1_705_276_800_000));
        assert_eq!(q.folder.as_deref(), Some("work-x"));
        assert_eq!(q.to, vec![(false, "bob".into())]);
        let q = parse("newer_than:7d before:2024/02/31", NOW);
        assert_eq!(q.after, Some(NOW - 7 * 86_400_000));
        assert_eq!(q.before, None, "overflow date is dropped");
        let q = parse("is:read -is:starred from:", NOW);
        assert_eq!(q.unread, Some(false));
        assert_eq!(q.starred, Some(false));
        assert!(q.from.is_empty());
    }

    #[test]
    fn imap_translation() {
        let q = parse(r#"hello world from:"Alice B" -to:bob is:unread after:2024-01-15"#, NOW);
        assert_eq!(
            to_imap(&q),
            r#"TEXT "hello world" FROM "Alice B" NOT TO "bob" UNSEEN SINCE 15-Jan-2024"#
        );
        assert_eq!(to_imap(&SearchQuery::default()), "ALL");
    }

    #[test]
    fn fts_translation() {
        let q = parse(r#"invo "big deal" -spam"#, NOW);
        assert_eq!(fts_match(&q).unwrap(), r#"("invo"*) AND ("big"* AND "deal"*) NOT ("spam"*)"#);
        assert!(fts_match(&parse("-spam", NOW)).is_none());
        let (c, p) = sql_filters(&parse("-spam", NOW));
        assert_eq!(c.len(), 1);
        assert_eq!(p, vec!["%spam%", "%spam%"]);
    }

    #[test]
    fn punctuation_words_are_dropped() {
        assert_eq!(fts_match(&parse("Q3 - report", NOW)).unwrap(), r#"("Q3"* AND "report"*)"#);
        assert_eq!(fts_match(&parse("a & b", NOW)).unwrap(), r#"("a"* AND "b"*)"#);
        assert_eq!(fts_match(&parse("price –", NOW)).unwrap(), r#"("price"*)"#);
        assert_eq!(fts_match(&parse("invoice -\"--\"", NOW)).unwrap(), r#"("invoice"*)"#);
        assert!(fts_match(&parse("- & *", NOW)).is_none());
        assert_eq!(text_words(&parse("Q3 - report -spam", NOW)), vec!["Q3", "report"]);
    }

    #[test]
    fn relative_dates_with_odd_units() {
        assert_eq!(parse("older_than:7د", NOW).before, None);
        assert_eq!(parse("newer_than:é", NOW).after, None);
        assert_eq!(parse("newer_than:3é", NOW).after, None);
        assert_eq!(parse("older_than:99999999999999999d", NOW).before, None);
        assert_eq!(parse("older_than:2W", NOW).before, Some(NOW - 14 * 86_400_000));
    }

    #[test]
    fn arabic_is_normalized() {
        assert_eq!(normalize_ar("شُكْرًا"), "شكرا");
        assert_eq!(normalize_ar("إجتماع أحمد آخر ٱلله مستشفى"), "اجتماع احمد اخر الله مستشفي");
        assert_eq!(normalize_ar("جـــميل"), "جميل");
        assert_eq!(normalize_ar("مدرسة"), "مدرسة");
        assert_eq!(fts_match(&parse("إجتماع", NOW)).unwrap(), r#"("اجتماع"*)"#);
        assert!(fts_match(&parse("\u{64e}\u{651}", NOW)).is_none());
    }
}
