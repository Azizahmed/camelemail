//! Preparing message HTML for display and computing thread ids.

use std::collections::{HashMap, HashSet};

use base64::Engine;
use once_cell::sync::Lazy;
use regex::Regex;

use crate::mime::ParsedMessage;

static REMOTE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?i)(src|background)\s*=\s*["']?\s*https?:|url\(\s*["']?\s*https?:"#).unwrap());
static CID_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?i)cid:([^"'\s>;)]+)"#).unwrap());

/// Sanitise email HTML. Scripts, event handlers, forms and frames are
/// removed; styling is kept. The UI renders the result in a sandboxed
/// iframe with a CSP, so this is defence in depth, not the only barrier.
pub fn sanitize_html(html: &str) -> String {
    SANITIZER.clean(html).to_string()
}

/// Built once: the allow-lists are the same for every message.
static SANITIZER: Lazy<ammonia::Builder<'static>> = Lazy::new(|| {
    let tags: HashSet<&str> = [
        "a", "abbr", "address", "area", "article", "aside", "b", "bdi", "bdo", "big", "blockquote", "br", "caption",
        "center", "cite", "code", "col", "colgroup", "dd", "del", "details", "dfn", "div", "dl", "dt", "em",
        "figcaption", "figure", "font", "footer", "h1", "h2", "h3", "h4", "h5", "h6", "header", "hgroup", "hr", "i",
        "img", "ins", "kbd", "li", "map", "mark", "nav", "ol", "p", "pre", "q", "rp", "rt", "ruby", "s", "samp",
        "section", "small", "span", "strike", "strong", "style", "sub", "summary", "sup", "table", "tbody", "td",
        "tfoot", "th", "thead", "time", "tr", "tt", "u", "ul", "var", "wbr", "picture", "source",
    ]
    .into_iter()
    .collect();
    let generic: HashSet<&str> = [
        "align", "bgcolor", "border", "cellpadding", "cellspacing", "class", "color", "colspan", "dir", "face",
        "height", "hspace", "id", "lang", "rowspan", "size", "style", "title", "valign", "vspace", "width",
        "background", "role", "aria-label", "aria-hidden",
    ]
    .into_iter()
    .collect();
    let mut tag_attrs: HashMap<&str, HashSet<&str>> = HashMap::new();
    tag_attrs.insert("a", ["href", "name", "target"].into_iter().collect());
    tag_attrs.insert("img", ["src", "alt", "srcset"].into_iter().collect());
    tag_attrs.insert("source", ["srcset", "media", "type"].into_iter().collect());
    tag_attrs.insert("area", ["href", "shape", "coords", "alt"].into_iter().collect());
    tag_attrs.insert("map", ["name"].into_iter().collect());
    tag_attrs.insert("ol", ["start", "type"].into_iter().collect());
    tag_attrs.insert("td", ["nowrap"].into_iter().collect());
    let schemes: HashSet<&str> = ["http", "https", "mailto", "tel", "cid", "data"].into_iter().collect();

    let mut b = ammonia::Builder::default();
    b.tags(tags)
        .clean_content_tags(["script", "noscript", "iframe", "object", "embed", "template"].into_iter().collect())
        .generic_attributes(generic)
        .tag_attributes(tag_attrs)
        .url_schemes(schemes)
        .link_rel(Some("noopener noreferrer"))
        .strip_comments(true);
    b
});

pub fn has_remote_content(html: &str) -> bool {
    REMOTE_RE.is_match(html)
}

/// Replace `cid:` references with data URLs built from inline parts.
pub fn inline_cid_images(html: &str, parts: &HashMap<String, (String, Vec<u8>)>) -> String {
    if parts.is_empty() {
        return html.to_string();
    }
    CID_RE
        .replace_all(html, |caps: &regex::Captures<'_>| {
            let cid = caps[1].trim_matches(['<', '>']);
            match parts.get(cid) {
                Some((mime, bytes)) => {
                    format!("data:{};base64,{}", mime, base64::engine::general_purpose::STANDARD.encode(bytes))
                }
                None => caps[0].to_string(),
            }
        })
        .to_string()
}

/// Thread id: the root of the References chain (else In-Reply-To, else the
/// message's own Message-ID), lowercased. Subject is never used.
pub fn thread_id(p: &ParsedMessage, fallback: &str) -> String {
    let root = p
        .references
        .first()
        .or(p.in_reply_to.as_ref())
        .or(p.message_id.as_ref())
        .map(|s| s.trim().trim_matches(['<', '>']).to_lowercase())
        .filter(|s| !s.is_empty());
    match root {
        Some(r) => format!("t:{r}"),
        None => format!("t:{fallback}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_dangerous_content() {
        let out = sanitize_html(
            r#"<style>p{color:red}</style><p onclick="x()" style="color:blue">Hi<script>alert(1)</script></p>
               <a href="javascript:alert(1)">bad</a><a href="https://ok.test">ok</a><iframe src="https://x"></iframe>
               <img src="https://track.test/p.gif"><form><input></form>"#,
        );
        assert!(out.contains("<style>p{color:red}</style>"), "{out}");
        assert!(out.contains("style=\"color:blue\""));
        assert!(!out.contains("onclick") && !out.contains("script") && !out.contains("iframe"));
        assert!(!out.contains("javascript:"));
        assert!(out.contains("href=\"https://ok.test\""));
        assert!(!out.contains("<input"));
        assert!(has_remote_content(&out));
        assert!(!has_remote_content("<p>plain</p><img src=\"data:image/png;base64,AA\">"));
    }

    #[test]
    fn cid_inlining() {
        let mut parts = HashMap::new();
        parts.insert("logo@x".to_string(), ("image/png".to_string(), vec![1, 2, 3]));
        let out = inline_cid_images(r#"<img src="cid:logo@x"><img src="cid:other">"#, &parts);
        assert!(out.contains("data:image/png;base64,AQID"));
        assert!(out.contains("cid:other"));
    }

    #[test]
    fn thread_root() {
        let mut p = ParsedMessage { message_id: Some("C@x".into()), ..Default::default() };
        assert_eq!(thread_id(&p, "f"), "t:c@x");
        p.in_reply_to = Some("B@x".into());
        assert_eq!(thread_id(&p, "f"), "t:b@x");
        p.references = vec!["A@X".into(), "B@x".into()];
        assert_eq!(thread_id(&p, "f"), "t:a@x");
        assert_eq!(thread_id(&ParsedMessage::default(), "acc/INBOX/1/5"), "t:acc/INBOX/1/5");
    }
}
