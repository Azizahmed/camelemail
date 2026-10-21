//! Mocks shared by the Microsoft tests: the identity platform's token
//! endpoint, IMAP/SMTP servers that only take the right XOAUTH2 token, and a
//! "browser" that signs in and follows the localhost redirect.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Router;
use base64::Engine as _;
use remail_core::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

pub const CALENDAR_SCOPE: &str = "https://graph.microsoft.com/Calendars.ReadWrite";

/// An organisation whose Microsoft sign-in uses the mock's app id.
pub fn microsoft_org() -> Option<remail_core::engine::OrgAccess> {
    let mut providers = remail_core::proto::org::Providers::default();
    providers.microsoft_app.client_id = " client-123 ".into();
    Some(remail_core::engine::OrgAccess { providers, google_server_exchange: false })
}

pub struct Quiet;
impl EventSink for Quiet {
    fn emit(&self, _: EngineEvent) {}
}

#[derive(Default)]
pub struct Idp {
    /// code → (PKCE challenge it was issued for, email, object id, calendar allowed).
    pub codes: HashMap<String, (String, String, String, bool)>,
    /// The scope of every sign-in page opened.
    pub authorize_scopes: Vec<String>,
    /// Refresh token → (mail access token it yields, rotated refresh token).
    pub refresh: HashMap<String, (String, String)>,
    /// Refresh tokens whose user allowed the calendar (others lack that consent).
    pub calendar_ok: HashSet<String>,
    /// (grant type, scope) of every token request.
    pub token_requests: Vec<(String, String)>,
}
pub type Shared = Arc<Mutex<Idp>>;

