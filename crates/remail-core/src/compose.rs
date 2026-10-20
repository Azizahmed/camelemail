//! Building replies, forwards and outgoing MIME messages.
//!
//! Subject prefixes, quote attribution, the forward header block and
//! signature placement follow common mail conventions, so mail sent from
//! Remail threads and quotes the way other clients expect.

use chrono::{DateTime, Local, TimeZone};
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::Message;
use once_cell::sync::Lazy;
use regex::Regex;

use crate::model::{Draft, EmailAddress, MessageFull, OutgoingAttachment, ReplyKind};

/// Total attachment cap (25 MB), what most mail servers accept.
pub const MAX_ATTACHMENT_TOTAL_BYTES: usize = 25 * 1024 * 1024;
/// Max recipients per field.
pub const MAX_RECIPIENTS_PER_FIELD: usize = 100;

/// `Re:` / `Fwd:` prefixing. Idempotent when the subject already starts with
/// the prefix (case-insensitive, also accepting the common `RE:`/`Fw:` forms).
pub fn prefixed_subject(subject: &str, kind: ReplyKind) -> String {
    let subject = subject.trim();
    let (prefix, accepted): (&str, &[&str]) = match kind {
        ReplyKind::Reply | ReplyKind::ReplyAll => ("Re:", &["re:"]),
        ReplyKind::Forward => ("Fwd:", &["fwd:", "fw:"]),
    };
    let lower = subject.to_lowercase();
    if accepted.iter().any(|p| lower.starts_with(p)) {
        return subject.to_string();
    }
    format!("{} {}", prefix, subject).trim_end().to_string()
}

fn same_address(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn is_own(addr: &str, own: &[String]) -> bool {
    own.iter().any(|o| same_address(o, addr))
}

fn push_unique(list: &mut Vec<EmailAddress>, addr: &EmailAddress) {
    if !addr.email.contains('@') {
        return;
    }
    if !list.iter().any(|a| same_address(&a.email, &addr.email)) {
        list.push(addr.clone());
    }
}

/// Recipients for a reply / reply-all.
///
/// - Reply goes to `Reply-To` if present, else `From`. If the original was
///   sent from the replying account, reply to its original `To` instead.
/// - Reply-all adds the original `To` + `Cc` to Cc, minus all of our own
///   addresses and anything already in To.
pub fn reply_recipients(
    original: &MessageFull,
    kind: ReplyKind,
    account_email: &str,
    own_addresses: &[String],
) -> (Vec<EmailAddress>, Vec<EmailAddress>) {
    let m = &original.meta;
    let mut to = Vec::new();
    let mut cc = Vec::new();
    if kind == ReplyKind::Forward {
        return (to, cc);
    }

    let sent_by_me = same_address(&m.from.email, account_email);
    if sent_by_me {
        for a in &m.to {
            push_unique(&mut to, a);
        }
    } else if !m.reply_to.is_empty() {
        for a in &m.reply_to {
            push_unique(&mut to, a);
        }
    } else {
        push_unique(&mut to, &m.from);
    }

    if kind == ReplyKind::ReplyAll {
        for a in m.to.iter().chain(m.cc.iter()) {
            if is_own(&a.email, own_addresses) || to.iter().any(|t| same_address(&t.email, &a.email)) {
                continue;
            }
            push_unique(&mut cc, a);
        }
    }
    // Replying to our own message with nobody else on it: send to ourselves
    // rather than leaving To empty.
    if to.is_empty() {
        push_unique(&mut to, &m.from);
    }
    (to, cc)
}

pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// `Mon, Feb 25, 2026 at 10:00 AM` (local time).
pub fn quote_date(ts_ms: i64) -> String {
    match Local.timestamp_millis_opt(ts_ms).single() {
        Some(dt) => format_quote_date(&dt),
        None => String::new(),
    }
}

fn format_quote_date<Tz: TimeZone>(dt: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    dt.format("%a, %b %-d, %Y at %-I:%M %p").to_string()
}

/// Body of the original message as HTML (plain text gets escaped and
/// line-broken).
fn original_body_html(original: &MessageFull) -> String {
    if let Some(html) = &original.html {
        strip_document_wrapper(html)
    } else if let Some(text) = &original.text {
        text_to_html(text)
    } else {
        String::new()
    }
}

pub fn text_to_html(text: &str) -> String {
    escape_html(text).replace("\r\n", "\n").replace('\n', "<br>")
}

/// Removes `<html>`, `<head>`, `<body>` wrappers, `<style>` and `<script>`
/// blocks so a full HTML document can be embedded in a quote.
pub fn strip_document_wrapper(html: &str) -> String {
    static RE_BLOCKS: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"(?is)<(style|script|head)\b[^>]*>.*?</(style|script|head)\s*>").unwrap()
    });
    static RE_TAGS: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"(?is)<!doctype[^>]*>|</?(html|body|meta)\b[^>]*>").unwrap()
    });
    let s = RE_BLOCKS.replace_all(html, "");
    RE_TAGS.replace_all(&s, "").trim().to_string()
}

