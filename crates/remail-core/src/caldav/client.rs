//! CalDAV (RFC 4791) client over reqwest with Basic auth.
//!
//! Discovery follows RFC 6764 (`/.well-known/caldav`, current-user-principal,
//! calendar-home-set); redirects are followed manually so the WebDAV method
//! and body survive them. Credentials only follow redirects within the first
//! request's origin, and https never redirects to plain http.

use std::time::Duration;

use anyhow::{anyhow, Result};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE, ETAG, LOCATION};
use reqwest::{Method, StatusCode, Url};
use serde::{Deserialize, Serialize};

const DAV: &str = "DAV:";
const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
const CS: &str = "http://calendarserver.org/ns/";
const APPLE: &str = "http://apple.com/ns/ical/";
const MAX_REDIRECTS: usize = 5;
/// Deepest XML element nesting kept from a server reply (real replies nest about 8).
const MAX_XML_DEPTH: usize = 64;
/// Replies nested deeper are refused before parsing (the XML parser recurses per level).
const MAX_XML_NESTING: usize = 256;
const MULTIGET_CHUNK: usize = 50;

/// Account credentials (the mail username and app password).
#[derive(Debug, Clone)]
pub struct DavCredentials {
    pub username: String,
    pub password: String,
    pub allow_invalid_certs: bool,
}

/// A calendar collection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DavCalendar {
    /// Absolute URL.
    pub href: String,
    pub name: String,
    /// `#RRGGBB`.
    pub color: Option<String>,
    pub ctag: Option<String>,
    pub sync_token: Option<String>,
    pub read_only: bool,
    pub supports_events: bool,
}

/// A calendar object resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DavObject {
    /// Absolute URL.
    pub href: String,
    pub etag: String,
    pub ics: String,
}

/// Changes reported by a sync-collection REPORT (RFC 6578).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DavChanges {
    pub sync_token: Option<String>,
    /// (absolute href, etag) of new or modified members.
    pub changed: Vec<(String, String)>,
    /// Absolute hrefs of removed members.
    pub deleted: Vec<String>,
}

/// Errors callers may want to branch on (returned inside `anyhow::Error`).
#[derive(Debug, thiserror::Error)]
pub enum DavError {
    #[error("the server rejected the username or password")]
    Auth,
    #[error("not found")]
    NotFound,
    #[error("the event was changed elsewhere")]
    Conflict,
    #[error("{0}")]
    Http(String),
}

