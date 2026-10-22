//! Checks and clean-up of what admins save: OAuth app IDs, servers, ports,
//! URLs and the AI setup. Each failure names the field, never echoes a
//! secret.

use remail_proto::org::{OrgAi, Providers, ServerPreset};

use crate::error::{AppError, AppResult};

const MAX_PRESETS: usize = 100;

fn bad(msg: impl Into<String>) -> AppError {
    AppError::bad_request(msg)
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` (hex).
pub fn is_guid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// A DNS name (`mail.acme.com`, `localhost`) or an IP address.
pub fn is_host(s: &str) -> bool {
    if s.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    !s.is_empty()
        && s.len() <= 253
        && s.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
}

/// A DNS name with at least one dot.
pub fn is_domain(s: &str) -> bool {
    s.contains('.') && s.parse::<std::net::IpAddr>().is_err() && is_host(s)
}

/// An `http(s)://` URL with a host.
pub fn is_http_url(s: &str) -> bool {
    reqwest::Url::parse(s).is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| !h.is_empty()))
}

fn is_google_client_id(s: &str) -> bool {
    s.len() <= 200
        && s.strip_suffix(".apps.googleusercontent.com")
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'))
}

fn is_tenant(s: &str) -> bool {
    matches!(s, "common" | "organizations" | "consumers") || is_guid(s) || is_domain(s)
}

/// A secret pasted by an admin: one token, no spaces or control characters.
pub fn secret(s: &str, what: &str) -> AppResult<String> {
    let s = s.trim();
    if s.len() > 2000 || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad(format!("The {what} must be one line without spaces (up to 2000 characters).")));
    }
    Ok(s.to_string())
}

fn security(s: &str, which: &str, name: &str) -> AppResult<String> {
    let s = s.trim().to_ascii_lowercase();
    if matches!(s.as_str(), "tls" | "starttls" | "none") {
        Ok(s)
    } else {
        Err(bad(format!("Server {name}: {which} security must be tls, starttls or none.")))
    }
}

fn preset(mut p: ServerPreset, i: usize) -> AppResult<ServerPreset> {
    p.name = p.name.trim().to_string();
    let label = if p.name.is_empty() { format!("#{}", i + 1) } else { format!("\"{}\"", p.name) };
    if p.name.is_empty() || p.name.chars().count() > 100 || has_control(&p.name) {
        return Err(bad(format!("Server {label} needs a name (up to 100 characters).")));
    }
    p.domain = p.domain.trim().trim_start_matches('@').to_ascii_lowercase();
    if !p.domain.is_empty() && !is_domain(&p.domain) {
        return Err(bad(format!("Server {label}: the e-mail domain must look like example.com.")));
    }
    p.imap_host = p.imap_host.trim().to_ascii_lowercase();
    p.smtp_host = p.smtp_host.trim().to_ascii_lowercase();
    if !is_host(&p.imap_host) {
        return Err(bad(format!("Server {label}: enter the IMAP server's host name.")));
    }
    if !is_host(&p.smtp_host) {
        return Err(bad(format!("Server {label}: enter the SMTP server's host name.")));
    }
    if p.imap_port == 0 || p.smtp_port == 0 {
        return Err(bad(format!("Server {label}: ports go from 1 to 65535.")));
    }
    p.imap_security = security(&p.imap_security, "IMAP", &label)?;
    p.smtp_security = security(&p.smtp_security, "SMTP", &label)?;
    p.caldav_url = p.caldav_url.trim().to_string();
    if !p.caldav_url.is_empty() && (p.caldav_url.len() > 500 || !is_http_url(&p.caldav_url)) {
        return Err(bad(format!("Server {label}: the CalDAV address must start with https://.")));
    }
    Ok(p)
}

/// Checks and tidies the providers an admin saves. The Google client secret
/// is handled by the caller.
pub fn providers(mut p: Providers) -> AppResult<Providers> {
    let g = &mut p.google_app;
    g.client_id = g.client_id.trim().to_string();
    g.tenant.clear();
    if !g.client_id.is_empty() && !is_google_client_id(&g.client_id) {
        return Err(bad("The Google client ID must end in .apps.googleusercontent.com."));
    }
    let m = &mut p.microsoft_app;
    m.client_id = m.client_id.trim().to_ascii_lowercase();
    m.client_secret.clear();
    if !m.client_id.is_empty() && !is_guid(&m.client_id) {
        return Err(bad("The Microsoft application (client) ID looks like 00000000-0000-0000-0000-000000000000."));
    }
    m.tenant = m.tenant.trim().to_ascii_lowercase();
    if m.tenant.is_empty() {
        m.tenant = "common".into();
    }
    if !is_tenant(&m.tenant) {
        return Err(bad("The Microsoft tenant must be common, organizations, consumers, a tenant ID or a domain."));
    }
    if p.presets.len() > MAX_PRESETS {
        return Err(bad(format!("Up to {MAX_PRESETS} servers can be listed.")));
    }
    p.presets = std::mem::take(&mut p.presets).into_iter().enumerate().map(|(i, x)| preset(x, i)).collect::<AppResult<_>>()?;
    Ok(p)
}