/// `<div class="gmail_quote">On …, Name <addr> wrote:<blockquote>…</blockquote></div>`
pub fn reply_quote_html(original: &MessageFull) -> String {
    let attribution = format!(
        "On {}, {} wrote:",
        escape_html(&quote_date(original.meta.date)),
        escape_html(&original.meta.from.to_header())
    );
    format!(
        "<div class=\"gmail_quote\"><div dir=\"ltr\" class=\"gmail_attr\">{attribution}<br></div>\
<blockquote class=\"gmail_quote\" style=\"margin:0px 0px 0px 0.8ex;border-left:1px solid rgb(204,204,204);padding-left:1ex\">{}</blockquote></div>",
        original_body_html(original)
    )
}

fn address_list(list: &[EmailAddress]) -> String {
    list.iter()
        .map(|a| if a.name.trim().is_empty() { format!("<{}>", a.email) } else { a.to_header() })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `---------- Forwarded message ----------` block followed by the original body.
pub fn forward_block_html(original: &MessageFull) -> String {
    let m = &original.meta;
    let mut out = String::from("---------- Forwarded message ----------<br>");
    out.push_str(&format!("From: {}<br>", escape_html(&address_list(std::slice::from_ref(&m.from)))));
    out.push_str(&format!("Date: {}<br>", escape_html(&quote_date(m.date))));
    out.push_str(&format!("Subject: {}<br>", escape_html(&m.subject)));
    if !m.to.is_empty() {
        out.push_str(&format!("To: {}<br>", escape_html(&address_list(&m.to))));
    }
    if !m.cc.is_empty() {
        out.push_str(&format!("Cc: {}<br>", escape_html(&address_list(&m.cc))));
    }
    out.push_str("<br>");
    out.push_str(&original_body_html(original));
    out
}

/// Signature wrapper; the `signature` class marks it for later replacement.
pub fn signature_block(signature_html: &str) -> String {
    format!("<br><br><div class=\"signature\">{}</div>", signature_html)
}

/// `References` for a reply, from the original's Message-ID, In-Reply-To and
/// References: its References (or its In-Reply-To) + its Message-ID.
pub fn reply_references(message_id: Option<&str>, in_reply_to: Option<&str>, references: &[String]) -> Vec<String> {
    let mut refs = references.to_vec();
    if refs.is_empty() {
        if let Some(irt) = in_reply_to {
            refs.push(irt.to_string());
        }
    }
    if let Some(mid) = message_id {
        if !refs.iter().any(|r| r == mid) {
            refs.push(mid.to_string());
        }
    }
    refs
}

/// Parsed `mailto:` URL (RFC 6068).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Mailto {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
}

