//! Server e-mail through SNDR's HTTPS API (CranL's e-mail service).

mod common;

use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use common::*;
use remail_server::Mode;
use serde_json::{json, Value};

#[tokio::test]
async fn verification_mail_goes_through_sndr() {
    let seen: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let rec = seen.clone();
    let app = Router::new().route(
        "/v1/send",
        post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let rec = rec.clone();
            async move {
                let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                rec.lock().unwrap().push((auth, body));
                Json(json!({"id": "em_1"}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let Some(s) = start_with(Mode::Hosted, |c| {
        c.dev_mail = false;
        c.sndr_api_key = Some("sndr_test_key".into());
        c.sndr_url = format!("http://{addr}/v1/send");
        c.mail_from = "Remail <no-reply@mail.example.com>".into();
    })
    .await
    else {
        return;
    };
    // Mail can be delivered, so sign-up is open again.
    assert_eq!(s.get("/meta", None).await.body["signup"], "open");
    let r = s.post("/auth/signup", json!({"name": "M", "email": "m@example.com", "password": PASSWORD}), None).await;
    assert_eq!(r.status, 202, "{:?}", r.body);

    let mut sent = None;
    for _ in 0..50 {
        if let Some(m) = seen.lock().unwrap().first().cloned() {
            sent = Some(m);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let (auth, body) = sent.expect("SNDR got the verification mail");
    assert_eq!(auth, "Bearer sndr_test_key");
    assert_eq!(body["from"], "Remail <no-reply@mail.example.com>");
    assert_eq!(body["to"], json!(["m@example.com"]));
    assert!(body["text"].as_str().unwrap().contains("/verify?t="), "{body}");
    s.finish().await;
}
