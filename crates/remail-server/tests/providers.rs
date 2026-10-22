//! Org config, provider list, the server-side allow-list on `accounts` and
//! the Google code exchange.

mod common;

use axum::routing::post;
use axum::{Form, Json, Router};
use common::*;
use remail_proto::org::{OrgSecrets, ServerPreset};
use remail_proto::sections::account;
use remail_proto::sync::{SectionDoc, Stamp};
use remail_server::Mode;
use serde_json::{json, Value};

fn account_doc(id: &str, provider: &str, host: &str, ms: i64) -> SectionDoc {
    let mut d = SectionDoc::default();
    let at = Stamp::new(ms, "dev_a");
    let r = d.record_mut(id);
    r.set(account::PROVIDER, json!(provider), &at);
    r.set(account::IMAP, json!({"host": host, "port": 993, "security": "tls"}), &at);
    d
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allow_list_is_enforced_server_side() {
    let Some(s) = start(Mode::Hosted).await else { return };
    let t = token(&s.new_user("p@example.com", PASSWORD).await);

    // Defaults: everything on, Google/Microsoft need a client id to show.
    let cfg = s.get("/org/config", Some(&t)).await;
    assert_eq!(cfg.status, 200);
    assert_eq!(cfg.body["version"], 0);
    assert_eq!(cfg.body["config"]["name"], "Camel Mail");
    assert_eq!(cfg.body["config"]["providers"]["imap"], true);
    let p = s.get("/providers", Some(&t)).await;
    let list = p.body.as_array().unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0]["kind"], "google");
    assert_eq!(list[0]["enabled"], false, "no Google client id yet");
    assert_eq!(list[2]["kind"], "imap");
    assert_eq!(list[2]["enabled"], true);

    // A Google account goes in while Google is allowed.
    let doc = account_doc("acc_g", "google", "", 1);
    let r = s.put_section(&t, "accounts", 0, &serde_json::to_value(&doc).unwrap()).await;
    assert_eq!(r.status, 200);

    // The admin switches Google off and limits IMAP to a preset.
    let org = remail_server::orgs::by_slug(&s.db, "public").await.unwrap().unwrap();
    let mut c = remail_server::orgs::config(&s.db, &s.state.env, &org).await.unwrap().1;
    c.providers.google = false;
    c.providers.manual_servers = false;
    c.providers.presets = vec![ServerPreset { name: "Acme".into(), imap_host: "mail.acme.com".into(), ..Default::default() }];
    remail_server::orgs::set_config(&s.db, &s.state.env, &org.id, &c, "test").await.unwrap();

    // A new Google account: 422 (a direct API call, no UI involved).
    let mut doc2 = doc.clone();
    doc2.merge(&account_doc("acc_g2", "google", "", 2));
    let r = s.put_section(&t, "accounts", 1, &serde_json::to_value(&doc2).unwrap()).await;
    assert_eq!(r.status, 422);
    assert_eq!(r.code(), "provider_not_allowed");
    // The existing Google account can still be edited (grandfathered)…
    let mut doc3 = doc.clone();
    doc3.record_mut("acc_g").set(account::COLOR, json!("red"), &Stamp::new(3, "dev_a"));
    let r = s.put_section(&t, "accounts", 1, &serde_json::to_value(&doc3).unwrap()).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    // …and removed.
    doc3.delete("acc_g", &Stamp::new(4, "dev_a"));
    let r = s.put_section(&t, "accounts", 2, &serde_json::to_value(&doc3).unwrap()).await;
    assert_eq!(r.status, 200);

    // IMAP: only the preset host.
    doc3.merge(&account_doc("acc_i", "imap", "imap.evil.com", 5));
    let r = s.put_section(&t, "accounts", 3, &serde_json::to_value(&doc3).unwrap()).await;
    assert_eq!(r.code(), "provider_not_allowed");
    let mut ok = doc3.clone();
    ok.record_mut("acc_i").set(account::IMAP, json!({"host": "MAIL.acme.com", "port": 993, "security": "tls"}), &Stamp::new(6, "dev_a"));
    let r = s.put_section(&t, "accounts", 3, &serde_json::to_value(&ok).unwrap()).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    // A record without a provider is malformed.
    let mut bad = ok.clone();
    bad.record_mut("acc_x").set(account::EMAIL, json!("x@y.z"), &Stamp::new(7, "dev_a"));
    let r = s.put_section(&t, "accounts", 4, &serde_json::to_value(&bad).unwrap()).await;
    assert_eq!(r.code(), "bad_request");

    // Providers reflect the change.
    let p = s.get("/providers", Some(&t)).await;
    assert_eq!(p.body[0]["enabled"], false);
    assert_eq!(p.body[2]["manualServers"], false);
    assert_eq!(p.body[2]["presets"][0]["imapHost"], "mail.acme.com");
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_code_exchange() {
    // Mock Google token endpoint.
    let mock = Router::new().route(
        "/token",
        post(|Form(f): Form<std::collections::HashMap<String, String>>| async move {
            if f.get("client_secret").map(String::as_str) == Some("s3cret") && f.get("code").map(String::as_str) == Some("good") {
                Json(json!({"access_token": "ya29.x", "expires_in": 3599, "refresh_token": "1//r", "token_type": "Bearer"})).into_response_ok()
            } else {
                (axum::http::StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant"}))).into_response_ok()
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_url = format!("http://{}/token", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, mock).await.unwrap() });

    let Some(s) = start_with(Mode::Hosted, |c| c.google_token_url = mock_url).await else { return };
    let t = token(&s.new_user("g@example.com", PASSWORD).await);
    let body = json!({"code": "good", "codeVerifier": "v", "redirectUri": "http://127.0.0.1:1/cb"});

    // Google has no client id: refused.
    let r = s.post("/providers/google/token", body.clone(), Some(&t)).await;
    assert_eq!(r.code(), "provider_not_allowed");

    let org = remail_server::orgs::by_slug(&s.db, "public").await.unwrap().unwrap();
    let mut c = remail_server::orgs::config(&s.db, &s.state.env, &org).await.unwrap().1;
    c.providers.google_app.client_id = "cid.apps.googleusercontent.com".into();
    remail_server::orgs::set_config(&s.db, &s.state.env, &org.id, &c, "test").await.unwrap();
    // No secret: the client does PKCE itself.
    let r = s.post("/providers/google/token", body.clone(), Some(&t)).await;
    assert_eq!(r.code(), "not_found");
    let p = s.get("/providers", Some(&t)).await;
    assert_eq!(p.body[0]["enabled"], true);
    assert_eq!(p.body[0]["serverExchange"], false);

    let secrets = OrgSecrets { google_client_secret: "s3cret".into(), ..Default::default() };
    remail_server::orgs::set_secrets(&s.db, &s.state.env, &org.id, &secrets).await.unwrap();
    let p = s.get("/providers", Some(&t)).await;
    assert_eq!(p.body[0]["serverExchange"], true);
    let all = serde_json::to_string(&p.body).unwrap() + &serde_json::to_string(&s.get("/org/config", Some(&t)).await.body).unwrap();
    assert!(!all.contains("s3cret"), "the secret never reaches clients");

    let r = s.post("/providers/google/token", body, Some(&t)).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    assert_eq!(r.body["accessToken"], "ya29.x");
    assert_eq!(r.body["refreshToken"], "1//r");
    let r = s.post("/providers/google/token", json!({"code": "bad", "codeVerifier": "v", "redirectUri": "x"}), Some(&t)).await;
    assert_eq!(r.code(), "invalid_code");
    // Secrets are ciphertext at rest.
    let ct: Vec<u8> = sqlx::query_scalar("select ciphertext from org_secrets").fetch_one(&s.db).await.unwrap();
    assert!(!String::from_utf8_lossy(&ct).contains("s3cret"));
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_refresh_with_the_org_secret() {
    // Mock Google token endpoint: refreshes need the client secret.
    let mock = Router::new().route(
        "/token",
        post(|Form(f): Form<std::collections::HashMap<String, String>>| async move {
            let get = |k: &str| f.get(k).map(String::as_str);
            let ok = get("grant_type") == Some("refresh_token")
                && get("client_id") == Some("cid.apps.googleusercontent.com")
                && get("client_secret") == Some("s3cret");
            match get("refresh_token") {
                Some("1//live") if ok => Json(json!({"access_token": "ya29.new", "expires_in": 3599, "token_type": "Bearer"})).into_response_ok(),
                Some("1//boom") => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "oops").into_response_ok(),
                _ => (axum::http::StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant", "error_description": "Token has been expired or revoked."})))
                    .into_response_ok(),
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_url = format!("http://{}/token", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, mock).await.unwrap() });

    let Some(s) = start_with(Mode::Hosted, |c| c.google_token_url = mock_url).await else { return };
    let t = token(&s.new_user("gr@example.com", PASSWORD).await);
    let body = json!({"refreshToken": "1//live"});

    // Not signed in: refused.
    assert_eq!(s.post("/providers/google/refresh", body.clone(), None).await.status, 401);
    // No Google client id yet.
    assert_eq!(s.post("/providers/google/refresh", body.clone(), Some(&t)).await.code(), "provider_not_allowed");
    let org = remail_server::orgs::by_slug(&s.db, "public").await.unwrap().unwrap();
    let mut c = remail_server::orgs::config(&s.db, &s.state.env, &org).await.unwrap().1;
    c.providers.google_app.client_id = "cid.apps.googleusercontent.com".into();
    remail_server::orgs::set_config(&s.db, &s.state.env, &org.id, &c, "test").await.unwrap();
    // No secret: the client refreshes with Google itself.
    assert_eq!(s.post("/providers/google/refresh", body.clone(), Some(&t)).await.code(), "not_found");

    let secrets = OrgSecrets { google_client_secret: "s3cret".into(), ..Default::default() };
    remail_server::orgs::set_secrets(&s.db, &s.state.env, &org.id, &secrets).await.unwrap();
    let r = s.post("/providers/google/refresh", body, Some(&t)).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    assert_eq!(r.body["accessToken"], "ya29.new");
    assert_eq!(r.body["expiresIn"], 3599);
    assert!(r.body.get("refreshToken").is_none_or(Value::is_null), "Google didn't rotate it");
    // Revoked at Google: the user signs in again (`invalid_token`).
    let r = s.post("/providers/google/refresh", json!({"refreshToken": "1//dead"}), Some(&t)).await;
    assert_eq!((r.status.as_u16(), r.code()), (400, "invalid_token"));
    assert!(r.body["error"]["message"].as_str().unwrap().contains("invalid_grant"));
    // Google failing is a passing problem, not a sign-out.
    let r = s.post("/providers/google/refresh", json!({"refreshToken": "1//boom"}), Some(&t)).await;
    assert_eq!(r.code(), "unavailable");
    assert_eq!(s.post("/providers/google/refresh", json!({"refreshToken": ""}), Some(&t)).await.code(), "bad_request");

    // Audited, without the tokens.
    let rows: Vec<(String, Value)> = sqlx::query_as("select action, meta from audit_log where action = 'oauth.google.refresh'")
        .fetch_all(&s.db)
        .await
        .unwrap();
    assert_eq!(rows.len(), 3, "{rows:?}");
    let all = format!("{rows:?}");
    assert!(!all.contains("1//") && !all.contains("ya29"), "{all}");
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn org_codes() {
    let Some(s) = start(Mode::Hosted).await else { return };
    let r = s.get("/orgcode/public", None).await;
    assert_eq!(r.body["server"], Value::String(s.url.clone()));
    remail_server::orgs::create(&s.db, &s.state.env, "Acme", "acme", "invite").await.unwrap();
    sqlx::query("update orgs set server_url = 'https://mail.acme.com' where slug = 'acme'").execute(&s.db).await.unwrap();
    assert_eq!(s.get("/orgcode/ACME", None).await.body["server"], "https://mail.acme.com");
    assert_eq!(s.get("/orgcode/nope", None).await.status, 404);
    assert_eq!(s.get("/orgcode/..", None).await.status, 404);
    s.finish().await;
}

trait OkResponse {
    fn into_response_ok(self) -> axum::response::Response;
}

impl<T: axum::response::IntoResponse> OkResponse for T {
    fn into_response_ok(self) -> axum::response::Response {
        self.into_response()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_app_from_the_environment() {
    let Some(s) = start_with(Mode::Hosted, |c| {
        c.google_client_id = Some("env.apps.googleusercontent.com".into());
        c.google_client_secret = Some("env-secret".into());
    })
    .await
    else {
        return;
    };
    // Devices get the installed-app secret and talk to Google themselves:
    // the server never exchanges or refreshes Google tokens.
    let t = token(&s.new_user("ge@example.com", PASSWORD).await);
    let p = s.get("/providers", Some(&t)).await;
    assert_eq!(p.body[0]["kind"], "google");
    assert_eq!(p.body[0]["enabled"], true);
    assert_eq!(p.body[0]["serverExchange"], false);
    assert_eq!(p.body[0]["oauth"]["clientId"], "env.apps.googleusercontent.com");
    assert_eq!(p.body[0]["oauth"]["clientSecret"], "env-secret");
    let body = json!({"code": "c", "codeVerifier": "v", "redirectUri": "http://localhost:1"});
    assert_eq!(s.post("/providers/google/token", body, Some(&t)).await.code(), "not_found");
    assert_eq!(s.post("/providers/google/refresh", json!({"refreshToken": "1//r"}), Some(&t)).await.code(), "not_found");

    // A secret the server held before (server-side exchange) is dropped on start.
    let org = remail_server::orgs::by_slug(&s.db, "public").await.unwrap().unwrap();
    let held = OrgSecrets { google_client_secret: "env-secret".into(), ..Default::default() };
    remail_server::orgs::set_secrets(&s.db, &s.state.env, &org.id, &held).await.unwrap();
    let v = remail_server::orgs::config(&s.db, &s.state.env, &org).await.unwrap().0;
    remail_server::orgs::startup(&s.state.cfg, &s.db, &s.state.env, false).await.unwrap();
    assert_eq!(remail_server::orgs::secrets(&s.db, &s.state.env, &org.id).await.unwrap().google_client_secret, "");
    // The same values again: the config isn't saved anew.
    assert_eq!(remail_server::orgs::config(&s.db, &s.state.env, &org).await.unwrap().0, v);
    s.finish().await;
}