pub fn b64url(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

pub fn id_token(email: &str, oid: &str) -> String {
    let claims = format!(r#"{{"preferred_username":"{email}","name":"Aziz","tid":"9188040d-6c67-4c5b-b112-36a304b66dad","oid":"{oid}"}}"#);
    format!("{}.{}.sig", b64url(br#"{"alg":"none"}"#), b64url(claims.as_bytes()))
}

pub async fn token(State(s): State<Shared>, body: String) -> Response {
    let form: HashMap<String, String> = reqwest::Url::parse(&format!("http://x/?{body}")).unwrap().query_pairs().into_owned().collect();
    let mut idp = s.lock().unwrap();
    idp.token_requests.push((form["grant_type"].clone(), form["scope"].clone()));
    assert_eq!(form["client_id"], "client-123");
    let err = |code: &str, desc: &str| (axum::http::StatusCode::BAD_REQUEST, axum::Json(serde_json::json!({"error": code, "error_description": desc}))).into_response();
    let (mail, calendar) = (form["scope"].contains("https://outlook.office.com/IMAP.AccessAsUser.All"), form["scope"].contains("graph.microsoft.com"));
    // Like Microsoft: a token is for one service at a time.
    if mail == calendar {
        return err("invalid_request", "AADSTS28000: Provided value for the input parameter scope is not valid because it contains more than one resource.");
    }
    match form["grant_type"].as_str() {
        "authorization_code" if calendar => err("invalid_request", "a code is redeemed for mail"),
        "authorization_code" => {
            let Some((challenge, email, oid, calendar_allowed)) = idp.codes.remove(&form["code"]) else { return err("invalid_grant", "unknown code") };
            let verifier = &form["code_verifier"];
            let expected = b64url(ring_digest(verifier.as_bytes()).as_slice());
            if expected != challenge {
                return err("invalid_grant", "PKCE verification failed");
            }
            // A sign-in that didn't allow the calendar can't be used for it later.
            let refresh = if calendar_allowed { "rt-1" } else { "rt-mail-only" };
            axum::Json(serde_json::json!({
                "access_token": "at-1", "refresh_token": refresh, "expires_in": 3600, "token_type": "Bearer",
                "id_token": id_token(&email, &oid),
            }))
            .into_response()
        }
        "refresh_token" if calendar => {
            let rt = form["refresh_token"].clone();
            if idp.calendar_ok.contains(&rt) {
                axum::Json(serde_json::json!({"access_token": format!("gat-{rt}"), "refresh_token": rt, "expires_in": 3600})).into_response()
            } else {
                err("invalid_grant", "AADSTS65001: The user or administrator has not consented to use the application.\r\nTrace ID: 2")
            }
        }
        "refresh_token" => match idp.refresh.get(&form["refresh_token"]).cloned() {
            Some((access, rotated)) => axum::Json(serde_json::json!({"access_token": access, "refresh_token": rotated, "expires_in": 3600})).into_response(),
            None => err("invalid_grant", "AADSTS70008: The refresh token has expired.\r\nTrace ID: 1"),
        },
        _ => err("unsupported_grant_type", "?"),
    }
}

pub fn ring_digest(data: &[u8]) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA256, data).as_ref().to_vec()
}

/// IMAP that takes `AUTHENTICATE XOAUTH2` with an accepted token; records tokens seen.
pub async fn fake_imap(accepted: Arc<Mutex<Vec<String>>>, seen: Arc<Mutex<Vec<String>>>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (sock, _) = l.accept().await.unwrap();
            let (accepted, seen) = (accepted.clone(), seen.clone());
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut lines = BufReader::new(r).lines();
                w.write_all(b"* OK [CAPABILITY IMAP4rev1 AUTH=XOAUTH2] fake ready\r\n").await.unwrap();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut parts = line.splitn(3, ' ');
                    let (tag, cmd) = (parts.next().unwrap_or("*").to_string(), parts.next().unwrap_or("").to_uppercase());
                    let reply = match cmd.as_str() {
                        "AUTHENTICATE" => {
                            w.write_all(b"+ \r\n").await.unwrap();
                            let resp = lines.next_line().await.unwrap().unwrap_or_default();
                            let decoded = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(resp.trim()).unwrap_or_default()).unwrap_or_default();
                            let token = decoded.split("auth=Bearer ").nth(1).unwrap_or("").trim_end_matches('\x01').to_string();
                            // (An empty line is a client that went away mid-login, e.g. a dropped engine's worker.)
                            assert!(resp.is_empty() || decoded.starts_with("user="), "{decoded:?}");
                            seen.lock().unwrap().push(token.clone());
                            if accepted.lock().unwrap().contains(&token) { format!("{tag} OK AUTHENTICATE completed\r\n") } else { format!("{tag} NO AUTHENTICATE failed\r\n") }
                        }
                        "CAPABILITY" => format!("* CAPABILITY IMAP4rev1 IDLE\r\n{tag} OK done\r\n"),
                        "LIST" => format!("* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\n{tag} OK LIST done\r\n"),
                        "LOGOUT" => {
                            let _ = w.write_all(format!("* BYE\r\n{tag} OK bye\r\n").as_bytes()).await;
                            return;
                        }
                        _ => format!("{tag} NO not in this fake\r\n"),
                    };
                    if w.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

/// SMTP that takes `AUTH XOAUTH2` with an accepted token.
pub async fn fake_smtp(accepted: Arc<Mutex<Vec<String>>>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (sock, _) = l.accept().await.unwrap();
            let accepted = accepted.clone();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut lines = BufReader::new(r).lines();
                w.write_all(b"220 fake ESMTP\r\n").await.unwrap();
                while let Ok(Some(line)) = lines.next_line().await {
                    let upper = line.to_uppercase();
                    let reply = if upper.starts_with("EHLO") {
                        "250-fake\r\n250 AUTH XOAUTH2\r\n".to_string()
                    } else if upper.starts_with("AUTH XOAUTH2") {
                        let b64 = line.split_whitespace().nth(2).unwrap_or("");
                        let decoded = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(b64).unwrap_or_default()).unwrap_or_default();
                        let token = decoded.split("auth=Bearer ").nth(1).unwrap_or("").trim_end_matches('\x01').to_string();
                        if accepted.lock().unwrap().contains(&token) { "235 2.7.0 Authentication successful\r\n".into() } else { "535 5.7.3 Authentication unsuccessful\r\n".into() }
                    } else if upper.starts_with("QUIT") {
                        let _ = w.write_all(b"221 bye\r\n").await;
                        return;
                    } else {
                        "250 ok\r\n".into()
                    };
                    if w.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

/// Stands in for the browser: signs in at the "authorize" URL and follows the redirect.
pub fn browser(idp: Shared, code: &'static str) -> impl Fn(&str) -> anyhow::Result<()> {
    browser_as(idp, code, "Aziz@Outlook.com", "oid-aziz")
}

pub fn browser_as(idp: Shared, code: &'static str, email: &'static str, oid: &'static str) -> impl Fn(&str) -> anyhow::Result<()> {
    move |url: &str| {
        let u = reqwest::Url::parse(url)?;
        let q: HashMap<String, String> = u.query_pairs().into_owned().collect();
        assert_eq!(q["code_challenge_method"], "S256");
        assert!(q["scope"].contains("IMAP.AccessAsUser.All"), "{}", q["scope"]);
        assert_eq!(q["response_type"], "code");
        let mut state = idp.lock().unwrap();
        state.authorize_scopes.push(q["scope"].clone());
        state.codes.insert(code.to_string(), (q["code_challenge"].clone(), email.to_string(), oid.to_string(), q["scope"].contains(CALENDAR_SCOPE)));
        drop(state);
        let redirect = format!("{}?code={code}&state={}", q["redirect_uri"], q["state"]);
        tokio::spawn(async move {
            let page = reqwest::get(&redirect).await.unwrap().text().await.unwrap();
            assert!(page.contains("signed in"), "{page}");
        });
        Ok(())
    }
}

/// Serve `app` on a free local port.
pub async fn serve(app: Router) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    port
}

/// A user who doesn't allow the calendar: declines the first consent screen
/// (mail and calendar), then signs in when asked for mail alone.
pub fn browser_declining_calendar(idp: Shared, code: &'static str, email: &'static str, oid: &'static str) -> impl Fn(&str) -> anyhow::Result<()> {
    let sign_in = browser_as(idp.clone(), code, email, oid);
    move |url: &str| {
        let q: HashMap<String, String> = reqwest::Url::parse(url)?.query_pairs().into_owned().collect();
        if !q["scope"].contains(CALENDAR_SCOPE) {
            return sign_in(url);
        }
        idp.lock().unwrap().authorize_scopes.push(q["scope"].clone());
        let redirect = format!("{}?error=access_denied&error_description=AADSTS65004%3A+User+declined+to+consent+to+access+the+app.&state={}", q["redirect_uri"], q["state"]);
        tokio::spawn(async move {
            let _ = reqwest::get(&redirect).await;
        });
        Ok(())
    }
}

pub mod server;

pub fn google_id_token(email: &str, sub: &str) -> String {
    let claims = format!(r#"{{"email":"{email}","email_verified":true,"sub":"{sub}","name":"Me"}}"#);
    format!("{}.{}.sig", b64url(br#"{"alg":"none"}"#), b64url(claims.as_bytes()))
}

/// Google's token endpoint: PKCE-checked codes, and refreshes (which name no scope).
pub async fn google_token(State(s): State<Shared>, body: String) -> Response {
    let form: HashMap<String, String> = reqwest::Url::parse(&format!("http://x/?{body}")).unwrap().query_pairs().into_owned().collect();
    let mut idp = s.lock().unwrap();
    idp.token_requests.push((form["grant_type"].clone(), form.get("scope").cloned().unwrap_or_default()));
    assert_eq!(form["client_id"], "gclient");
    let err = |code: &str| (axum::http::StatusCode::BAD_REQUEST, axum::Json(serde_json::json!({"error": code, "error_description": code}))).into_response();
    match form["grant_type"].as_str() {
        "authorization_code" => {
            let Some((challenge, email, sub, _)) = idp.codes.remove(&form["code"]) else { return err("invalid_grant") };
            if b64url(&ring_digest(form["code_verifier"].as_bytes())) != challenge {
                return err("invalid_grant");
            }
            axum::Json(serde_json::json!({
                "access_token": "gat-1", "refresh_token": "grt-1", "expires_in": 3600, "id_token": google_id_token(&email, &sub),
            }))
            .into_response()
        }
        "refresh_token" if form.contains_key("scope") => err("invalid_scope"),
        "refresh_token" => match idp.refresh.get(&form["refresh_token"]).cloned() {
            Some((access, _)) => axum::Json(serde_json::json!({"access_token": access, "expires_in": 3600})).into_response(),
            None => err("invalid_grant"),
        },
        _ => err("unsupported_grant_type"),
    }
}

/// Stands in for the browser on Google's consent page.
pub fn google_browser(idp: Shared, code: &'static str, email: &'static str, sub: &'static str) -> impl Fn(&str) -> anyhow::Result<()> {
    move |url: &str| {
        let q: HashMap<String, String> = reqwest::Url::parse(url)?.query_pairs().into_owned().collect();
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["scope"], remail_core::oauth::GOOGLE_SCOPE);
        assert_eq!((q["access_type"].as_str(), q["prompt"].as_str()), ("offline", "consent"));
        idp.lock().unwrap().codes.insert(code.to_string(), (q["code_challenge"].clone(), email.to_string(), sub.to_string(), true));
        let redirect = format!("{}?code={code}&state={}", q["redirect_uri"], q["state"]);
        tokio::spawn(async move {
            let page = reqwest::get(&redirect).await.unwrap().text().await.unwrap();
            assert!(page.contains("signed in"), "{page}");
        });
        Ok(())
    }
}