/// A response with its body read.
struct Reply {
    url: Url,
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

/// CalDAV client for one account.
pub struct CalDavClient {
    http: reqwest::Client,
    creds: DavCredentials,
    /// OAuth access token sent instead of the password (Google Calendar).
    bearer: Option<String>,
}

impl CalDavClient {
    /// Build a client (30 s timeout, user agent "Remail", no automatic redirects).
    pub fn new(creds: DavCredentials) -> Result<CalDavClient> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(15))
            .user_agent("Remail")
            .redirect(reqwest::redirect::Policy::none())
            .danger_accept_invalid_certs(creds.allow_invalid_certs)
            .build()?;
        Ok(CalDavClient { http, creds, bearer: None })
    }

    /// A client that signs in with an OAuth access token (`Authorization: Bearer`).
    pub fn with_bearer(token: String, allow_invalid_certs: bool) -> Result<CalDavClient> {
        let mut c = Self::new(DavCredentials { username: String::new(), password: String::new(), allow_invalid_certs })?;
        c.bearer = Some(token);
        Ok(c)
    }

    /// Candidate start URLs for discovery: provider presets first, then RFC 6764 well-known URLs.
    pub fn guess_start_urls(email: &str, imap_host: &str) -> Vec<String> {
        let domain = email
            .rsplit_once('@')
            .map(|(_, d)| d)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let host = imap_host.trim().trim_end_matches('.').to_ascii_lowercase();
        let either = |f: &dyn Fn(&str) -> bool| f(&domain) || f(&host);
        let ends = |s: &str, suffixes: &[&str]| {
            suffixes
                .iter()
                .any(|x| s == *x || s.ends_with(&format!(".{x}")))
        };
        let mut out: Vec<String> = Vec::new();
        if either(&|s| ends(s, &["icloud.com", "me.com", "mac.com"])) {
            out.push("https://caldav.icloud.com/".into());
        }
        if either(&|s| {
            ends(s, &["fastmail.com", "fastmail.fm", "messagingengine.com"])
                || s.starts_with("fastmail.")
        }) {
            out.push("https://caldav.fastmail.com/dav/".into());
        }
        if either(&|s| s.contains("zoho")) {
            let tld = [".com.au", ".eu", ".in", ".jp", ".com.cn", ".sa", ".ca"]
                .into_iter()
                .find(|t| host.ends_with(t) || domain.ends_with(t))
                .unwrap_or(".com");
            out.push(format!("https://calendar.zoho{tld}/caldav/"));
        }
        if either(&|s| s.contains("yahoo") || ends(s, &["ymail.com", "rocketmail.com"])) {
            out.push("https://caldav.calendar.yahoo.com/".into());
        }
        if either(&|s| ends(s, &["aol.com", "aim.com"])) {
            out.push("https://caldav.aol.com/".into());
        }
        if either(&|s| {
            s == "gmx.net" || s.ends_with(".gmx.net") || ends(s, &["gmx.de", "gmx.at", "gmx.ch"])
        }) {
            out.push("https://caldav.gmx.net/".into());
        } else if either(&|s| s.starts_with("gmx.") || s.contains(".gmx.")) {
            out.push("https://caldav.gmx.com/".into());
        }
        if either(&|s| ends(s, &["web.de"])) {
            out.push("https://caldav.web.de/".into());
        }
        if either(&|s| ends(s, &["mailbox.org"])) {
            out.push("https://dav.mailbox.org/".into());
        }
        if either(&|s| s.starts_with("posteo.") || s.contains(".posteo.")) {
            out.push("https://posteo.de:8443/".into());
        }
        if either(&|s| s.contains("yandex") || ends(s, &["ya.ru"])) {
            out.push("https://caldav.yandex.ru/".into());
        }
        if !domain.is_empty() {
            out.push(format!("https://{domain}/.well-known/caldav"));
        }
        let bare = host
            .strip_prefix("imap.")
            .or_else(|| host.strip_prefix("mail."))
            .unwrap_or(&host);
        if !bare.is_empty() {
            out.push(format!("https://{bare}/.well-known/caldav"));
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|u| seen.insert(u.clone()));
        out
    }

    /// Find the calendar home URL starting from `start_url`.
    pub async fn discover(&self, start_url: &str) -> Result<String> {
        const BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:current-user-principal/><c:calendar-home-set/><d:resourcetype/></d:prop></d:propfind>"#;
        let reply = self.propfind(start_url, "0", BODY).await?;
        let base = reply.url.clone();
        let responses = parse_multistatus(&reply.body, &base)?;
        let first = responses.first();
        if let Some(home) = first.and_then(|r| r.href_prop(CALDAV, "calendar-home-set", &base)) {
            return Ok(home);
        }
        if let Some(principal) =
            first.and_then(|r| r.href_prop(DAV, "current-user-principal", &base))
        {
            let reply = self.propfind(&principal, "0", BODY).await?;
            let responses = parse_multistatus(&reply.body, &reply.url)?;
            if let Some(home) = responses
                .first()
                .and_then(|r| r.href_prop(CALDAV, "calendar-home-set", &reply.url))
            {
                return Ok(home);
            }
        }
        if first.is_some_and(|r| r.is_calendar()) {
            if let Ok(parent) = base.join("..") {
                return Ok(canonical(&parent));
            }
        }
        const LIST: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/></d:prop></d:propfind>"#;
        let reply = self.propfind(base.as_str(), "1", LIST).await?;
        if parse_multistatus(&reply.body, &reply.url)?
            .iter()
            .any(MsResponse::is_calendar)
        {
            return Ok(canonical(&reply.url));
        }
        Err(anyhow!(DavError::Http(format!(
            "no CalDAV calendars found at {start_url}"
        ))))
    }

    /// Calendars in a calendar home.
    pub async fn calendars(&self, home: &str) -> Result<Vec<DavCalendar>> {
        const BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:ic="http://apple.com/ns/ical/"><d:prop><d:resourcetype/><d:displayname/><ic:calendar-color/><cs:getctag/><d:sync-token/><c:supported-calendar-component-set/><d:current-user-privilege-set/></d:prop></d:propfind>"#;
        let reply = self.propfind(home, "1", BODY).await?;
        parse_calendars(&reply.body, &reply.url)
    }

    /// `(absolute href, etag)` of the calendar object resources in a calendar.
    pub async fn list_etags(&self, calendar_href: &str) -> Result<Vec<(String, String)>> {
        const BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:getetag/><d:getcontenttype/><d:resourcetype/></d:prop></d:propfind>"#;
        let reply = self.propfind(calendar_href, "1", BODY).await?;
        parse_etags(&reply.body, &reply.url)
    }

    /// Fetch objects with a calendar-multiget REPORT (chunks of 50; missing hrefs are skipped).
    pub async fn multiget(&self, calendar_href: &str, hrefs: &[String]) -> Result<Vec<DavObject>> {
        let base = Url::parse(calendar_href)?;
        let mut out = Vec::new();
        for chunk in hrefs.chunks(MULTIGET_CHUNK) {
            let mut body = String::from(
                r#"<?xml version="1.0" encoding="utf-8"?>
<c:calendar-multiget xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:getetag/><c:calendar-data/></d:prop>"#,
            );
            for href in chunk {
                let path = match base.join(href) {
                    Ok(u) if u.origin() == base.origin() => u.path().to_string(),
                    Ok(u) => u.to_string(),
                    Err(_) => href.clone(),
                };
                body.push_str(&format!("<d:href>{}</d:href>", xml_escape(&path)));
            }
            body.push_str("</c:calendar-multiget>");
            let reply = self
                .send("REPORT", calendar_href, Some("1"), Some(body), &[])
                .await?;
            expect_multistatus(&reply)?;
            for r in parse_multistatus(&reply.body, &reply.url)? {
                let Some(ics) = r.prop(CALDAV, "calendar-data").map(|e| e.text.clone()) else {
                    continue;
                };
                if ics.trim().is_empty() {
                    continue;
                }
                let etag = r
                    .prop(DAV, "getetag")
                    .map(|e| e.text.trim().to_string())
                    .unwrap_or_default();
                out.push(DavObject {
                    href: r.href,
                    etag,
                    ics,
                });
            }
        }
        Ok(out)
    }

    /// Upload an object. `create` sends `If-None-Match: *`, otherwise `If-Match` when `etag` is given.
    /// Returns the new ETag if the server sent one.
    pub async fn put(
        &self,
        href: &str,
        ics: &str,
        etag: Option<&str>,
        create: bool,
    ) -> Result<Option<String>> {
        let mut headers = vec![(
            CONTENT_TYPE.as_str(),
            "text/calendar; charset=utf-8".to_string(),
        )];
        if create {
            headers.push(("If-None-Match", "*".into()));
        } else if let Some(e) = etag.filter(|e| !e.is_empty()) {
            headers.push(("If-Match", quote_etag(e)));
        }
        let reply = self
            .send("PUT", href, None, Some(ics.to_string()), &headers)
            .await?;
        match reply.status.as_u16() {
            200..=299 => Ok(header_etag(&reply.headers)),
            401 | 403 => Err(anyhow!(DavError::Auth)),
            404 | 410 => Err(anyhow!(DavError::NotFound)),
            412 => Err(anyhow!(DavError::Conflict)),
            _ => Err(http_error("PUT", &reply)),
        }
    }

    /// Delete an object; a missing object counts as deleted.
    pub async fn delete(&self, href: &str, etag: Option<&str>) -> Result<()> {
        let mut headers = Vec::new();
        if let Some(e) = etag.filter(|e| !e.is_empty()) {
            headers.push(("If-Match", quote_etag(e)));
        }
        let reply = self.send("DELETE", href, None, None, &headers).await?;
        match reply.status.as_u16() {
            200..=299 | 404 | 410 => Ok(()),
            401 | 403 => Err(anyhow!(DavError::Auth)),
            412 => Err(anyhow!(DavError::Conflict)),
            _ => Err(http_error("DELETE", &reply)),
        }
    }

    /// Current ETag of an object (None if it does not exist).
    pub async fn etag_of(&self, href: &str) -> Result<Option<String>> {
        const BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:getetag/></d:prop></d:propfind>"#;
        let reply = self
            .send("PROPFIND", href, Some("0"), Some(BODY.into()), &[])
            .await?;
        if matches!(reply.status.as_u16(), 404 | 410) {
            return Ok(None);
        }
        expect_multistatus(&reply)?;
        let responses = parse_multistatus(&reply.body, &reply.url)?;
        Ok(responses
            .iter()
            .find_map(|r| r.prop(DAV, "getetag"))
            .map(|e| e.text.trim().to_string())
            .filter(|e| !e.is_empty()))
    }

    /// Create a calendar collection (MKCALENDAR) for events.
    pub async fn make_calendar(&self, href: &str, name: &str, color: Option<&str>) -> Result<()> {
        let color = color
            .map(|c| {
                format!(
                    r#"<ic:calendar-color xmlns:ic="{APPLE}">{}</ic:calendar-color>"#,
                    xml_escape(c)
                )
            })
            .unwrap_or_default();
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<c:mkcalendar xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:set><d:prop><d:displayname>{}</d:displayname>{color}<c:supported-calendar-component-set><c:comp name="VEVENT"/></c:supported-calendar-component-set></d:prop></d:set></c:mkcalendar>"#,
            xml_escape(name)
        );
        let reply = self.send("MKCALENDAR", href, None, Some(body), &[]).await?;
        match reply.status.as_u16() {
            200..=299 => Ok(()),
            401 | 403 => Err(anyhow!(DavError::Auth)),
            _ => Err(http_error("MKCALENDAR", &reply)),
        }
    }

    /// Incremental changes since `sync_token` (RFC 6578). `Ok(None)` means the
    /// token is no longer valid and the caller should fall back to [`Self::list_etags`].
    pub async fn sync_changes(
        &self,
        calendar_href: &str,
        sync_token: &str,
    ) -> Result<Option<DavChanges>> {
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<d:sync-collection xmlns:d="DAV:"><d:sync-token>{}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/><d:getcontenttype/></d:prop></d:sync-collection>"#,
            xml_escape(sync_token)
        );
        let reply = self
            .send("REPORT", calendar_href, Some("1"), Some(body), &[])
            .await?;
        if matches!(reply.status.as_u16(), 403 | 409 | 410)
            || reply.body.contains("valid-sync-token")
        {
            return Ok(None);
        }
        expect_multistatus(&reply)?;
        Ok(Some(parse_sync(&reply.body, &reply.url, calendar_href)?))
    }

    /// URL for a new event in a calendar: `<calendar_href>/<uuid>.ics`.
    pub fn new_href(calendar_href: &str) -> String {
        format!(
            "{}/{}.ics",
            calendar_href.trim_end_matches('/'),
            uuid::Uuid::new_v4()
        )
    }

    async fn propfind(&self, url: &str, depth: &str, body: &str) -> Result<Reply> {
        let reply = self
            .send("PROPFIND", url, Some(depth), Some(body.to_string()), &[])
            .await?;
        expect_multistatus(&reply)?;
        Ok(reply)
    }

    /// Send a request, following up to 5 redirects with the same method and body
    /// (credentials only while the redirects stay within the first origin).
    async fn send(
        &self,
        method: &str,
        url: &str,
        depth: Option<&str>,
        body: Option<String>,
        headers: &[(&str, String)],
    ) -> Result<Reply> {
        let method = Method::from_bytes(method.as_bytes())?;
        let mut url = Url::parse(url.trim())
            .map_err(|e| anyhow!(DavError::Http(format!("invalid URL {url}: {e}"))))?;
        let first = url.clone();
        let mut with_auth = true;
        for _ in 0..=MAX_REDIRECTS {
            let mut req = self.http.request(method.clone(), url.clone());
            if with_auth {
                req = match &self.bearer {
                    Some(token) => req.bearer_auth(token),
                    None => req.basic_auth(&self.creds.username, Some(&self.creds.password)),
                };
            }
            if let Some(d) = depth {
                req = req.header("Depth", d);
            }
            if let Some(b) = &body {
                if !headers
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                {
                    req = req.header(CONTENT_TYPE, "application/xml; charset=utf-8");
                }
                req = req.body(b.clone());
            }
            for (k, v) in headers {
                req = req.header(*k, v);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| anyhow!(DavError::Http(format!("{method} {url}: {e}"))))?;
            let status = resp.status();
            if status.is_redirection() && status != StatusCode::NOT_MODIFIED {
                let next = resp
                    .headers()
                    .get(LOCATION)
                    .and_then(|l| l.to_str().ok())
                    .and_then(|l| url.join(l).ok());
                if let Some(next) = next {
                    tracing::debug!(from = %url, to = %next, "caldav redirect");
                    with_auth = redirect_auth(&first, &url, &next)?;
                    url = next;
                    continue;
                }
            }
            let headers = resp.headers().clone();
            let body = resp.text().await.unwrap_or_default();
            if status == StatusCode::UNAUTHORIZED {
                if !with_auth {
                    // Our password stays with the server the user entered.
                    return Err(anyhow!(DavError::Http(format!(
                        "{} sent Remail on to {}, which asks for the password. To use that server, enter {} as the server address.",
                        origin_text(&first),
                        origin_text(&url),
                        without_query(&url)
                    ))));
                }
                return Err(anyhow!(DavError::Auth));
            }
            return Ok(Reply {
                url,
                status,
                headers,
                body,
            });
        }
        Err(anyhow!(DavError::Http(format!(
            "too many redirects from {url}"
        ))))
    }
}

