//! Parsing raw RFC 5322 messages into Remail's models.

use mail_parser::{Address, HeaderValue, MessageParser, MimeHeaders};

use crate::classify::{self, Category};
use crate::model::{AttachmentMeta, EmailAddress, Unsubscribe};

/// Snippet length shown in thread list rows.
pub const SNIPPET_LEN: usize = 200;

/// Everything Remail keeps from a parsed message.
#[derive(Debug, Clone, Default)]
pub struct ParsedMessage {
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub from: EmailAddress,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    pub bcc: Vec<EmailAddress>,
    pub reply_to: Vec<EmailAddress>,
    /// `Date:` header as epoch millis (None when missing or unparsable).
    pub date: Option<i64>,
    pub html: Option<String>,
    pub text: Option<String>,
    pub snippet: String,
    pub attachments: Vec<AttachmentMeta>,
    pub unsubscribe: Option<Unsubscribe>,
    /// Smart Inbox bucket.
    pub category: Category,
    /// A one-time / verification code email.
    pub otp: bool,
}

impl ParsedMessage {
    pub fn has_attachments(&self) -> bool {
        self.attachments.iter().any(|a| !a.inline)
    }
}

pub fn parse(raw: &[u8]) -> Option<ParsedMessage> {
    let msg = MessageParser::default().parse(raw)?;

    let html = msg.body_html(0).map(|s| s.into_owned());
    let text = msg.body_text(0).map(|s| s.into_owned());
    let snippet = make_snippet(text.as_deref(), html.as_deref());

    let mut attachments = Vec::new();
    for (index, part) in msg.attachments().enumerate() {
        let mime_type = part
            .content_type()
            .map(|ct| match ct.subtype() {
                Some(sub) => format!("{}/{}", ct.ctype(), sub),
                None => ct.ctype().to_string(),
            })
            .unwrap_or_else(|| "application/octet-stream".into());
        let content_id = part.content_id().map(|s| s.trim_matches(['<', '>']).to_string());
        let is_inline_disposition = part
            .content_disposition()
            .map(|d| d.ctype().eq_ignore_ascii_case("inline"))
            .unwrap_or(false);
        let filename = part
            .attachment_name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| default_filename(&mime_type, index));
        attachments.push(AttachmentMeta {
            index,
            filename,
            size: part.len(),
            inline: content_id.is_some() && (is_inline_disposition || mime_type.starts_with("image/")),
            content_id,
            mime_type,
        });
    }

    let mut signals = classify::Signals::default();
    for (name, value) in msg.headers_raw() {
        signals.observe(name, value);
    }
    let from = msg.from().and_then(|a| addresses(a).into_iter().next()).unwrap_or_default();
    let subject = msg.subject().unwrap_or_default().trim().to_string();
    let category = classify::classify(&signals, &from, &subject);
    let otp = classify::is_one_time_code(&subject, &truncate_chars(text.as_deref().unwrap_or(&snippet), 600));

    Some(ParsedMessage {
        message_id: msg.message_id().map(normalize_message_id),
        in_reply_to: header_ids(msg.in_reply_to()).into_iter().next(),
        references: header_ids(msg.references()),
        subject,
        from,
        to: msg.to().map(addresses).unwrap_or_default(),
        cc: msg.cc().map(addresses).unwrap_or_default(),
        bcc: msg.bcc().map(addresses).unwrap_or_default(),
        reply_to: msg.reply_to().map(addresses).unwrap_or_default(),
        date: msg.date().map(|d| d.to_timestamp() * 1000),
        html,
        text,
        snippet,
        attachments,
        unsubscribe: parse_unsubscribe(
            msg.header_raw("List-Unsubscribe"),
            msg.header_raw("List-Unsubscribe-Post"),
        ),
        category,
        otp,
    })
}

/// Smart Inbox bucket from the headers alone (for re-classifying stored mail).
pub fn parse_headers_category(raw: &[u8]) -> Option<Category> {
    let msg = MessageParser::default().parse_headers(raw)?;
    let mut signals = classify::Signals::default();
    for (name, value) in msg.headers_raw() {
        signals.observe(name, value);
    }
    let from = msg.from().and_then(|a| addresses(a).into_iter().next()).unwrap_or_default();
    Some(classify::classify(&signals, &from, msg.subject().unwrap_or_default().trim()))
}

/// Raw bytes of the attachments at `indexes` (same order as
/// `ParsedMessage::attachments`), parsing the message once. Missing indexes
/// are left out.
pub fn attachments_bytes(raw: &[u8], indexes: &[usize]) -> Vec<(usize, Vec<u8>)> {
    let Some(msg) = MessageParser::default().parse(raw) else { return Vec::new() };
    msg.attachments()
        .enumerate()
        .filter(|(i, _)| indexes.contains(i))
        .map(|(i, part)| (i, part.contents().to_vec()))
        .collect()
}

fn default_filename(mime: &str, index: usize) -> String {
    let ext = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "application/pdf" => "pdf",
        "text/calendar" => "ics",
        "message/rfc822" => "eml",
        _ => "bin",
    };
    format!("attachment-{}.{}", index + 1, ext)
}

fn addresses(addr: &Address<'_>) -> Vec<EmailAddress> {
    addr.iter()
        .filter_map(|a| {
            let email = a.address()?.trim().to_string();
            if email.is_empty() {
                return None;
            }
            Some(EmailAddress {
                name: a.name().map(|n| n.trim().trim_matches('"').to_string()).unwrap_or_default(),
                email,
            })
        })
        .collect()
}

fn normalize_message_id(id: &str) -> String {
    id.trim().trim_matches(['<', '>']).to_string()
}

