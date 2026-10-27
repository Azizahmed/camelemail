//! Outlook turning down a token the identity platform just issued, against
//! the mock identity platform and fake IMAP (its own binary: the mock's
//! address is process-wide).

mod common;

use std::sync::{Arc, Mutex};

use axum::routing::post;
use axum::Router;
use common::*;
use remail_core::secrets::MemoryStore;
use remail_core::store::Store;
use remail_core::*;

/// Outlook turning down a token the identity platform just issued is
/// usually passing: the mailbox retries ("offline") and only asks to sign
/// in again once the refusals keep up.
#[tokio::test(flavor = "multi_thread")]
async fn refused_fresh_token_retries_before_asking_to_sign_in() {
    let idp: Shared = Arc::new(Mutex::new(Idp::default()));
    let idp_port = serve(Router::new().route("/common/oauth2/v2.0/token", post(token)).with_state(idp.clone())).await;
    std::env::set_var("REMAIL_MS_AUTHORITY", format!("http://127.0.0.1:{idp_port}/common"));
    std::env::set_var("REMAIL_MS_GRAPH", format!("http://127.0.0.1:{idp_port}/v1.0"));
    let accepted = Arc::new(Mutex::new(vec!["at-1".to_string()]));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let imap = ServerConfig { host: "127.0.0.1".into(), port: fake_imap(accepted.clone(), seen.clone()).await, security: Security::None };
    let smtp = ServerConfig { host: "127.0.0.1".into(), port: fake_smtp(accepted.clone()).await, security: Security::None };
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::with_store(Store::open(&dir.path().join("remail.db")).unwrap(), Arc::new(MemoryStore::default()), Arc::new(Quiet)).unwrap();
    engine.set_org(microsoft_org());
    let req = OAuthAccountRequest { imap: Some(imap), smtp: Some(smtp), ..Default::default() };
    let id = engine.add_microsoft_account(browser(idp.clone(), "code-1"), req).await.unwrap().account.id;

    // The refresh works, but IMAP refuses every token from now on.
    idp.lock().unwrap().refresh.insert("rt-1".into(), ("at-2".into(), "rt-1".into()));
    accepted.lock().unwrap().clear();
    let status = || engine.accounts().unwrap().into_iter().find(|a| a.id == id).unwrap().status;
    let first = engine.sync_and_wait(&id).await.unwrap_err();
    assert!(!remail_core::imap::is_auth_error(&first), "{first:#}");
    assert_eq!(status(), AccountStatus::Offline);
    let mut tries = 1;
    while status() != AccountStatus::AuthError {
        let _ = engine.sync_and_wait(&id).await;
        tries += 1;
        assert!(tries <= 8, "never asked to sign in again");
    }
    assert!(tries >= 3, "asked to sign in again after {tries} refusals");
    assert!(seen.lock().unwrap().iter().any(|t| t == "at-2"), "each try used a fresh token");
}
