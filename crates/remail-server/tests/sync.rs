//! Sections over HTTP: round trip, 412 + merge + retry, size caps, names,
//! `cred:*` independence, two offline devices, notices.

mod common;

use std::time::Duration;

use common::*;
use futures_util::{SinkExt, StreamExt};
use remail_proto::api::Notice;
use remail_proto::sections::account;
use remail_proto::sync::{SectionDoc, Stamp, SINGLETON};
use remail_server::Mode;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

fn to_value(d: &SectionDoc) -> Value {
    serde_json::to_value(d).unwrap()
}

fn from_value(v: &Value) -> SectionDoc {
    serde_json::from_value(v.clone()).unwrap()
}

/// PUT with the client loop: on 412 merge the current doc and retry.
async fn put_merging(s: &Server, token: &str, id: &str, mut base_version: i64, mut doc: SectionDoc) -> (i64, u32) {
    for attempt in 0..5 {
        let r = s.put_section(token, id, base_version, &to_value(&doc)).await;
        match r.status.as_u16() {
            200 => return (r.body["version"].as_i64().unwrap(), attempt),
            412 => {
                assert_eq!(r.code(), "version_mismatch");
                let current = &r.body["current"];
                doc.merge(&from_value(&current["doc"]));
                base_version = current["version"].as_i64().unwrap();
            }
            _ => panic!("PUT {id}: {} {:?}", r.status, r.body),
        }
    }
    panic!("no convergence after 5 tries");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn round_trip_and_versions() {
    let Some(s) = start(Mode::Hosted).await else { return };
    let t = token(&s.new_user("a@example.com", PASSWORD).await);

    // Never written: version 0, empty doc.
    let r = s.get("/sections/prefs", Some(&t)).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body["version"], 0);
    assert_eq!(r.headers["etag"], "\"v0\"");
    assert_eq!(r.body["doc"], json!({"records": {}}));

    let mut doc = SectionDoc::default();
    doc.record_mut(SINGLETON).set("theme", json!("dark"), &Stamp::new(1000, "dev_a"));
    let r = s.put_section(&t, "prefs", 0, &to_value(&doc)).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    assert_eq!(r.body["version"], 1);
    assert_eq!(r.headers["etag"], "\"v1\"");

    let r = s.get("/sections/prefs", Some(&t)).await;
    assert_eq!(r.body["version"], 1);
    assert_eq!(from_value(&r.body["doc"]), doc);

    // Ciphertext only in the database, bound to its version.
    let ct: Vec<u8> = sqlx::query_scalar("select ciphertext from user_sections where section = 'prefs'").fetch_one(&s.db).await.unwrap();
    assert!(!String::from_utf8_lossy(&ct).contains("dark"));

    // Index.
    let idx = s.get("/sections", Some(&t)).await;
    assert_eq!(idx.body["sections"][0]["id"], "prefs");
    assert_eq!(idx.body["sections"][0]["version"], 1);
    assert!(idx.body["serverTime"].as_i64().unwrap() > 0);

    // Missing If-Match, stale If-Match.
    let r = s.send(s.http.put(s.api("/sections/prefs")).bearer_auth(&t).json(&json!({"doc": to_value(&doc)}))).await;
    assert_eq!(r.code(), "bad_request");
    let r = s.put_section(&t, "prefs", 0, &to_value(&doc)).await;
    assert_eq!(r.status, 412);
    assert_eq!(r.body["current"]["version"], 1);
    assert_eq!(from_value(&r.body["current"]["doc"]), doc);

    // Bad names.
    for bad in ["nope", "cred:usr_1", "cred:acc_x%2F..", "org"] {
        let r = s.get(&format!("/sections/{bad}"), Some(&t)).await;
        assert_eq!(r.status, 404, "{bad}");
        let r = s.put_section(&t, bad, 0, &json!({"records": {}})).await;
        assert_eq!(r.status, 404, "{bad}");
    }

    // Size cap.
    let mut big = SectionDoc::default();
    big.record_mut(SINGLETON).set("blob", json!("x".repeat(300 * 1024)), &Stamp::new(2000, "dev_a"));
    let r = s.put_section(&t, "templates", 0, &to_value(&big)).await;
    assert_eq!(r.status, 413);
    assert_eq!(r.code(), "too_large");

    // Per-user total: 9 sections of ~250 KiB go over 2 MiB.
    let mut chunk = SectionDoc::default();
    chunk.record_mut(SINGLETON).set("blob", json!("y".repeat(250 * 1024)), &Stamp::new(2000, "dev_a"));
    let mut over = false;
    for i in 0..10 {
        let r = s.put_section(&t, &format!("cred:acc_{i}"), 0, &to_value(&chunk)).await;
        if r.status == 413 {
            over = true;
            break;
        }
        assert_eq!(r.status, 200);
    }
    assert!(over, "2 MiB per-user cap");

    // Not signed in.
    assert_eq!(s.get("/sections/prefs", None).await.status, 401);

    // Decrypts were audited, aggregated per device.
    let rows: Vec<Value> = sqlx::query_scalar("select meta from audit_log where action = 'decrypt.sync'").fetch_all(&s.db).await.unwrap();
    assert_eq!(rows.len(), 1, "one aggregated row: {rows:?}");
    assert!(rows[0]["count"].as_i64().unwrap() >= 3);
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row_swap_fails_to_decrypt() {
    let Some(s) = start(Mode::Hosted).await else { return };
    let t = token(&s.new_user("swap@example.com", PASSWORD).await);
    let mut a = SectionDoc::default();
    a.record_mut(SINGLETON).set("k", json!("prefs"), &Stamp::new(1, "d"));
    s.put_section(&t, "prefs", 0, &to_value(&a)).await;
    s.put_section(&t, "shortcuts", 0, &to_value(&a)).await;
    // Swap the two ciphertexts in SQL: AAD binds them to their section.
    sqlx::query(
        "update user_sections u set ciphertext = o.ciphertext from user_sections o
         where u.user_id = o.user_id and u.section = 'prefs' and o.section = 'shortcuts'",
    )
    .execute(&s.db)
    .await
    .unwrap();
    let r = s.get("/sections/prefs", Some(&t)).await;
    assert_eq!(r.status, 500);
    assert_eq!(r.code(), "internal");
    assert_eq!(s.get("/sections/shortcuts", Some(&t)).await.status, 200);
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_devices_offline_conflict() {
    let Some(s) = start(Mode::Hosted).await else { return };
    let email = "two@example.com";
    let a = token(&s.new_user(email, PASSWORD).await);
    let b = token(&s.device(email, PASSWORD, "Desktop B").await);

    // Stamps are recent (old tombstones are collected on write).
    let t0 = remail_proto::now_ms() - 60_000;
    // Shared starting point: two accounts.
    let mut base = SectionDoc::default();
    for id in ["acc_1", "acc_2"] {
        let r = base.record_mut(id);
        let at = Stamp::new(t0 + 1_000, "dev_a");
        r.set(account::PROVIDER, json!("imap"), &at);
        r.set(account::IMAP, json!({"host": "mail.example.com", "port": 993, "security": "tls"}), &at);
        r.set(account::DISPLAY_NAME, json!(id), &at);
        r.set(account::COLOR, json!("blue"), &at);
    }
    let (v1, _) = put_merging(&s, &a, "accounts", 0, base.clone()).await;
    assert_eq!(v1, 1);

    // Both go offline from v1 and edit.
    let mut doc_a = base.clone();
    doc_a.record_mut("acc_1").set(account::COLOR, json!("red"), &Stamp::new(t0 + 2_000, "dev_a"));
    doc_a.record_mut("acc_1").set(account::DISPLAY_NAME, json!("Work (A)"), &Stamp::new(t0 + 2_000, "dev_a"));
    doc_a.delete("acc_2", &Stamp::new(t0 + 2_500, "dev_a"));
    let mut doc_b = base.clone();
    doc_b.record_mut("acc_1").set(account::SORT_ORDER, json!(5), &Stamp::new(t0 + 1_500, "dev_b"));
    doc_b.record_mut("acc_1").set(account::DISPLAY_NAME, json!("Work (B)"), &Stamp::new(t0 + 3_000, "dev_b"));
    // B renames acc_2 *before* A deleted it: the delete wins.
    doc_b.record_mut("acc_2").set(account::DISPLAY_NAME, json!("Old"), &Stamp::new(t0 + 2_200, "dev_b"));

    let (va, tries_a) = put_merging(&s, &a, "accounts", v1, doc_a).await;
    assert_eq!((va, tries_a), (2, 0));
    let (vb, tries_b) = put_merging(&s, &b, "accounts", v1, doc_b).await;
    assert_eq!((vb, tries_b), (3, 1), "B got a 412, merged and retried once");

    // Both devices read the same converged document.
    let ra = s.get("/sections/accounts", Some(&a)).await;
    let rb = s.get("/sections/accounts", Some(&b)).await;
    assert_eq!(ra.body, rb.body);
    let doc = from_value(&ra.body["doc"]);
    let acc1 = doc.record("acc_1").unwrap();
    assert_eq!(acc1.get(account::COLOR), Some(&json!("red")), "different fields merge");
    assert_eq!(acc1.get(account::SORT_ORDER), Some(&json!(5)));
    assert_eq!(acc1.get(account::DISPLAY_NAME), Some(&json!("Work (B)")), "same field: later stamp wins");
    assert!(doc.record("acc_2").is_none(), "tombstone beats the earlier offline edit");
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cred_sections_never_conflict_with_accounts() {
    let Some(s) = start(Mode::Hosted).await else { return };
    let email = "cred@example.com";
    let a = token(&s.new_user(email, PASSWORD).await);
    let b = token(&s.device(email, PASSWORD, "B").await);
    let mut acc = SectionDoc::default();
    acc.record_mut("acc_1").set(account::PROVIDER, json!("microsoft"), &Stamp::new(1, "dev_a"));
    put_merging(&s, &a, "accounts", 0, acc.clone()).await;
    let mut cred = SectionDoc::default();
    cred.record_mut(SINGLETON).set("grant", json!({"refreshToken": "r1"}), &Stamp::new(1, "dev_a"));
    put_merging(&s, &a, "cred:acc_1", 0, cred.clone()).await;

    // Interleaved: B refreshes the token while A edits the account, many times.
    let mut acc_v = 1;
    let mut cred_v = 1;
    for i in 0..10 {
        acc.record_mut("acc_1").set(account::COLOR, json!(format!("c{i}")), &Stamp::new(10 + i, "dev_a"));
        let r = s.put_section(&a, "accounts", acc_v, &to_value(&acc)).await;
        assert_eq!(r.status, 200, "accounts never 412s on a token refresh");
        acc_v = r.body["version"].as_i64().unwrap();
        cred.record_mut(SINGLETON).set("grant", json!({"refreshToken": format!("r{i}")}), &Stamp::new(10 + i, "dev_b"));
        let r = s.put_section(&b, "cred:acc_1", cred_v, &to_value(&cred)).await;
        assert_eq!(r.status, 200, "cred never 412s on an account edit");
        cred_v = r.body["version"].as_i64().unwrap();
    }
    assert_eq!((acc_v, cred_v), (11, 11));

    // Concurrent writers to the same section serialise: every PUT from v
    // either wins (one) or gets 412, never a lost update.
    let mut handles = Vec::new();
    for i in 0..8 {
        let http = s.http.clone();
        let url = s.api("/sections/signatures");
        let t = a.clone();
        handles.push(tokio::spawn(async move {
            let mut d = SectionDoc::default();
            d.record_mut(&format!("sig_{i}")).set("name", json!(i), &Stamp::new(1, "dev_a"));
            http.put(url).bearer_auth(t).header("If-Match", "\"v0\"").json(&json!({"doc": d})).send().await.unwrap().status().as_u16()
        }));
    }
    let mut codes = Vec::new();
    for h in handles {
        codes.push(h.await.unwrap());
    }
    assert_eq!(codes.iter().filter(|c| **c == 200).count(), 1, "{codes:?}");
    assert_eq!(codes.iter().filter(|c| **c == 412).count(), 7, "{codes:?}");
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tombstones_are_collected_on_write() {
    let Some(s) = start(Mode::Hosted).await else { return };
    let t = token(&s.new_user("gc@example.com", PASSWORD).await);
    let now = remail_proto::now_ms();
    let mut d = SectionDoc::default();
    d.record_mut("tmpl_old").set("name", json!("x"), &Stamp::new(now - 40 * 86_400_000, "d"));
    d.delete("tmpl_old", &Stamp::new(now - 35 * 86_400_000, "d"));
    d.record_mut("tmpl_new").set("name", json!("y"), &Stamp::new(now, "d"));
    d.delete("tmpl_recent", &Stamp::new(now - 1000, "d"));
    let r = s.put_section(&t, "templates", 0, &to_value(&d)).await;
    assert_eq!(r.status, 200);
    let got = from_value(&s.get("/sections/templates", Some(&t)).await.body["doc"]);
    assert!(!got.records.contains_key("tmpl_old"), "30-day-old tombstone dropped");
    assert!(got.records.contains_key("tmpl_recent"), "recent tombstone kept");
    assert!(got.record("tmpl_new").is_some());
    s.finish().await;
}

async fn ws_connect(s: &Server, access: &str) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let url = s.api("/notices").replace("http://", "ws://");
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.expect("ws connect");
    ws.send(Message::Text(json!({"accessToken": access}).to_string().into())).await.unwrap();
    ws
}

async fn next_notice(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
) -> Option<Notice> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.ok()??.ok()?;
        match msg {
            Message::Text(t) => return serde_json::from_str(&t).ok(),
            Message::Close(_) => return None,
            _ => continue,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notices_on_change() {
    let Some(s) = start(Mode::Hosted).await else { return };
    let email = "ws@example.com";
    let a = s.new_user(email, PASSWORD).await;
    let b = s.device(email, PASSWORD, "B").await;
    let other = token(&s.new_user("other@example.com", PASSWORD).await);

    // Bad auth: closed.
    let url = s.api("/notices").replace("http://", "ws://");
    let (mut bad, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    bad.send(Message::Text(json!({"accessToken": "rma_nope"}).to_string().into())).await.unwrap();
    assert!(next_notice(&mut bad).await.is_none());

    let mut ws_b = ws_connect(&s, &token(&b)).await;
    assert!(matches!(next_notice(&mut ws_b).await, Some(Notice::Hello { .. })));
    let mut ws_other = ws_connect(&s, &other).await;
    assert!(matches!(next_notice(&mut ws_other).await, Some(Notice::Hello { .. })));

    let mut d = SectionDoc::default();
    d.record_mut(SINGLETON).set("theme", json!("dark"), &Stamp::new(1, "dev_a"));
    s.put_section(&token(&a), "prefs", 0, &to_value(&d)).await;
    assert_eq!(next_notice(&mut ws_b).await, Some(Notice::Section { section: "prefs".into(), version: 1 }));

    // Org config change reaches the org's devices (through NOTIFY from "another process").
    let org = remail_server::orgs::by_slug(&s.db, "public").await.unwrap().unwrap();
    let cfg = remail_server::orgs::config(&s.db, &s.state.env, &org).await.unwrap().1;
    remail_server::orgs::set_config(&s.db, &s.state.env, &org.id, &cfg, "cli").await.unwrap();
    assert_eq!(next_notice(&mut ws_b).await, Some(Notice::Org { version: 1 }));
    // The other user is in the same (public) org: org notice, but never A's section notice.
    assert_eq!(next_notice(&mut ws_other).await, Some(Notice::Org { version: 1 }));

    // Revoking B signs its socket out.
    let id = b["deviceId"].as_str().unwrap();
    let r = s.send(s.http.delete(s.api(&format!("/me/devices/{id}"))).bearer_auth(token(&a))).await;
    assert_eq!(r.status, 204);
    assert_eq!(next_notice(&mut ws_b).await, Some(Notice::SignedOut));
    s.finish().await;
}
