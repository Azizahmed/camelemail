//! OAuth 2.0 sign-in for mail servers that no longer take passwords:
//! Microsoft (Outlook.com, Hotmail, Live, MSN and Microsoft 365) and Google
//! (Gmail and Google Workspace).
//!
//! Authorization code flow with PKCE for a public (desktop) client. The
//! browser signs in at the provider and is sent back to a one-shot listener on
//! `http://localhost:<port>`; the code is exchanged for an access token (used
//! with SASL XOAUTH2 for IMAP and SMTP) and a refresh token (kept in the
//! keyring, never in the database).

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::store::now_ms;

pub const MICROSOFT: &str = "microsoft";
pub const GOOGLE: &str = "google";
/// Gmail (IMAP/SMTP over XOAUTH2) and Google Calendar (CalDAV with the same token).
pub const GOOGLE_SCOPE: &str = "https://mail.google.com/ https://www.googleapis.com/auth/calendar openid email";
/// Tenant id of personal Microsoft accounts (Outlook.com, Hotmail, Live, MSN).
pub const MICROSOFT_CONSUMER_TENANT: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";

/// An identity provider's endpoints and the scopes mail needs.
#[derive(Debug, Clone)]
pub struct Provider {
    pub id: &'static str,
    pub authorize_url: String,
    pub token_url: String,
    /// Mail's scopes: what the sign-in's own token is for.
    pub scope: String,
    /// Asked for at sign-in too, so tokens for them come from the same
    /// sign-in later (the calendar). A token is for one service at a time.
    pub also_consent: String,
    /// Extra parameters of the sign-in page (`prompt`, `access_type`).
    pub authorize_params: Vec<(&'static str, &'static str)>,
    /// Refreshes name the service's scopes (Microsoft: one resource per
    /// token); Google tokens cover every scope granted, so it's left out.
    pub refresh_with_scope: bool,
}

impl Provider {
    /// Personal and work/school accounts alike (the `common` authority).
    pub fn microsoft() -> Provider {
        Self::microsoft_at("https://login.microsoftonline.com/common")
    }

    /// `authority`: the endpoint base (tests point it at a local server).
    pub fn microsoft_at(authority: &str) -> Provider {
        let base = authority.trim_end_matches('/');
        Provider {
            id: MICROSOFT,
            authorize_url: format!("{base}/oauth2/v2.0/authorize"),
            token_url: format!("{base}/oauth2/v2.0/token"),
            scope: "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access openid email profile"
                .into(),
            also_consent: crate::graph::SCOPE.into(),
            authorize_params: vec![("prompt", "select_account")],
            refresh_with_scope: true,
        }
    }

    /// Gmail and Google Workspace.
    pub fn google() -> Provider {
        Provider {
            authorize_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            ..Self::google_at("https://oauth2.googleapis.com")
        }
    }

    /// Google endpoints under one base (tests point it at a local server):
    /// `<base>/o/oauth2/v2/auth` and `<base>/token`.
    pub fn google_at(base: &str) -> Provider {
        let base = base.trim_end_matches('/');
        Provider {
            id: GOOGLE,
            authorize_url: format!("{base}/o/oauth2/v2/auth"),
            token_url: format!("{base}/token"),
            scope: GOOGLE_SCOPE.into(),
            also_consent: String::new(),
            // A refresh token only comes with offline access, and again only on consent.
            authorize_params: vec![("access_type", "offline"), ("prompt", "consent")],
            refresh_with_scope: false,
        }
    }

    /// `Microsoft` / `Google`, for messages.
    pub fn display_name(&self) -> &'static str {
        if self.id == GOOGLE {
            "Google"
        } else {
            "Microsoft"
        }
    }

    /// Everything the user is asked to allow at sign-in.
    pub fn consent_scope(&self) -> String {
        let mut all: Vec<&str> = Vec::new();
        for s in self.scope.split_whitespace().chain(self.also_consent.split_whitespace()) {
            if !all.contains(&s) {
                all.push(s);
            }
        }
        all.join(" ")
    }
}