fn percent_decode(s: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        (b as char).to_digit(16).map(|d| d as u8)
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let (Some(h), Some(l)) = (bytes.get(i + 1).and_then(|b| hex(*b)), bytes.get(i + 2).and_then(|b| hex(*b))) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn split_addrs(s: &str) -> Vec<String> {
    s.split(',').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect()
}

/// Recipients in the path win over `to=`; for repeated keys the first wins.
pub fn parse_mailto(url: &str) -> Option<Mailto> {
    let rest = url.get(..7).filter(|p| p.eq_ignore_ascii_case("mailto:")).map(|_| &url[7..])?;
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    let mut out = Mailto { to: split_addrs(&percent_decode(path)), ..Default::default() };
    let mut seen = std::collections::HashSet::new();
    if let Some(q) = query {
        for pair in q.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let key = percent_decode(&k.replace('+', " ")).to_lowercase();
            let val = percent_decode(&v.replace('+', " "));
            if !seen.insert(key.clone()) {
                continue;
            }
            match key.as_str() {
                "to" if out.to.is_empty() => out.to = split_addrs(&val),
                "cc" => out.cc = split_addrs(&val),
                "bcc" => out.bcc = split_addrs(&val),
                "subject" => out.subject = val,
                "body" => out.body = val,
                _ => {}
            }
        }
    }
    Some(out)
}

/// Parse a free-typed recipient (`Name <a@b>` or `a@b`).
pub fn parse_address(s: &str) -> Option<EmailAddress> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let (Some(lt), Some(gt)) = (s.rfind('<'), s.rfind('>')) {
        if lt < gt {
            let email = s[lt + 1..gt].trim().to_string();
            let name = s[..lt].trim().trim_matches('"').trim().to_string();
            return email.contains('@').then_some(EmailAddress { name, email });
        }
    }
    s.contains('@').then(|| EmailAddress { name: String::new(), email: s.to_string() })
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("invalid address {0}")]
    Address(String),
    #[error("message has no recipients")]
    NoRecipients,
    #[error("too many recipients in one field (max {MAX_RECIPIENTS_PER_FIELD})")]
    TooManyRecipients,
    #[error("attachments exceed the 25 MB limit")]
    AttachmentsTooLarge,
    #[error("attachment {0} is not valid base64")]
    AttachmentEncoding(String),
    #[error("could not build message: {0}")]
    Build(String),
}

fn mailbox(a: &EmailAddress) -> Result<Mailbox, BuildError> {
    let addr = a.email.parse().map_err(|_| BuildError::Address(a.email.clone()))?;
    Ok(Mailbox::new((!a.name.trim().is_empty()).then(|| a.name.trim().to_string()), addr))
}

/// Everything needed to turn a draft into an RFC 5322 message.
pub struct OutgoingMeta<'a> {
    pub from: EmailAddress,
    pub message_id: String,
    pub in_reply_to: Option<&'a str>,
    pub references: &'a [String],
}

/// Build the outgoing message. Returns the formatted bytes (used both for
/// SMTP and for APPENDing a copy to Sent) and the envelope recipients.
pub fn build_message(draft: &Draft, meta: &OutgoingMeta<'_>) -> Result<(Vec<u8>, Vec<String>), BuildError> {
    use base64::Engine;

    for field in [&draft.to, &draft.cc, &draft.bcc] {
        if field.len() > MAX_RECIPIENTS_PER_FIELD {
            return Err(BuildError::TooManyRecipients);
        }
    }
    if draft.to.is_empty() && draft.cc.is_empty() && draft.bcc.is_empty() {
        return Err(BuildError::NoRecipients);
    }

    let mut b = Message::builder()
        .from(mailbox(&meta.from)?)
        .subject(draft.subject.clone())
        .message_id(Some(format!("<{}>", meta.message_id)))
        .date_now()
        .user_agent("Remail".to_string());
    for a in &draft.to {
        b = b.to(mailbox(a)?);
    }
    for a in &draft.cc {
        b = b.cc(mailbox(a)?);
    }
    for a in &draft.bcc {
        b = b.bcc(mailbox(a)?);
    }
    if let Some(irt) = meta.in_reply_to {
        b = b.in_reply_to(format!("<{}>", irt));
    }
    if !meta.references.is_empty() {
        let refs = meta.references.iter().map(|r| format!("<{}>", r)).collect::<Vec<_>>().join(" ");
        b = b.references(refs);
    }

    let full_body = if draft.quoted_html.trim().is_empty() {
        draft.html_body.clone()
    } else {
        format!("{}<br>{}", draft.html_body, draft.quoted_html)
    };
    // Pictures in the body go out as inline (`cid:`) parts: many clients drop `data:` images.
    let (full_body, inline) = inline_images(&full_body, &meta.message_id);
    let html = wrap_html(&full_body);
    let text = crate::mime::html_to_text(&full_body);
    let alternative = MultiPart::alternative()
        .singlepart(SinglePart::builder().header(ContentType::TEXT_PLAIN).body(text))
        .singlepart(SinglePart::builder().header(ContentType::TEXT_HTML).body(html));
    let mut total: usize = inline.iter().map(|(_, _, bytes)| bytes.len()).sum();
    let body = if inline.is_empty() {
        alternative
    } else {
        let mut related = MultiPart::related().multipart(alternative);
        for (cid, mime, bytes) in inline {
            let ct = ContentType::parse(&mime).unwrap_or_else(|_| ContentType::parse("application/octet-stream").unwrap());
            related = related.singlepart(Attachment::new_inline(cid).body(bytes, ct));
        }
        related
    };

    let mut decoded: Vec<(&OutgoingAttachment, Vec<u8>)> = Vec::new();
    for att in &draft.attachments {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(att.data_base64.as_bytes())
            .map_err(|_| BuildError::AttachmentEncoding(att.filename.clone()))?;
        total += bytes.len();
        decoded.push((att, bytes));
    }
    if total > MAX_ATTACHMENT_TOTAL_BYTES {
        return Err(BuildError::AttachmentsTooLarge);
    }

    let message = if decoded.is_empty() {
        b.multipart(body)
    } else {
        let mut mixed = MultiPart::mixed().multipart(body);
        for (att, bytes) in decoded {
            let ct = ContentType::parse(&att.mime_type)
                .unwrap_or_else(|_| ContentType::parse("application/octet-stream").unwrap());
            mixed = mixed.singlepart(Attachment::new(att.filename.clone()).body(bytes, ct));
        }
        b.multipart(mixed)
    }
    .map_err(|e| BuildError::Build(e.to_string()))?;

    let envelope = message.envelope().to().iter().map(|a| a.to_string()).collect();
    Ok((message.formatted(), envelope))
}