/// Whether a redirect from `current` to `next` may carry the credentials:
/// only within the origin of the `first` request (or its https upgrade).
/// Leaving https for plain http is refused.
fn redirect_auth(first: &Url, current: &Url, next: &Url) -> Result<bool> {
    if current.scheme() == "https" && next.scheme() != "https" {
        return Err(anyhow!(DavError::Http(format!(
            "{} redirected to an insecure address ({}); Remail didn't follow it",
            origin_text(current),
            origin_text(next)
        ))));
    }
    let upgraded = first.scheme() == "http"
        && next.scheme() == "https"
        && first.host_str() == next.host_str()
        && next.port_or_known_default() == Some(443)
        && first.port_or_known_default() == Some(80);
    Ok(next.origin() == first.origin() || upgraded)
}

fn origin_text(url: &Url) -> String {
    url.origin().ascii_serialization()
}

fn without_query(url: &Url) -> String {
    let mut u = url.clone();
    u.set_query(None);
    u.set_fragment(None);
    u.to_string()
}

fn http_error(what: &str, reply: &Reply) -> anyhow::Error {
    let snippet: String = reply.body.chars().take(200).collect();
    anyhow!(DavError::Http(format!(
        "{what} {} failed: HTTP {} {}",
        reply.url,
        reply.status.as_u16(),
        snippet.trim()
    )))
}