/// Who signed in (from the ID token).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub email: Option<String>,
    pub name: Option<String>,
    /// Microsoft tenant (`tid`): tells personal accounts from Microsoft 365.
    pub tenant: Option<String>,
    /// The user's object id (`oid`): stable even when the address changes.
    pub object_id: Option<String>,
    /// OpenID `sub` (Google's stable account id).
    pub sub: Option<String>,
}

impl Identity {
    /// The account's identity across sign-ins: Microsoft's `tid/oid`, else `sub`.
    pub fn subject(&self) -> Option<String> {
        match (self.tenant.as_deref(), self.object_id.as_deref()) {
            (Some(t), Some(o)) => Some(format!("{t}/{o}")),
            _ => self.sub.clone(),
        }
    }
}

/// What a sign-in or a refresh returned.
#[derive(Clone)]
pub struct Tokens {
    pub access_token: String,
    /// Providers may rotate it; keep the newest.
    pub refresh_token: Option<String>,
    /// Epoch ms.
    pub expires_at: i64,
    pub identity: Identity,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens").field("expires_at", &self.expires_at).field("identity", &self.identity).finish_non_exhaustive()
    }
}

/// What the keyring holds for an OAuth account.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    pub provider: String,
    pub client_id: String,
    pub refresh_token: String,
    /// `tid/oid` of the account that signed in (see `Identity::subject`).
    #[serde(default)]
    pub subject: Option<String>,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant").field("provider", &self.provider).field("subject", &self.subject).finish_non_exhaustive()
    }
}

/// The account has to be signed in again (an expired or revoked sign-in, or
/// a server that no longer takes its token). Counts as an authentication error.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SignInRequired(pub String);

/// The provider refused (e.g. `invalid_grant` when the user revoked access).
#[derive(Debug, thiserror::Error)]
#[error("{description}")]
pub struct OAuthError {
    pub code: String,
    pub description: String,
    /// The provider's own first line (e.g. `AADSTS65004: User declined…`).
    pub detail: String,
}

impl OAuthError {
    /// The user, or their organization, didn't allow everything asked for
    /// (declined consent, or an administrator must approve the app).
    pub fn consent_refused(&self) -> bool {
        self.code == "consent_required" || ["AADSTS65001", "AADSTS65004", "AADSTS90094", "AADSTS90095"].iter().any(|c| self.detail.contains(c))
    }

    /// The user has to sign in again (rather than a passing failure).
    pub fn needs_sign_in(&self) -> bool {
        matches!(self.code.as_str(), "invalid_grant" | "interaction_required" | "consent_required" | "login_required" | "invalid_client" | "unauthorized_client")
    }
}

