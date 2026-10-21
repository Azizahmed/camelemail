//! "Sign in with Google" end to end against a mock Google: PKCE sign-in
//! through the localhost redirect, Gmail over XOAUTH2 (fake IMAP/SMTP),
//! token refresh (Google refreshes name no scope), the calendar asked for
//! with the same token over CalDAV, and signing in again.

mod common;

use std::sync::{Arc, Mutex};

use axum::routing::{any, post};
use axum::Router;
use common::*;
use remail_core::secrets::{MemoryStore, SecretStore};
use remail_core::store::Store;
use remail_core::*;

#[tokio::test(flavor = "multi_thread")]
async fn google_sign_in() {
    let idp: Shared = Arc::new(Mutex::new(Idp::default()));
    // CalDAV requests and the Authorization header they carried.
    let dav_auth: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen_auth = dav_auth.clone();
    let caldav = any(move |headers: axum::http::HeaderMap| {
        let seen = seen_auth.clone();
        async move {
            seen.lock().unwrap().push(headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string());
            axum::http::StatusCode::NOT_FOUND
        }
    });
    let app = Router::new().route("/token", post(google_token)).with_state(idp.clone()).route("/caldav/{*rest}", caldav);
    let port = serve(app).await;
    std::env::set_var("REMAIL_GOOGLE_AUTHORITY", format!("http://127.0.0.1:{port}"));
    std::env::set_var("REMAIL_GOOGLE_CALDAV", format!("http://127.0.0.1:{port}/caldav"));

    let accepted = Arc::new(Mutex::new(vec!["gat-1".to_string()]));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let imap = ServerConfig { host: "127.0.0.1".into(), port: fake_imap(accepted.clone(), seen.clone()).await, security: Security::None };
    let smtp = ServerConfig { host: "127.0.0.1".into(), port: fake_smtp(accepted.clone()).await, security: Security::None };

    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemoryStore::default());
    let engine = Engine::with_store(Store::open(&dir.path().join("remail.db")).unwrap(), secrets.clone(), Arc::new(Quiet)).unwrap();
    let req = || OAuthAccountRequest { imap: Some(imap.clone()), smtp: Some(smtp.clone()), ..Default::default() };

    // No Google client in the organisation: it says what's missing.
    let none = engine.add_google_account(|_| Ok(()), req()).await.unwrap_err().to_string();
    assert!(none.contains("Google client ID"), "{none}");
    // The organisation doesn't allow Google.
    let mut providers = remail_core::proto::org::Providers::default();
    providers.google_app.client_id = "gclient".into();
    providers.google = false;
    engine.set_org(Some(engine::OrgAccess { providers: providers.clone(), google_server_exchange: false }));
    let off = engine::friendly_error(&engine.add_google_account(|_| Ok(()), req()).await.unwrap_err());
    assert!(off.starts_with("cloud:provider_not_allowed:"), "{off}");
    providers.google = true;
    engine.set_org(Some(engine::OrgAccess { providers, google_server_exchange: false }));

    let added = engine.add_google_account(google_browser(idp.clone(), "code-1", "Me@Gmail.com", "sub-me"), req()).await.unwrap();
    assert_eq!(added.account.email, "me@gmail.com");
    assert_eq!(added.account.auth, "google");
    assert!(added.account.is_gmail);
    assert!(added.warning.is_none(), "{:?}", added.warning);
    let grant: serde_json::Value = serde_json::from_str(&secrets.get(&added.account.id).unwrap().unwrap()).unwrap();
    assert_eq!((grant["provider"].as_str(), grant["clientId"].as_str(), grant["refreshToken"].as_str()), (Some("google"), Some("gclient"), Some("grt-1")));
    assert_eq!(grant["subject"], "sub-me");
    assert_eq!(seen.lock().unwrap().first().map(String::as_str), Some("gat-1"));
    // Its calendars are looked for with the same token (Bearer, not a password).
    for _ in 0..50 {
        if !dav_auth.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(dav_auth.lock().unwrap().first().map(String::as_str), Some("Bearer gat-1"));

    // A fresh start refreshes (without a scope: Google's tokens cover what was granted).
    let id = added.account.id.clone();
    drop(engine);
    idp.lock().unwrap().refresh.insert("grt-1".into(), ("gat-2".into(), "grt-1".into()));
    accepted.lock().unwrap().push("gat-2".into());
    let engine = Engine::with_store(Store::open(&dir.path().join("remail.db")).unwrap(), secrets.clone(), Arc::new(Quiet)).unwrap();
    let mut providers = remail_core::proto::org::Providers::default();
    providers.google_app.client_id = "gclient".into();
    engine.set_org(Some(engine::OrgAccess { providers, google_server_exchange: false }));
    let account = engine.accounts().unwrap().into_iter().find(|a| a.id == id).unwrap();
    let changed = NewAccount { email: account.email.clone(), display_name: "Me".into(), username: String::new(), password: String::new(), imap: imap.clone(), smtp: smtp.clone(), allow_invalid_certs: false };
    engine.update_account(&id, changed).await.unwrap();
    assert!(seen.lock().unwrap().contains(&"gat-2".to_string()));

    // Signing in again as another Google account is refused; as the same one it works.
    let other = engine.reauthorize_account(&id, google_browser(idp.clone(), "code-2", "me@gmail.com", "sub-other")).await.unwrap_err().to_string();
    assert!(other.contains("but this account is me@gmail.com"), "{other}");
    engine.reauthorize_account(&id, google_browser(idp.clone(), "code-3", "me@gmail.com", "sub-me")).await.unwrap();
    let reqs = idp.lock().unwrap().token_requests.clone();
    assert_eq!(reqs.iter().filter(|r| r.0 == "authorization_code").count(), 3, "{reqs:?}");
    assert!(reqs.iter().filter(|r| r.0 == "refresh_token").all(|r| r.1.is_empty()), "{reqs:?}");
}
