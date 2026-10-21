//! "Sign in with Microsoft" end to end, against a mock Microsoft identity
//! platform and fake IMAP/SMTP servers that only take the right XOAUTH2
//! token: PKCE sign-in through the localhost redirect, adding the account,
//! token refresh with rotation, an expired sign-in, signing in again.

mod common;

use std::sync::{Arc, Mutex};

use axum::routing::post;
use axum::Router;
use common::*;
use remail_core::secrets::{MemoryStore, SecretStore};
use remail_core::store::Store;
use remail_core::*;

#[tokio::test(flavor = "multi_thread")]
async fn microsoft_sign_in() {
    let idp: Shared = Arc::new(Mutex::new(Idp::default()));
    let idp_port = serve(Router::new().route("/common/oauth2/v2.0/token", post(token)).with_state(idp.clone())).await;
    std::env::set_var("REMAIL_MS_AUTHORITY", format!("http://127.0.0.1:{idp_port}/common"));
    // These accounts never allowed the calendar; keep any calendar request local all the same.
    std::env::set_var("REMAIL_MS_GRAPH", format!("http://127.0.0.1:{idp_port}/v1.0"));

    let accepted = Arc::new(Mutex::new(vec!["at-1".to_string()]));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let imap = ServerConfig { host: "127.0.0.1".into(), port: fake_imap(accepted.clone(), seen.clone()).await, security: Security::None };
    let smtp = ServerConfig { host: "127.0.0.1".into(), port: fake_smtp(accepted.clone()).await, security: Security::None };

    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemoryStore::default());
    let engine = Engine::with_store(Store::open(&dir.path().join("remail.db")).unwrap(), secrets.clone(), Arc::new(Quiet)).unwrap();
    let req = || OAuthAccountRequest { imap: Some(imap.clone()), smtp: Some(smtp.clone()), ..Default::default() };

    // Without an app (client) ID, it says what's missing.
    let none = engine.add_microsoft_account(|_| Ok(()), req()).await.unwrap_err().to_string();
    assert!(none.contains("client) ID"), "{none}");
    engine.set_org(microsoft_org());

    // Sign in: PKCE checked by the mock, XOAUTH2 by the fake servers.
    let added = engine.add_microsoft_account(browser(idp.clone(), "code-1"), req()).await.unwrap();
    assert_eq!(added.account.email, "aziz@outlook.com");
    assert_eq!(added.account.display_name, "Aziz");
    assert_eq!(added.account.auth, "microsoft");
    assert!(added.warning.is_none(), "{:?}", added.warning);
    let grant: serde_json::Value = serde_json::from_str(&secrets.get(&added.account.id).unwrap().unwrap()).unwrap();
    assert_eq!((grant["clientId"].as_str(), grant["refreshToken"].as_str(), grant["provider"].as_str()), (Some("client-123"), Some("rt-1"), Some("microsoft")));
    assert_eq!(grant["subject"], "9188040d-6c67-4c5b-b112-36a304b66dad/oid-aziz");
    assert_eq!(seen.lock().unwrap().first().map(String::as_str), Some("at-1"));

    // Same account again is refused.
    let dup = engine.add_microsoft_account(browser(idp.clone(), "code-2"), req()).await.unwrap_err().to_string();
    assert!(dup.contains("already added"), "{dup}");

    // A fresh start has no cached token: it refreshes, and keeps the rotated refresh token.
    let id = added.account.id.clone();
    drop(engine);
    idp.lock().unwrap().refresh.insert("rt-1".into(), ("at-2".into(), "rt-2".into()));
    accepted.lock().unwrap().push("at-2".into());
    let engine = Engine::with_store(Store::open(&dir.path().join("remail.db")).unwrap(), secrets.clone(), Arc::new(Quiet)).unwrap();
    engine.set_org(microsoft_org());
    let account = engine.accounts().unwrap().into_iter().find(|a| a.id == id).unwrap();
    assert_eq!(account.auth, "microsoft", "survives a restart");
    let changed = NewAccount { email: account.email.clone(), display_name: "Aziz A".into(), username: String::new(), password: String::new(), imap: imap.clone(), smtp: smtp.clone(), allow_invalid_certs: false };
    engine.update_account(&id, changed).await.unwrap();
    assert!(seen.lock().unwrap().contains(&"at-2".to_string()));
    let grant: serde_json::Value = serde_json::from_str(&secrets.get(&id).unwrap().unwrap()).unwrap();
    assert_eq!(grant["refreshToken"], "rt-2");

    // The sign-in is revoked: the next refresh says to sign in again.
    drop(engine);
    let engine = Engine::with_store(Store::open(&dir.path().join("remail.db")).unwrap(), secrets.clone(), Arc::new(Quiet)).unwrap();
    engine.set_org(microsoft_org());
    let account = engine.accounts().unwrap().into_iter().find(|a| a.id == id).unwrap();
    let again = NewAccount { email: account.email.clone(), display_name: "Aziz".into(), username: String::new(), password: String::new(), imap: imap.clone(), smtp: smtp.clone(), allow_invalid_certs: false };
    let expired = engine.update_account(&id, again).await.unwrap_err();
    assert!(remail_core::imap::is_auth_error(&expired), "{expired:#}");
    assert!(expired.to_string().contains("expired"), "{expired:#}");

    // Signing in again as someone else (another Microsoft account) is refused, even with the same address.
    let other = engine.reauthorize_account(&id, browser_as(idp.clone(), "code-x", "aziz@outlook.com", "oid-someone-else")).await.unwrap_err().to_string();
    assert!(other.contains("but this account is aziz@outlook.com"), "{other}");

    // Signing in again stores the new grant and clears the error.
    engine.reauthorize_account(&id, browser(idp.clone(), "code-3")).await.unwrap();
    let grant: serde_json::Value = serde_json::from_str(&secrets.get(&id).unwrap().unwrap()).unwrap();
    assert_eq!(grant["refreshToken"], "rt-1");
    assert_eq!(engine.accounts().unwrap()[0].status, AccountStatus::Active);
    // Three browser sign-ins; refreshes come from update_account and the background workers.
    let reqs = idp.lock().unwrap().token_requests.clone();
    assert_eq!(reqs.iter().filter(|r| r.0 == "authorization_code").count(), 4, "{reqs:?}");
    assert!(reqs.iter().filter(|r| r.0 == "refresh_token" && r.1.contains("IMAP")).count() >= 2, "{reqs:?}");
    // Codes are redeemed for mail only; the calendar's token is a separate request.
    assert!(reqs.iter().filter(|r| r.0 == "authorization_code").all(|r| !r.1.contains("graph.microsoft.com")), "{reqs:?}");

    // A Hotmail account added with a password (before Microsoft sign-in existed) switches over.
    let legacy = NewAccount {
        email: "old@hotmail.com".into(),
        display_name: "Old".into(),
        username: "old@hotmail.com".into(),
        password: String::new(),
        imap: imap.clone(),
        smtp: smtp.clone(),
        allow_invalid_certs: false,
    };
    let legacy = engine.store().insert_account(&legacy, false, "password").unwrap();
    secrets.set(&legacy.id, "old-password").unwrap();
    engine.reauthorize_account(&legacy.id, browser_as(idp.clone(), "code-4", "old@hotmail.com", "oid-old")).await.unwrap();
    let converted = engine.accounts().unwrap().into_iter().find(|a| a.id == legacy.id).unwrap();
    assert_eq!(converted.auth, "microsoft");
    let grant: serde_json::Value = serde_json::from_str(&secrets.get(&legacy.id).unwrap().unwrap()).unwrap();
    assert_eq!(grant["subject"], "9188040d-6c67-4c5b-b112-36a304b66dad/oid-old");

    // Waiting for the browser can be cancelled.
    let e2 = Arc::new(engine);
    let waiting = {
        let e = e2.clone();
        let id = id.clone();
        tokio::spawn(async move { e.reauthorize_account(&id, |_| Ok(())).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    e2.cancel_sign_in();
    let cancelled = waiting.await.unwrap().unwrap_err().to_string();
    assert!(cancelled.contains("cancelled"), "{cancelled}");
}