/// SASL XOAUTH2 (before base64): `user=<user>^Aauth=Bearer <token>^A^A`.
pub fn xoauth2(user: &str, access_token: &str) -> String {
    format!("user={user}\x01auth=Bearer {access_token}\x01\x01")
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random_token(bytes: usize) -> String {
    use ring::rand::SecureRandom;
    let mut buf = vec![0u8; bytes];
    ring::rand::SystemRandom::new().fill(&mut buf).expect("system random");
    b64url(&buf)
}

/// PKCE S256 challenge for a verifier (RFC 7636).
pub fn pkce_challenge(verifier: &str) -> String {
    b64url(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref())
}

/// The claims we use from an ID token. Its signature isn't checked: it came
/// straight from the token endpoint over TLS and only names the account.
pub fn identity_from_id_token(id_token: &str) -> Identity {
    let Some(payload) = id_token.split('.').nth(1) else { return Identity::default() };
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')) else {
        return Identity::default();
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return Identity::default() };
    let s = |k: &str| v[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    Identity {
        email: s("email").or_else(|| s("preferred_username").filter(|u| u.contains('@'))).map(|e| e.to_lowercase()),
        name: s("name"),
        tenant: s("tid"),
        object_id: s("oid"),
        sub: s("sub"),
    }
}

/// A sign-in waiting for the browser to come back.
pub struct Pending {
    /// Open this in the user's browser.
    pub url: String,
    verifier: String,
    state: String,
    redirect_uri: String,
    v4: TcpListener,
    v6: Option<TcpListener>,
}

/// Start a sign-in: listen on a free localhost port and build the URL to open.
pub async fn begin(provider: &Provider, client_id: &str, login_hint: Option<&str>) -> Result<Pending> {
    let (v4, v6) = loopback_listeners().await?;
    let port = v4.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{port}");
    let verifier = random_token(48);
    let state = random_token(16);
    let mut url = reqwest::Url::parse(&provider.authorize_url)?;
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("response_mode", "query")
        .append_pair("scope", &provider.consent_scope())
        .append_pair("state", &state)
        .append_pair("code_challenge", &pkce_challenge(&verifier))
        .append_pair("code_challenge_method", "S256");
    for (k, v) in &provider.authorize_params {
        url.query_pairs_mut().append_pair(k, v);
    }
    if let Some(hint) = login_hint.filter(|h| !h.is_empty()) {
        url.query_pairs_mut().append_pair("login_hint", hint);
    }
    Ok(Pending { url: url.into(), verifier, state, redirect_uri, v4, v6 })
}

/// The browser came back with a code: what a token exchange needs.
pub struct Authorized {
    pub code: String,
    pub verifier: String,
    pub redirect_uri: String,
}

impl std::fmt::Debug for Authorized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authorized").field("redirect_uri", &self.redirect_uri).finish_non_exhaustive()
    }
}

impl Pending {
    /// Wait for the browser to come back (up to `timeout`), then trade the code for tokens.
    pub async fn finish(self, http: &reqwest::Client, provider: &Provider, client_id: &str, timeout: Duration) -> Result<Tokens> {
        let a = self.authorized(timeout).await?;
        exchange(http, provider, client_id, &a).await
    }

    /// Wait for the browser to come back (up to `timeout`) with a code, for
    /// an exchange done elsewhere (the Remail server holds Google's client secret).
    pub async fn authorized(self, timeout: Duration) -> Result<Authorized> {
        let code = tokio::time::timeout(timeout, self.wait_for_code())
            .await
            .map_err(|_| anyhow!("The sign-in wasn't finished in time. Please try again."))??;
        Ok(Authorized { code, verifier: self.verifier, redirect_uri: self.redirect_uri })
    }

    /// Serve the redirect. Connections are handled side by side, each with a
    /// short deadline, so an idle or slow one can't hold up the browser; only
    /// a request carrying this sign-in's `state` ends the wait.
    async fn wait_for_code(&self) -> Result<String> {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<String>>(4);
        loop {
            let sock = tokio::select! {
                r = self.v4.accept() => r?.0,
                r = async { self.v6.as_ref().expect("guarded").accept().await }, if self.v6.is_some() => r?.0,
                Some(outcome) = rx.recv() => return outcome,
            };
            let (tx, state) = (tx.clone(), self.state.clone());
            tokio::spawn(async move {
                if let Ok(Some(outcome)) = tokio::time::timeout(Duration::from_secs(5), serve_redirect(sock, &state)).await {
                    let _ = tx.send(outcome).await;
                }
            });
        }
    }
}

/// A free port on 127.0.0.1 and, where IPv6 exists, the same port on ::1
/// (browsers may try ::1 for "localhost" first). If another program holds
/// that port on ::1, the redirect could reach it instead: pick another port.
async fn loopback_listeners() -> Result<(TcpListener, Option<TcpListener>)> {
    for _ in 0..20 {
        let v4 = TcpListener::bind(("127.0.0.1", 0)).await.context("couldn't open a local port for the sign-in")?;
        let port = v4.local_addr()?.port();
        match TcpListener::bind(("::1", port)).await {
            Ok(v6) => return Ok((v4, Some(v6))),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            // No IPv6 here: "localhost" can only be 127.0.0.1.
            Err(_) => return Ok((v4, None)),
        }
    }
    Err(anyhow!("couldn't find a free local port for the sign-in"))
}