static DATA_IMG_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)(<img\b[^>]*?\bsrc\s*=\s*)(?:"data:(image/[a-z0-9.+-]+);base64,([a-z0-9+/=\s]+)"|'data:(image/[a-z0-9.+-]+);base64,([a-z0-9+/=\s]+)')"#).unwrap()
});

/// Swap `data:` images for `cid:` references. Returns the new HTML and the
/// parts (content id, MIME type, bytes); the same picture is attached once.
fn inline_images(html: &str, message_id: &str) -> (String, Vec<(String, String, Vec<u8>)>) {
    use base64::Engine;
    let mut parts: Vec<(String, String, Vec<u8>)> = Vec::new();
    let out = DATA_IMG_RE.replace_all(html, |c: &regex::Captures<'_>| {
        let mime = c.get(2).or(c.get(4)).map_or("", |m| m.as_str()).to_ascii_lowercase();
        let data: String = c.get(3).or(c.get(5)).map_or("", |m| m.as_str()).split_ascii_whitespace().collect();
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data.as_bytes()) else {
            return c[0].to_string();
        };
        let cid = match parts.iter().find(|(_, m, b)| *m == mime && *b == bytes) {
            Some((cid, _, _)) => cid.clone(),
            None => {
                let cid = format!("img{}.{}", parts.len() + 1, message_id);
                parts.push((cid.clone(), mime, bytes));
                cid
            }
        };
        format!("{}\"cid:{}\"", &c[1], cid)
    });
    (out.into_owned(), parts)
}