fn expect_multistatus(reply: &Reply) -> Result<()> {
    match reply.status.as_u16() {
        207 => Ok(()),
        200 if reply.body.contains("multistatus") => Ok(()),
        401 => Err(anyhow!(DavError::Auth)),
        404 | 410 => Err(anyhow!(DavError::NotFound)),
        _ => Err(http_error("request to", reply)),
    }
}

fn header_etag(headers: &HeaderMap) -> Option<String> {
    headers
        .get(ETAG)
        .and_then(|v: &HeaderValue| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn quote_etag(etag: &str) -> String {
    let e = etag.trim();
    if e.starts_with('"') || e.starts_with("W/") {
        e.to_string()
    } else {
        format!("\"{e}\"")
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Owned XML element.
#[derive(Debug, Clone, Default)]
struct El {
    ns: String,
    name: String,
    text: String,
    attrs: Vec<(String, String)>,
    children: Vec<El>,
}

impl El {
    /// Copy an element; children nested beyond [`MAX_XML_DEPTH`] are dropped.
    fn from_node(n: roxmltree::Node) -> El {
        El::from_node_at(n, 0)
    }

    fn from_node_at(n: roxmltree::Node, depth: usize) -> El {
        El {
            ns: n.tag_name().namespace().unwrap_or("").to_string(),
            name: n.tag_name().name().to_string(),
            text: n
                .children()
                .filter(|c| c.is_text())
                .filter_map(|c| c.text())
                .collect(),
            attrs: n
                .attributes()
                .map(|a| (a.name().to_string(), a.value().to_string()))
                .collect(),
            children: if depth >= MAX_XML_DEPTH {
                Vec::new()
            } else {
                n.children()
                    .filter(|c| c.is_element())
                    .map(|c| El::from_node_at(c, depth + 1))
                    .collect()
            },
        }
    }

    fn is(&self, ns: &str, name: &str) -> bool {
        self.name == name && (self.ns == ns || self.ns.is_empty())
    }

    fn child(&self, ns: &str, name: &str) -> Option<&El> {
        self.children.iter().find(|c| c.is(ns, name))
    }

    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn descendants<'a>(&'a self, out: &mut Vec<&'a El>) {
        for c in &self.children {
            out.push(c);
            c.descendants(out);
        }
    }
}

/// One `<d:response>` of a multistatus.
#[derive(Debug, Clone)]
struct MsResponse {
    /// Absolute, canonical URL.
    href: String,
    /// Response-level status (used by sync-collection for deletions).
    status: Option<u16>,
    /// Properties from 2xx propstats.
    props: Vec<El>,
}

impl MsResponse {
    fn prop(&self, ns: &str, name: &str) -> Option<&El> {
        self.props.iter().find(|p| p.is(ns, name))
    }

    /// The (resolved) `<d:href>` inside a property.
    fn href_prop(&self, ns: &str, name: &str, base: &Url) -> Option<String> {
        let href = self.prop(ns, name)?.child(DAV, "href")?;
        resolve(base, &href.text)
    }

    fn resource_types(&self) -> Vec<&El> {
        self.prop(DAV, "resourcetype")
            .map(|r| r.children.iter().collect())
            .unwrap_or_default()
    }

    fn is_calendar(&self) -> bool {
        self.resource_types()
            .iter()
            .any(|t| t.is(CALDAV, "calendar"))
    }

    fn is_collection(&self) -> bool {
        self.resource_types()
            .iter()
            .any(|t| t.is(DAV, "collection"))
    }

    fn text(&self, ns: &str, name: &str) -> Option<String> {
        self.prop(ns, name)
            .map(|e| e.text.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

/// Parse a status line such as `HTTP/1.1 404 Not Found`.
fn status_code(line: &str) -> Option<u16> {
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Parse a multistatus body; hrefs are resolved against `base`. Empty bodies yield nothing.
fn parse_multistatus(body: &str, base: &Url) -> Result<Vec<MsResponse>> {
    let body = body.trim_start_matches('\u{feff}').trim();
    if body.is_empty() {
        return Ok(Vec::new());
    }
    if nested_too_deeply(body) {
        return Err(anyhow!(DavError::Http(
            "invalid XML from server: nested too deeply".into()
        )));
    }
    let opts = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let doc = roxmltree::Document::parse_with_options(body, opts)
        .map_err(|e| anyhow!(DavError::Http(format!("invalid XML from server: {e}"))))?;
    let mut out = Vec::new();
    for node in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "response")
    {
        let el = El::from_node(node);
        let Some(href) = el.child(DAV, "href").and_then(|h| resolve(base, &h.text)) else {
            continue;
        };
        let status = el.child(DAV, "status").and_then(|s| status_code(&s.text));
        let mut props = Vec::new();
        for ps in el.children.iter().filter(|c| c.is(DAV, "propstat")) {
            let code = ps
                .child(DAV, "status")
                .and_then(|s| status_code(&s.text))
                .unwrap_or(200);
            if (200..300).contains(&code) {
                if let Some(p) = ps.child(DAV, "prop") {
                    props.extend(p.children.iter().cloned());
                }
            }
        }
        out.push(MsResponse {
            href,
            status,
            props,
        });
    }
    Ok(out)
}

/// Does the element nesting of `xml` go beyond [`MAX_XML_NESTING`]? Errs on
/// the side of yes (`<` inside a DTD counts as an element).
fn nested_too_deeply(xml: &str) -> bool {
    let b = xml.as_bytes();
    let skip_past = |from: usize, end: &[u8]| {
        b[from..]
            .windows(end.len())
            .position(|w| w == end)
            .map_or(b.len(), |p| from + p + end.len())
    };
    let mut depth = 0usize;
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &b[i..];
        if rest.starts_with(b"<!--") {
            i = skip_past(i + 4, b"-->");
        } else if rest.starts_with(b"<![CDATA[") {
            i = skip_past(i + 9, b"]]>");
        } else if rest.starts_with(b"<?") || rest.starts_with(b"<!") {
            i = skip_past(i + 2, b">");
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            i = skip_past(i + 2, b">");
        } else {
            // A start tag: find its end outside quoted attribute values.
            let mut j = i + 1;
            let mut quote = None;
            while j < b.len() {
                match (quote, b[j]) {
                    (Some(q), c) if c == q => quote = None,
                    (None, c @ (b'"' | b'\'')) => quote = Some(c),
                    (None, b'>') => break,
                    _ => {}
                }
                j += 1;
            }
            if b.get(j.wrapping_sub(1)) != Some(&b'/') {
                depth += 1;
                if depth > MAX_XML_NESTING {
                    return true;
                }
            }
            i = j + 1;
        }
    }
    false
}

/// Resolve an href against a base URL and canonicalise its percent-encoding.
fn resolve(base: &Url, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    base.join(href).ok().map(|u| canonical(&u))
}

/// URL with a uniformly percent-encoded path (so `%40` and `@` compare equal).
fn canonical(url: &Url) -> String {
    let mut u = url.clone();
    let path = url.path();
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            match hex {
                Some(b) if b != b'/' && b != b'%' && b != b'?' && b != b'#' => {
                    decoded.push(b);
                    i += 3;
                    continue;
                }
                _ => {}
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    let mut encoded = String::with_capacity(decoded.len());
    let mut j = 0;
    while j < decoded.len() {
        let b = decoded[j];
        let keep = b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&b);
        if b == b'%'
            && j + 2 < decoded.len()
            && decoded[j + 1].is_ascii_hexdigit()
            && decoded[j + 2].is_ascii_hexdigit()
        {
            encoded.push('%');
            encoded.push(decoded[j + 1].to_ascii_uppercase() as char);
            encoded.push(decoded[j + 2].to_ascii_uppercase() as char);
            j += 3;
            continue;
        }
        if keep {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
        j += 1;
    }
    u.set_path(&encoded);
    u.to_string()
}

fn same_resource(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// Normalise a calendar colour to `#RRGGBB`.
fn normalize_color(c: &str) -> Option<String> {
    let hex = c.trim().strip_prefix('#')?;
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let rgb = match hex.len() {
        6 | 8 => hex[..6].to_string(),
        3 | 4 => hex[..3].chars().flat_map(|c| [c, c]).collect(),
        _ => return None,
    };
    Some(format!("#{}", rgb.to_ascii_uppercase()))
}

fn last_segment(href: &str) -> String {
    let path = Url::parse(href)
        .map(|u| u.path().to_string())
        .unwrap_or_else(|_| href.to_string());
    let seg = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    percent_decode(&seg)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(b) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Calendar collections from a Depth 1 PROPFIND of a calendar home.
fn parse_calendars(body: &str, base: &Url) -> Result<Vec<DavCalendar>> {
    let mut out = Vec::new();
    for r in parse_multistatus(body, base)? {
        if !r.is_calendar() {
            continue;
        }
        let supports_events = match r.prop(CALDAV, "supported-calendar-component-set") {
            Some(set) if !set.children.is_empty() => set.children.iter().any(|c| {
                c.is(CALDAV, "comp")
                    && c.attr("name")
                        .is_some_and(|n| n.eq_ignore_ascii_case("VEVENT"))
            }),
            _ => true,
        };
        let read_only = match r.prop(DAV, "current-user-privilege-set") {
            Some(set) => {
                let mut all = Vec::new();
                set.descendants(&mut all);
                !all.iter()
                    .any(|e| e.is(DAV, "write") || e.is(DAV, "write-content") || e.is(DAV, "all"))
            }
            None => false,
        };
        let name = r
            .text(DAV, "displayname")
            .unwrap_or_else(|| last_segment(&r.href));
        out.push(DavCalendar {
            color: r
                .text(APPLE, "calendar-color")
                .and_then(|c| normalize_color(&c)),
            ctag: r.text(CS, "getctag"),
            sync_token: r.text(DAV, "sync-token"),
            name,
            href: r.href,
            read_only,
            supports_events,
        });
    }
    Ok(out)
}

/// Members of a calendar from a Depth 1 PROPFIND.
fn parse_etags(body: &str, base: &Url) -> Result<Vec<(String, String)>> {
    let self_href = canonical(base);
    Ok(parse_multistatus(body, base)?
        .into_iter()
        .filter(|r| !same_resource(&r.href, &self_href) && !r.is_collection())
        .filter(|r| match r.text(DAV, "getcontenttype") {
            Some(ct) => ct.to_ascii_lowercase().contains("text/calendar"),
            None => {
                r.href.to_ascii_lowercase().ends_with(".ics") || r.prop(DAV, "getetag").is_some()
            }
        })
        .map(|r| {
            let etag = r.text(DAV, "getetag").unwrap_or_default();
            (r.href, etag)
        })
        .collect())
}

/// Result of a sync-collection REPORT.
fn parse_sync(body: &str, base: &Url, calendar_href: &str) -> Result<DavChanges> {
    let body_trim = body.trim_start_matches('\u{feff}').trim();
    let mut changes = DavChanges::default();
    if !body_trim.is_empty() && !nested_too_deeply(body_trim) {
        let opts = roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        };
        if let Ok(doc) = roxmltree::Document::parse_with_options(body_trim, opts) {
            changes.sync_token = doc
                .root_element()
                .children()
                .find(|n| n.is_element() && n.tag_name().name() == "sync-token")
                .and_then(|n| n.text())
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty());
        }
    }
    let self_href = Url::parse(calendar_href)
        .map(|u| canonical(&u))
        .unwrap_or_default();
    for r in parse_multistatus(body, base)? {
        if same_resource(&r.href, &self_href) || r.is_collection() {
            continue;
        }
        if matches!(r.status, Some(404) | Some(410)) {
            changes.deleted.push(r.href);
        } else if let Some(etag) = r.text(DAV, "getetag") {
            if r.text(DAV, "getcontenttype")
                .is_none_or(|ct| ct.to_ascii_lowercase().contains("text/calendar"))
            {
                changes.changed.push((r.href, etag));
            }
        }
    }
    Ok(changes)
}

#[cfg(test)]
pub(crate) mod test_access {
    //! Hooks for the XML parsing tests.
    use super::*;

    pub fn calendars(body: &str, base: &str) -> Result<Vec<DavCalendar>> {
        parse_calendars(body, &Url::parse(base)?)
    }

    pub fn etags(body: &str, base: &str) -> Result<Vec<(String, String)>> {
        parse_etags(body, &Url::parse(base)?)
    }

    /// (calendar-home-set, current-user-principal) of the first response.
    pub fn principal(body: &str, base: &str) -> Result<(Option<String>, Option<String>)> {
        let base = Url::parse(base)?;
        let r = parse_multistatus(body, &base)?;
        let first = r.first();
        Ok((
            first.and_then(|r| r.href_prop(CALDAV, "calendar-home-set", &base)),
            first.and_then(|r| r.href_prop(DAV, "current-user-principal", &base)),
        ))
    }

    pub fn objects(body: &str, base: &str) -> Result<Vec<(String, String, String)>> {
        let base = Url::parse(base)?;
        Ok(parse_multistatus(body, &base)?
            .into_iter()
            .filter_map(|r| {
                let ics = r.prop(CALDAV, "calendar-data")?.text.clone();
                let etag = r.text(DAV, "getetag").unwrap_or_default();
                Some((r.href, etag, ics))
            })
            .collect())
    }

    pub fn sync(body: &str, base: &str) -> Result<DavChanges> {
        parse_sync(body, &Url::parse(base)?, base)
    }

    /// Whether a redirect keeps the credentials (Err: refused).
    pub fn redirect_auth(first: &str, current: &str, next: &str) -> Result<bool> {
        super::redirect_auth(&Url::parse(first)?, &Url::parse(current)?, &Url::parse(next)?)
    }
}