/// Answer one request on the redirect listener; Some(outcome) when it was this sign-in's redirect.
async fn serve_redirect(mut sock: tokio::net::TcpStream, state: &str) -> Option<Result<String>> {
    let mut buf = vec![0u8; 16 * 1024];
    let mut n = 0;
    while n < buf.len() {
        match sock.read(&mut buf[n..]).await {
            Ok(0) | Err(_) => break,
            Ok(read) => n += read,
        }
        if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buf[..n]).into_owned();
    let outcome = parse_redirect(&request, state);
    let (status, body) = match &outcome {
        Some(Ok(_)) => ("200 OK", page("You're signed in", "You can close this tab and go back to Remail.")),
        Some(Err(e)) => ("400 Bad Request", page("Sign-in didn't work", &format!("{e}. You can close this tab and try again in Remail."))),
        None => ("404 Not Found", String::new()),
    };
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(reply.as_bytes()).await;
    let _ = sock.shutdown().await;
    outcome
}

/// The code from the browser's redirect (`GET /?code=…&state=…`) or the error
/// the provider sent back. None for anything without this sign-in's `state`
/// (the favicon, or other programs and web pages poking the port).
fn parse_redirect(request: &str, state: &str) -> Option<Result<String>> {
    let target = request.lines().next()?.split_whitespace().nth(1)?;
    let url = reqwest::Url::parse(&format!("http://localhost{target}")).ok()?;
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    if q.get("state").map(String::as_str) != Some(state) {
        return None;
    }
    if let Some(err) = q.get("error") {
        let desc = q.get("error_description").map(|d| d.lines().next().unwrap_or("").to_string()).unwrap_or_default();
        let msg = if err == "access_denied" { "Sign-in was cancelled".to_string() } else if desc.is_empty() { err.clone() } else { desc };
        let detail = q.get("error_description").map(|d| d.lines().next().unwrap_or("").to_string()).unwrap_or_default();
        return Some(Err(OAuthError { code: err.clone(), description: msg, detail }.into()));
    }
    q.get("code").map(|code| Ok(code.clone()))
}

fn page(title: &str, text: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        "<!doctype html><meta charset=utf-8><title>Remail</title><body style=\"font:15px system-ui,sans-serif;margin:15vh auto;max-width:420px;text-align:center;color:#222\"><h2 style=\"color:#ff7c19\">{}</h2><p>{}</p></body>",
        esc(title),
        esc(text)
    )
}

/// Trade an authorization code for tokens (PKCE, public client).
pub async fn exchange(http: &reqwest::Client, provider: &Provider, client_id: &str, a: &Authorized) -> Result<Tokens> {
    token_request(
        http,
        provider,
        &[
            ("client_id", client_id),
            ("grant_type", "authorization_code"),
            ("code", &a.code),
            ("redirect_uri", &a.redirect_uri),
            ("code_verifier", &a.verifier),
            ("scope", &provider.scope),
        ],
    )
    .await
}

/// A new access token for `scope` (one service's scopes) from a refresh token.
pub async fn refresh(http: &reqwest::Client, provider: &Provider, client_id: &str, refresh_token: &str, scope: &str) -> Result<Tokens> {
    let mut form = vec![("client_id", client_id), ("grant_type", "refresh_token"), ("refresh_token", refresh_token)];
    if provider.refresh_with_scope {
        form.push(("scope", scope));
    }
    token_request(http, provider, &form).await
}

/// Tokens from a code exchange the Remail server did (Google with a client secret).
pub fn tokens_from_server(t: remail_proto::api::GoogleTokens) -> Tokens {
    Tokens {
        expires_at: now_ms() + t.expires_in.max(60) * 1000,
        identity: t.id_token.as_deref().map(identity_from_id_token).unwrap_or_default(),
        access_token: t.access_token,
        refresh_token: t.refresh_token,
    }
}