fn header_ids(v: &HeaderValue<'_>) -> Vec<String> {
    match v {
        HeaderValue::Text(t) => split_ids(t),
        HeaderValue::TextList(list) => list.iter().flat_map(|t| split_ids(t)).collect(),
        _ => Vec::new(),
    }
}

fn split_ids(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || c == ',')
        .map(normalize_message_id)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Plain-text preview: prefers the text part, falls back to stripped HTML,
/// collapses whitespace and drops quoted reply lines.
pub fn make_snippet(text: Option<&str>, html: Option<&str>) -> String {
    let source = match (text, html) {
        (Some(t), _) if !t.trim().is_empty() => t.to_string(),
        (_, Some(h)) => html_to_text(h),
        _ => String::new(),
    };
    let mut out = String::with_capacity(SNIPPET_LEN + 8);
    for line in source.lines() {
        let line = line.trim();
        if line.starts_with('>') {
            continue;
        }
        // Quoted history starts here ("On <date>, <name> wrote:").
        if is_attribution(line) || line.starts_with("---------- Forwarded message") {
            break;
        }
        for word in line.split_whitespace() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(word);
        }
        if out.chars().count() >= SNIPPET_LEN {
            break;
        }
    }
    truncate_chars(&out, SNIPPET_LEN)
}

fn is_attribution(line: &str) -> bool {
    let l = line.trim_end();
    l.starts_with("On ") && l.ends_with("wrote:") && l.len() < 300
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((idx, _)) => s[..idx].to_string(),
        None => s.to_string(),
    }
}

/// Render HTML to readable plain text (used for snippets and the text/plain
/// alternative of outgoing mail).
pub fn html_to_text(html: &str) -> String {
    html2text::from_read(html.as_bytes(), 100_000).unwrap_or_else(|_| strip_tags(html))
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Parse `List-Unsubscribe: <mailto:...>, <https://...>` plus the RFC 8058
/// `List-Unsubscribe-Post` marker.
pub fn parse_unsubscribe(header: Option<&str>, post: Option<&str>) -> Option<Unsubscribe> {
    let header = header?;
    let mut out = Unsubscribe::default();
    for entry in header.split(',') {
        let entry = entry.trim().trim_start_matches('<').trim_end_matches('>').trim();
        let lower = entry.to_ascii_lowercase();
        if lower.starts_with("mailto:") && out.mailto.is_none() {
            out.mailto = Some(entry.to_string());
        } else if (lower.starts_with("https://") || lower.starts_with("http://")) && out.http_url.is_none() {
            out.http_url = Some(entry.to_string());
        }
    }
    out.one_click = out.http_url.is_some()
        && post
            .map(|p| p.to_ascii_lowercase().replace(' ', "").contains("list-unsubscribe=one-click"))
            .unwrap_or(false);
    out.is_actionable().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "From: \"Alice Example\" <alice@example.com>\r\n\
To: Bob <bob@example.org>, carol@example.net\r\n\
Subject: Hello there\r\n\
Message-ID: <abc123@example.com>\r\n\
In-Reply-To: <parent@example.com>\r\n\
References: <root@example.com> <parent@example.com>\r\n\
Date: Tue, 14 Oct 2025 09:30:00 +0000\r\n\
List-Unsubscribe: <mailto:unsub@example.com?subject=stop>, <https://example.com/u/1>\r\n\
List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"b1\"\r\n\
\r\n\
--b1\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hi Bob,\r\n\
> quoted line\r\n\
This is the body.\r\n\
--b1\r\n\
Content-Type: application/pdf; name=\"report.pdf\"\r\n\
Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQK\r\n\
--b1--\r\n";

    #[test]
    fn parses_headers_and_parts() {
        let p = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(p.subject, "Hello there");
        assert_eq!(p.from, EmailAddress::new("Alice Example", "alice@example.com"));
        assert_eq!(p.to.len(), 2);
        assert_eq!(p.to[1].email, "carol@example.net");
        assert_eq!(p.message_id.as_deref(), Some("abc123@example.com"));
        assert_eq!(p.in_reply_to.as_deref(), Some("parent@example.com"));
        assert_eq!(p.references, vec!["root@example.com", "parent@example.com"]);
        assert_eq!(p.date, Some(1760434200000));
        assert_eq!(p.snippet, "Hi Bob, This is the body.");
        assert_eq!(p.attachments.len(), 1);
        assert_eq!(p.attachments[0].filename, "report.pdf");
        assert!(p.has_attachments());
        let u = p.unsubscribe.unwrap();
        assert!(u.one_click);
        assert_eq!(u.http_url.as_deref(), Some("https://example.com/u/1"));
        assert_eq!(u.mailto.as_deref(), Some("mailto:unsub@example.com?subject=stop"));
        assert_eq!(p.category, Category::Updates);
        assert_eq!(attachments_bytes(SAMPLE.as_bytes(), &[0, 7]), vec![(0, b"%PDF-1.4\n".to_vec())]);
        assert!(attachments_bytes(b"", &[0]).is_empty());
    }

    #[test]
    fn unsubscribe_without_post_is_not_one_click() {
        let u = parse_unsubscribe(Some("<https://x.test/u>"), None).unwrap();
        assert!(!u.one_click);
        assert!(parse_unsubscribe(Some("garbage"), None).is_none());
    }

    #[test]
    fn snippet_stops_at_quote() {
        let s = make_snippet(Some("Sounds good!\n\nOn Sun, Oct 18, 2026 at 10:56 PM, Sara <s@x> wrote:\n> earlier"), None);
        assert_eq!(s, "Sounds good!");
    }

    #[test]
    fn snippet_from_html() {
        let s = make_snippet(None, Some("<p>Hello <b>world</b></p><p>Second</p>"));
        assert!(s.contains("Hello") && s.contains("world") && s.contains("Second"), "{s}");
    }
}