fn wrap_html(body: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head><body>{}</body></html>",
        body
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MessageMeta;

    fn msg(from: &str, to: &[&str], cc: &[&str]) -> MessageFull {
        let a = |s: &str| parse_address(s).unwrap();
        MessageFull {
            meta: MessageMeta {
                id: "m1".into(),
                account_id: "acc".into(),
                thread_id: "t1".into(),
                folder: "INBOX".into(),
                uid: 1,
                message_id: Some("orig@x.test".into()),
                subject: "Plans".into(),
                from: a(from),
                to: to.iter().map(|s| a(s)).collect(),
                cc: cc.iter().map(|s| a(s)).collect(),
                bcc: vec![],
                reply_to: vec![],
                date: 1_760_434_200_000,
                snippet: String::new(),
                is_unread: false,
                is_starred: false,
                is_draft: false,
                is_answered: false,
                has_attachments: false,
                labels: vec![],
                unsubscribe: None,
                body_synced: true,
            },
            html: None,
            text: Some("Line one\nLine <two>".into()),
            attachments: vec![],
            in_reply_to: Some("root@x.test".into()),
            references: vec![],
            has_remote_images: false,
        }
    }

    #[test]
    fn subject_prefix_is_idempotent() {
        assert_eq!(prefixed_subject("Hello", ReplyKind::Reply), "Re: Hello");
        assert_eq!(prefixed_subject("Re: Hello", ReplyKind::Reply), "Re: Hello");
        assert_eq!(prefixed_subject("RE: Hello", ReplyKind::ReplyAll), "RE: Hello");
        assert_eq!(prefixed_subject("Hello", ReplyKind::Forward), "Fwd: Hello");
        assert_eq!(prefixed_subject("Fw: Hello", ReplyKind::Forward), "Fw: Hello");
        assert_eq!(prefixed_subject("", ReplyKind::Reply), "Re:");
    }

    #[test]
    fn reply_all_excludes_self_and_duplicates() {
        let m = msg("Alice <alice@x.test>", &["me@x.test", "Bob <bob@x.test>"], &["carol@x.test", "ALICE@x.test"]);
        let own = vec!["me@x.test".to_string()];
        let (to, cc) = reply_recipients(&m, ReplyKind::ReplyAll, "me@x.test", &own);
        assert_eq!(to.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(), vec!["alice@x.test"]);
        assert_eq!(cc.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(), vec!["bob@x.test", "carol@x.test"]);
        let (to, cc) = reply_recipients(&m, ReplyKind::Reply, "me@x.test", &own);
        assert_eq!(to.len(), 1);
        assert!(cc.is_empty());
    }

    #[test]
    fn reply_to_own_message_goes_to_original_recipients() {
        let m = msg("me@x.test", &["bob@x.test"], &[]);
        let (_to, _) = reply_recipients(&m, ReplyKind::Reply, "me@x.test", &["me@x.test".into()]);
        // Mail from my *other* account is a normal incoming message.
        let m = msg("other@x.test", &["me@x.test"], &[]);
        let (to, _) = reply_recipients(&m, ReplyKind::Reply, "me@x.test", &["me@x.test".into(), "other@x.test".into()]);
        assert_eq!(to[0].email, "other@x.test");
        let m = msg("me@x.test", &["bob@x.test"], &[]);
        let (to, _) = reply_recipients(&m, ReplyKind::Reply, "me@x.test", &["me@x.test".into()]);
        assert_eq!(to[0].email, "bob@x.test");
    }

    #[test]
    fn quote_and_forward_blocks() {
        let m = msg("Alice <alice@x.test>", &["bob@x.test"], &[]);
        let q = reply_quote_html(&m);
        assert!(q.contains("wrote:"));
        assert!(q.contains("Alice &lt;alice@x.test&gt;"));
        assert!(q.contains("Line one<br>Line &lt;two&gt;"));
        let f = forward_block_html(&m);
        assert!(f.starts_with("---------- Forwarded message ----------<br>From: Alice &lt;alice@x.test&gt;<br>"));
        assert!(f.contains("To: &lt;bob@x.test&gt;<br>"));
        assert!(!f.contains("Cc:"));
        let refs = |m: &MessageFull| reply_references(m.meta.message_id.as_deref(), m.in_reply_to.as_deref(), &m.references);
        assert_eq!(refs(&m), vec!["root@x.test", "orig@x.test"]);
        assert_eq!(reply_references(Some("c@x"), Some("b@x"), &[]), vec!["b@x", "c@x"]);
        assert_eq!(reply_references(Some("c@x"), None, &["a@x".into(), "c@x".into()]), vec!["a@x", "c@x"]);
    }

    #[test]
    fn mailto_parsing() {
        let m = parse_mailto("mailto:a@x.test,b@x.test?subject=Hi%20there&to=ignored@x.test&body=Line+1&subject=dup").unwrap();
        assert_eq!(m.to, vec!["a@x.test", "b@x.test"]);
        assert_eq!(m.subject, "Hi there");
        assert_eq!(m.body, "Line 1");
        let m = parse_mailto("MAILTO:?to=c@x.test&cc=d@x.test").unwrap();
        assert_eq!(m.to, vec!["c@x.test"]);
        assert_eq!(m.cc, vec!["d@x.test"]);
        assert!(parse_mailto("https://x").is_none());
        // Multibyte text and stray '%' must not panic.
        let m = parse_mailto("mailto:a@x.test?subject=%D8%B3%D9%84%D8%A7%D9%85%20%&body=caf\u{e9}%2").unwrap();
        assert_eq!(m.subject, "سلام %");
        assert_eq!(m.body, "café%2");
    }

    #[test]
    fn builds_mime_with_threading_headers() {
        let draft = Draft {
            id: "d".into(),
            account_id: "acc".into(),
            to: vec![parse_address("Bob <bob@x.test>").unwrap()],
            cc: vec![],
            bcc: vec![parse_address("hidden@x.test").unwrap()],
            subject: "Re: Plans".into(),
            html_body: "<p>Sounds good</p>".into(),
            quoted_html: "<blockquote>old</blockquote>".into(),
            attachments: vec![OutgoingAttachment {
                filename: "a.txt".into(),
                mime_type: "text/plain".into(),
                data_base64: "aGVsbG8=".into(),
            }],
            reply_to_message_id: None,
            thread_id: None,
            reply_kind: None,
            signature_id: None,
            updated_at: 0,
        };
        let refs = vec!["root@x.test".to_string(), "orig@x.test".to_string()];
        let (bytes, rcpt) = build_message(
            &draft,
            &OutgoingMeta {
                from: EmailAddress::new("Me", "me@x.test"),
                message_id: "new@x.test".into(),
                in_reply_to: Some("orig@x.test"),
                references: &refs,
            },
        )
        .unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("In-Reply-To: <orig@x.test>"), "{s}");
        assert!(s.contains("References: <root@x.test> <orig@x.test>"), "{s}");
        assert!(s.contains("Message-ID: <new@x.test>"));
        assert!(!s.contains("hidden@x.test"), "Bcc must not appear in headers");
        assert!(rcpt.contains(&"hidden@x.test".to_string()));
        assert!(s.contains("<blockquote>old</blockquote>"));
        let parsed = crate::mime::parse(&bytes).unwrap();
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.subject, "Re: Plans");
    }

    #[test]
    fn body_images_go_out_as_inline_parts() {
        let png = "iVBORw0KGgo=";
        let html = format!(r#"<p>Look</p><img src="data:image/png;base64,{png}" width="40"><img alt='x' src='data:image/png;base64,{png}'>"#);
        let (out, parts) = inline_images(&html, "m@x.test");
        assert_eq!(parts.len(), 1, "the same picture is attached once");
        assert_eq!(parts[0].0, "img1.m@x.test");
        assert_eq!(parts[0].1, "image/png");
        assert_eq!(out.matches(r#"src="cid:img1.m@x.test""#).count(), 2, "{out}");
        assert!(out.contains(r#"width="40""#));

        let draft = Draft {
            id: "d1".into(),
            account_id: "acc".into(),
            to: vec![parse_address("bob@x.test").unwrap()],
            cc: vec![],
            bcc: vec![],
            subject: "Pic".into(),
            html_body: html,
            quoted_html: String::new(),
            attachments: vec![],
            reply_to_message_id: None,
            thread_id: None,
            reply_kind: None,
            signature_id: None,
            updated_at: 0,
        };
        let meta = OutgoingMeta { from: EmailAddress::new("Me", "me@x.test"), message_id: "m@x.test".into(), in_reply_to: None, references: &[] };
        let (bytes, _) = build_message(&draft, &meta).unwrap();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("multipart/related"), "{s}");
        assert!(s.contains("Content-ID: <img1.m@x.test>"), "{s}");
        assert!(!s.contains("data:image"), "{s}");
        let parsed = crate::mime::parse(&bytes).unwrap();
        assert_eq!(parsed.attachments.iter().filter(|a| a.inline).count(), 1);
        assert!(parsed.attachments.iter().all(|a| a.inline));
    }
}
