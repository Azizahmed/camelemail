//! Where the Remail server is: the build's default, a URL or host the user
//! typed, or an organisation code looked up in the hosted directory.

use super::CloudError;

/// The hosted Remail service (deploy/server/README.md).
pub const HOSTED_SERVER: &str = "https://remail-63jemz.cranl.net";

/// Server the build points at: `REMAIL_SERVER` at build time (branded and
/// on-prem builds), else the hosted service. Debug builds use a local
/// development server instead.
pub fn default_server() -> Option<String> {
    let built = option_env!("REMAIL_SERVER").map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    built.or_else(|| Some(if cfg!(debug_assertions) { "http://localhost:8080" } else { HOSTED_SERVER }.to_string()))
}

/// Where organisation codes are looked up (`REMAIL_ORG_DIRECTORY` at build
/// time), else the default server.
pub fn org_directory() -> Option<String> {
    option_env!("REMAIL_ORG_DIRECTORY").map(str::trim).filter(|s| !s.is_empty()).map(String::from).or_else(default_server)
}

/// What the user typed on the "Using Remail at work?" screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    /// A server base URL, already checked (`https://…`, or `http://` on this machine).
    Server(String),
    /// An organisation code to look up.
    OrgCode(String),
}

pub fn parse(input: &str) -> Result<Address, CloudError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(bad("Enter your organisation's code or server address"));
    }
    if s.contains("://") {
        return check_url(s).map(Address::Server);
    }
    let host_part = s.split('/').next().unwrap_or(s);
    let host_only = host_part.rsplit_once(':').map(|(h, _)| h).unwrap_or(host_part);
    if is_local(host_only) {
        return check_url(&format!("http://{s}")).map(Address::Server);
    }
    if s.contains('.') || s.contains(':') || s.contains('/') {
        return check_url(&format!("https://{s}")).map(Address::Server);
    }
    if s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Ok(Address::OrgCode(s.to_ascii_lowercase()));
    }
    Err(bad("That isn't an organisation code or a server address"))
}

/// A server base URL as Remail uses it: `https`, or plain `http` only for
/// this machine (an on-premises server must use TLS); no query, no trailing slash.
pub fn check_url(raw: &str) -> Result<String, CloudError> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|_| bad("That server address isn't valid"))?;
    let host = url.host_str().unwrap_or_default().to_string();
    if host.is_empty() {
        return Err(bad("That server address has no host name"));
    }
    match url.scheme() {
        "https" => {}
        "http" if is_local(&host) => {}
        "http" => return Err(bad("The Remail server must use https:// (TLS)")),
        _ => return Err(bad("The server address must start with https://")),
    }
    if url.query().is_some() || url.fragment().is_some() || !url.username().is_empty() {
        return Err(bad("That server address isn't valid"));
    }
    let mut out = format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default());
    if let Some(port) = url.port() {
        out.push_str(&format!(":{port}"));
    }
    out.push_str(url.path().trim_end_matches('/'));
    Ok(out)
}

fn is_local(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    h == "localhost" || h == "127.0.0.1" || h == "::1"
}

fn bad(msg: &str) -> CloudError {
    CloudError::new("bad_request", msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(parse("https://mail.acme.com/").unwrap(), Address::Server("https://mail.acme.com".into()));
        assert_eq!(parse("mail.acme.com").unwrap(), Address::Server("https://mail.acme.com".into()));
        assert_eq!(parse("mail.acme.com:8443/remail").unwrap(), Address::Server("https://mail.acme.com:8443/remail".into()));
        assert_eq!(parse("http://localhost:8080").unwrap(), Address::Server("http://localhost:8080".into()));
        assert_eq!(parse("127.0.0.1:8080").unwrap(), Address::Server("http://127.0.0.1:8080".into()));
        assert_eq!(parse("http://[::1]:9").unwrap(), Address::Server("http://[::1]:9".into()));
        assert_eq!(parse(" Acme-Corp ").unwrap(), Address::OrgCode("acme-corp".into()));
        // On-premises servers must use TLS, in any build.
        assert!(parse("http://mail.acme.com").unwrap_err().message.contains("https"));
        assert!(parse("ftp://x.com").is_err());
        assert!(parse("https://x.com/?a=b").is_err());
        assert!(parse("").is_err());
        assert!(parse("not a code!").is_err());
    }
}