/// Checks and tidies the AI setup. While it's off, the fields may be blank.
pub fn ai(mut a: OrgAi) -> AppResult<OrgAi> {
    a.provider = a.provider.trim().to_ascii_lowercase();
    if !matches!(a.provider.as_str(), "openai" | "anthropic") {
        return Err(bad("The AI provider must be openai or anthropic."));
    }
    a.base_url = a.base_url.trim().trim_end_matches('/').to_string();
    a.model = a.model.trim().to_string();
    if !a.base_url.is_empty() && (a.base_url.len() > 500 || !is_http_url(&a.base_url)) {
        return Err(bad("The API URL must start with https:// (or http:// for a local server)."));
    }
    if a.model.chars().count() > 200 || has_control(&a.model) {
        return Err(bad("The model name is too long."));
    }
    if a.enabled && a.base_url.is_empty() {
        return Err(bad("Enter the provider's API URL to turn AI assistance on."));
    }
    if a.enabled && a.model.is_empty() {
        return Err(bad("Choose a model to turn AI assistance on."));
    }
    if !(256..=1_000_000).contains(&a.max_tokens) {
        return Err(bad("The answer length must be between 256 and 1,000,000 tokens."));
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_hosts() {
        assert!(is_guid("9832a9e1-5779-4e2f-b2cb-8d2bdf4e9b6e"));
        assert!(!is_guid("9832a9e1-5779-4e2f-b2cb-8d2bdf4e9b6"));
        assert!(!is_guid("9832a9e1x5779-4e2f-b2cb-8d2bdf4e9b6e"));
        assert!(is_host("mail.acme.com") && is_host("localhost") && is_host("10.0.0.5") && is_host("::1"));
        assert!(!is_host("mail acme.com") && !is_host("-x.com") && !is_host("a..b") && !is_host(""));
        assert!(is_domain("acme.com") && !is_domain("localhost") && !is_domain("10.0.0.5"));
        assert!(is_http_url("https://dav.acme.com/") && !is_http_url("ftp://x.com") && !is_http_url("acme.com"));
        assert!(is_google_client_id("123-abc.apps.googleusercontent.com"));
        assert!(!is_google_client_id(".apps.googleusercontent.com") && !is_google_client_id("123-abc"));
        assert!(is_tenant("common") && is_tenant("contoso.onmicrosoft.com") && is_tenant("9832a9e1-5779-4e2f-b2cb-8d2bdf4e9b6e"));
        assert!(!is_tenant("everyone"));
    }

    #[test]
    fn providers_are_tidied() {
        let mut p = Providers::default();
        p.microsoft_app.client_id = " 9832A9E1-5779-4E2F-B2CB-8D2BDF4E9B6E ".into();
        p.microsoft_app.tenant = String::new();
        p.presets.push(ServerPreset {
            name: " Acme ".into(),
            domain: "@ACME.com".into(),
            imap_host: "Mail.Acme.com".into(),
            smtp_host: "smtp.acme.com".into(),
            imap_security: "TLS".into(),
            ..Default::default()
        });
        let p = providers(p).unwrap();
        assert_eq!(p.microsoft_app.client_id, "9832a9e1-5779-4e2f-b2cb-8d2bdf4e9b6e");
        assert_eq!(p.microsoft_app.tenant, "common");
        assert_eq!((p.presets[0].name.as_str(), p.presets[0].domain.as_str()), ("Acme", "acme.com"));
        assert_eq!((p.presets[0].imap_host.as_str(), p.presets[0].imap_security.as_str()), ("mail.acme.com", "tls"));

        let mut bad_port = Providers::default();
        bad_port.presets.push(ServerPreset { name: "X".into(), imap_host: "a.b".into(), smtp_host: "a.b".into(), imap_port: 0, ..Default::default() });
        assert!(providers(bad_port).is_err());
        let mut bad_id = Providers::default();
        bad_id.google_app.client_id = "not-google".into();
        assert!(providers(bad_id).is_err());
    }

    #[test]
    fn ai_setup() {
        assert!(ai(OrgAi::default()).is_ok(), "off and blank is fine");
        let on = OrgAi { enabled: true, base_url: "https://api.openai.com/v1/".into(), model: "gpt-x".into(), ..Default::default() };
        assert_eq!(ai(on.clone()).unwrap().base_url, "https://api.openai.com/v1");
        assert!(ai(OrgAi { model: String::new(), ..on.clone() }).is_err());
        assert!(ai(OrgAi { provider: "gemini".into(), ..on.clone() }).is_err());
        assert!(ai(OrgAi { base_url: "api.openai.com".into(), ..on.clone() }).is_err());
        assert!(ai(OrgAi { max_tokens: 10, ..on }).is_err());
        assert!(secret("sk-abc", "API key").is_ok());
        assert!(secret("sk abc", "API key").is_err());
    }
}
