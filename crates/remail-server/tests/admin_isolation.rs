//! Admin isolation (design §13). Static half: only the sync module may touch
//! user sections or user data keys. Dynamic half: a user's sections carry
//! marker strings, and no admin route ever answers with one (or with the
//! section ciphertext, or the org's secrets).

mod common;

use std::path::{Path, PathBuf};

use common::*;
use remail_proto::sync::{SectionDoc, Stamp};
use remail_server::Mode;
use serde_json::{json, Value};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Source without comments (doc comments may name what they forbid).
fn code(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn only_sections_decrypts_user_data() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let sections = src.join("sections");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(files.len() > 10);
    for f in &files {
        if f.starts_with(&sections) {
            continue;
        }
        let c = code(f);
        for needle in ["decrypt_user_section", "encrypt_user_section", "SyncPath(", "user_sections"] {
            assert!(!c.contains(needle), "{} uses {needle}: only src/sections/ may", f.display());
        }
        // `user_key` is defined in crypto/envelope.rs; nobody else calls it.
        if !f.ends_with("crypto/envelope.rs") {
            assert!(!c.contains("user_key("), "{} asks for a user data key", f.display());
        }
    }
}

#[test]
fn admin_module_has_no_path_to_user_data() {
    let admin = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/admin");
    let mut files = Vec::new();
    rust_files(&admin, &mut files);
    assert!(!files.is_empty());
    for f in &files {
        let c = code(f);
        for needle in ["sections", "user_key", "OwnerKind::User", "crypto::open", "decrypt"] {
            assert!(!c.contains(needle), "{} references {needle}", f.display());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_admin_route_returns_user_data() {
    let Some(s) = start_with(Mode::Hosted, |c| c.admin_emails = vec!["boss@example.com".into()]).await else { return };
    let boss = token(&s.new_user("boss@example.com", PASSWORD).await);
    let user = s.new_user("u@example.com", PASSWORD).await;
    let t = token(&user);
    let user_id = user["user"]["id"].as_str().unwrap().to_string();

    // Markers in every section the user can write.
    for (i, id) in remail_proto::sections::FIXED.iter().enumerate() {
        if *id == remail_proto::sections::ACCOUNTS {
            continue; // checked against the allow-list; a cred section stands in
        }
        let mut doc = SectionDoc::default();
        doc.record_mut("rec_1").set("note", json!(format!("MARKER-{id}")), &Stamp::new(1 + i as i64, "dev_a"));
        let r = s.put_section(&t, id, 0, &serde_json::to_value(&doc).unwrap()).await;
        assert_eq!(r.status, 200, "{id}: {:?}", r.body);
    }
    let mut cred = SectionDoc::default();
    cred.record_mut("secret").set("password", json!("MARKER-password"), &Stamp::new(99, "dev_a"));
    assert_eq!(s.put_section(&t, "cred:acc_1", 0, &serde_json::to_value(&cred).unwrap()).await.status, 200);
    // And the org's own secrets.
    let r = s.send(s.http.put(s.api("/admin/ai")).bearer_auth(&boss).json(&json!({
        "ai": {"enabled": true, "provider": "openai", "baseUrl": "https://llm.example.com/v1", "model": "m", "maxTokens": 4096},
        "apiKey": "MARKER-ai-key"
    })))
    .await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    let ciphertexts: Vec<Vec<u8>> = sqlx::query_scalar("select ciphertext from user_sections").fetch_all(&s.db).await.unwrap();
    assert!(ciphertexts.len() >= 8);

    let routes: Vec<(&str, String, Value)> = vec![
        ("GET", "/admin/config".into(), Value::Null),
        ("GET", "/admin/users".into(), Value::Null),
        ("GET", "/admin/users?query=u%40".into(), Value::Null),
        ("PUT", "/admin/providers".into(), json!({"providers": {"imap": true}})),
        (
            "PUT",
            "/admin/ai".into(),
            json!({"ai": {"enabled": true, "provider": "openai", "baseUrl": "https://llm.example.com/v1", "model": "m2", "maxTokens": 4096}}),
        ),
        ("PUT", format!("/admin/users/{user_id}/role"), json!({"role": "admin"})),
        ("PUT", format!("/admin/users/{user_id}/role"), json!({"role": "user"})),
    ];
    for (method, path, body) in routes {
        let req = match method {
            "GET" => s.http.get(s.api(&path)),
            _ => s.http.put(s.api(&path)).json(&body),
        };
        let r = s.send(req.bearer_auth(&boss)).await;
        assert_eq!(r.status, 200, "{method} {path}: {:?}", r.body);
        let text = r.body.to_string();
        assert!(!text.contains("MARKER"), "{method} {path} leaked: {text}");
        for ct in &ciphertexts {
            use base64::Engine as _;
            for enc in [base64::engine::general_purpose::STANDARD.encode(ct), base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ct)] {
                assert!(!text.contains(&enc[..enc.len().min(24)]), "{method} {path} returned section ciphertext");
            }
        }
    }
    s.finish().await;
}