async fn token_request(http: &reqwest::Client, provider: &Provider, form: &[(&str, &str)]) -> Result<Tokens> {
    let mut encoder = reqwest::Url::parse("http://form.invalid/")?;
    encoder.query_pairs_mut().extend_pairs(form);
    let body = encoder.query().unwrap_or_default().to_string();
    let resp = http
        .post(&provider.token_url)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| e.without_url())
        .context("couldn't reach the sign-in service")?;
    let status = resp.status();
    let v: serde_json::Value = resp.json().await.context("the sign-in service sent an unreadable reply")?;
    if !status.is_success() || v.get("error").is_some() {
        let code = v["error"].as_str().unwrap_or("error").to_string();
        // Microsoft puts an "AADSTS…: message\r\nTrace ID…" block here; keep the first line.
        let description = v["error_description"].as_str().and_then(|d| d.lines().next()).unwrap_or(&code).trim().to_string();
        return Err(OAuthError { code, detail: description.clone(), description }.into());
    }
    let access_token = v["access_token"].as_str().filter(|t| !t.is_empty()).context("the sign-in service sent no access token")?.to_string();
    let expires_in = v["expires_in"].as_i64().or_else(|| v["expires_in"].as_str().and_then(|s| s.parse().ok())).unwrap_or(3600);
    Ok(Tokens {
        access_token,
        refresh_token: v["refresh_token"].as_str().map(String::from),
        expires_at: now_ms() + expires_in * 1000,
        identity: v["id_token"].as_str().map(identity_from_id_token).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636() {
        assert_eq!(pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let a = random_token(48);
        assert!(a.len() >= 43 && a.len() <= 128 && a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(a, random_token(48));
    }

    #[test]
    fn xoauth2_string() {
        // Microsoft's documented example.
        let s = xoauth2("test@contoso.onmicrosoft.com", "EwBAAl3BAAUFFpUAo7J3Ve0bjLBWZWCclRC3EoAA");
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(s),
            "dXNlcj10ZXN0QGNvbnRvc28ub25taWNyb3NvZnQuY29tAWF1dGg9QmVhcmVyIEV3QkFBbDNCQUFVRkZwVUFvN0ozVmUwYmpMQldaV0NjbFJDM0VvQUEBAQ=="
        );
    }

    #[test]
    fn id_token_claims() {
        let payload = b64url(br#"{"preferred_username":"Aziz@Hotmail.com","name":"Aziz","tid":"9188040d-6c67-4c5b-b112-36a304b66dad"}"#);
        let id = identity_from_id_token(&format!("eyJhbGciOiJub25lIn0.{payload}.sig"));
        assert_eq!(id.email.as_deref(), Some("aziz@hotmail.com"));
        assert_eq!(id.name.as_deref(), Some("Aziz"));
        assert_eq!(id.tenant.as_deref(), Some(MICROSOFT_CONSUMER_TENANT));
        let work = b64url(br#"{"preferred_username":"not-an-email","email":"a@contoso.com"}"#);
        assert_eq!(identity_from_id_token(&format!("x.{work}.y")).email.as_deref(), Some("a@contoso.com"));
        assert_eq!(identity_from_id_token("garbage"), Identity::default());
        assert_eq!(id.subject(), None, "no oid in that token");
        let full = b64url(br#"{"email":"a@b.c","tid":"t1","oid":"o1"}"#);
        assert_eq!(identity_from_id_token(&format!("x.{full}.y")).subject().as_deref(), Some("t1/o1"));
    }

    #[test]
    fn secrets_stay_out_of_debug_output() {
        let g = Grant { provider: "microsoft".into(), client_id: "cid".into(), refresh_token: "SECRET-RT".into(), subject: None };
        let t = Tokens { access_token: "SECRET-AT".into(), refresh_token: Some("SECRET-RT".into()), expires_at: 0, identity: Identity::default() };
        assert!(!format!("{g:?}{t:?}").contains("SECRET"));
    }

    #[tokio::test]
    async fn redirect_listener_ignores_strangers() {
        let p = begin(&Provider::microsoft_at("https://idp.invalid"), "cid", None).await.unwrap();
        let q: HashMap<String, String> = reqwest::Url::parse(&p.url).unwrap().query_pairs().into_owned().collect();
        let (redirect, state) = (q["redirect_uri"].clone(), q["state"].clone());
        let port = redirect.rsplit(':').next().unwrap().to_string();
        let wait = tokio::spawn(async move { p.wait_for_code().await });
        // An idle connection, a stranger's error and a wrong state don't end or block the wait.
        let _idle = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await.unwrap();
        for bad in ["/?error=access_denied", "/?code=x&state=nope", "/favicon.ico"] {
            let r = reqwest::get(format!("http://127.0.0.1:{port}{bad}")).await.unwrap();
            assert_eq!(r.status(), 404, "{bad}");
        }
        let ok = reqwest::get(format!("http://127.0.0.1:{port}/?code=the-code&state={state}")).await.unwrap();
        assert!(ok.text().await.unwrap().contains("signed in"));
        assert_eq!(wait.await.unwrap().unwrap(), "the-code");
    }

    #[test]
    fn redirect_requests() {
        let ok = parse_redirect("GET /?code=abc&state=s1&session_state=x HTTP/1.1\r\nHost: localhost\r\n\r\n", "s1");
        assert_eq!(ok.unwrap().unwrap(), "abc");
        assert!(parse_redirect("GET /favicon.ico HTTP/1.1\r\n\r\n", "s1").is_none());
        // Without this sign-in's state: ignored, so another program can't end the wait.
        assert!(parse_redirect("GET /?code=abc&state=other HTTP/1.1\r\n\r\n", "s1").is_none());
        assert!(parse_redirect("GET /?error=access_denied HTTP/1.1\r\n\r\n", "s1").is_none());
        let denied = parse_redirect("GET /?error=access_denied&error_description=The+user+cancelled&state=s1 HTTP/1.1\r\n\r\n", "s1").unwrap().unwrap_err();
        assert_eq!(denied.to_string(), "Sign-in was cancelled");
        let e = denied.downcast_ref::<OAuthError>().unwrap();
        assert_eq!(e.code, "access_denied");
        assert!(!e.consent_refused());
        let declined = parse_redirect("GET /?error=access_denied&error_description=AADSTS65004%3A+User+declined+to+consent&state=s1 HTTP/1.1\r\n\r\n", "s1").unwrap().unwrap_err();
        assert!(declined.downcast_ref::<OAuthError>().unwrap().consent_refused());
    }

    #[test]
    fn google_endpoints_and_identity() {
        let p = Provider::google();
        assert_eq!(p.authorize_url, "https://accounts.google.com/o/oauth2/v2/auth");
        assert_eq!(p.token_url, "https://oauth2.googleapis.com/token");
        assert_eq!(p.consent_scope(), GOOGLE_SCOPE);
        assert!(p.authorize_params.contains(&("access_type", "offline")) && p.authorize_params.contains(&("prompt", "consent")));
        let claims = b64url(br#"{"email":"Me@Gmail.com","sub":"1234567890","name":"Me"}"#);
        let id = identity_from_id_token(&format!("x.{claims}.y"));
        assert_eq!((id.email.as_deref(), id.subject().as_deref()), (Some("me@gmail.com"), Some("1234567890")));
    }

    #[test]
    fn microsoft_endpoints() {
        let p = Provider::microsoft();
        assert_eq!(p.authorize_url, "https://login.microsoftonline.com/common/oauth2/v2.0/authorize");
        assert!(p.scope.contains("https://outlook.office.com/IMAP.AccessAsUser.All") && p.scope.contains("offline_access"));
        // Sign-in asks for mail and calendar; mail's own token requests stay single-service.
        let all = p.consent_scope();
        assert!(all.contains("IMAP.AccessAsUser.All") && all.contains("https://graph.microsoft.com/Calendars.ReadWrite"));
        assert_eq!(all.matches("offline_access").count(), 1);
        assert!(!p.scope.contains("graph.microsoft.com"));
    }
}
