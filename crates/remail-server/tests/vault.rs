//! End-to-end encrypted mailbox sign-ins, from the server's side: it never
//! sees the password, only a proof and the wrapped vault key
//! (docs/platform/zero-knowledge.md). Prelogin, sign-up and sign-in with a
//! proof, converting an existing account (at sign-in or signed in), old apps,
//! changing and resetting the password.

mod common;

use common::*;
use remail_proto::sync::{SectionDoc, Stamp, SINGLETON};
use remail_server::Mode;
use serde_json::{json, Value};

/// A `cred:*` section holding one sealed sign-in.
fn sealed_cred(key_id: &str) -> Value {
    let mut doc = SectionDoc::default();
    let stamp = Stamp::new(1000, "dev_a");
    let r = doc.record_mut(SINGLETON);
    r.set("kind", json!("sealed"), &stamp);
    r.set("vk", json!(key_id), &stamp);
    r.set("sealed", json!("bm9uY2VjaXBoZXJ0ZXh0"), &stamp);
    serde_json::to_value(&doc).unwrap()
}

fn proof(c: char) -> String {
    format!("v1.{}", c.to_string().repeat(43))
}

fn vault(key_id: &str, salt: &str) -> Value {
    json!({
        "kdf": {"alg": "argon2id", "mKib": 65536, "t": 3, "p": 1, "salt": salt},
        "keyEnc": "A".repeat(80),
        "keyId": key_id,
    })
}

fn device(name: &str) -> Value {
    json!({"name": name, "platform": "android", "appVersion": "0.5.0"})
}

async fn verified_signup(s: &Server, email: &str, body: Value) {
    let r = s.post("/auth/signup", body, None).await;
    assert_eq!(r.status, 202, "{:?}", r.body);
    let t = s.mailed_token(email, "verify").await;
    assert_eq!(s.post("/auth/verify", json!({"token": t}), None).await.status, 204);
}

async fn login(s: &Server, email: &str, password: &str) -> Resp {
    s.post("/auth/login", json!({"email": email, "password": password, "device": device("Phone")}), None).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proofs_and_vaults() {
    let Some(s) = start(Mode::Hosted).await else { return };

    // Prelogin: unknown addresses get made-up settings, the same each time.
    let a = s.post("/auth/prelogin", json!({"email": "nobody@example.com"}), None).await;
    assert_eq!(a.status, 200);
    assert_eq!(a.body["kdf"]["alg"], "argon2id");
    let b = s.post("/auth/prelogin", json!({"email": "Nobody@Example.com "}), None).await;
    assert_eq!(a.body, b.body);
    let other = s.post("/auth/prelogin", json!({"email": "someone@example.com"}), None).await;
    assert_ne!(a.body["kdf"]["salt"], other.body["kdf"]["salt"]);

    // Sign-up with a proof: no strength check (the server can't see the password), no password stored.
    let email = "zk@example.com";
    let v1 = vault("vk_first1", "c2FsdHNhbHRzYWx0c2FsdA==");
    let bad = s.post("/auth/signup", json!({"name": "Zk", "email": email, "password": "short", "vault": v1}), None).await;
    assert_eq!(bad.status, 400, "a proof must look like one");
    verified_signup(&s, email, json!({"name": "Zk", "email": email, "password": proof('a'), "vault": v1})).await;
    let pre = s.post("/auth/prelogin", json!({"email": email}), None).await;
    assert_eq!(pre.body["kdf"], v1["kdf"]);

    // Sign-in with the proof hands back the wrapped vault key.
    let r = login(&s, email, &proof('a')).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    assert_eq!(r.body["vault"], v1);
    assert_eq!(r.body["user"]["vaultKeyId"], "vk_first1");
    let phone = token(&r.body);
    assert_eq!(s.get("/me/vault", Some(&phone)).await.body, v1);
    // Refreshing doesn't repeat it.
    let refreshed = s.post("/auth/refresh", json!({"refreshToken": refresh_token(&r.body)}), None).await;
    assert!(refreshed.body.get("vault").is_none(), "{:?}", refreshed.body);
    let phone = token(&refreshed.body);

    // An old app sends the password itself: told to update, and that isn't a failed sign-in.
    for _ in 0..6 {
        let old = login(&s, email, PASSWORD).await;
        assert_eq!((old.status.as_u16(), old.code()), (426, "update_required"));
    }
    assert_eq!(login(&s, email, &proof('b')).await.code(), "invalid_credentials");
    assert_eq!(login(&s, email, &proof('a')).await.status, 200, "no lockout from old apps");

    // Sealed sign-ins sync like any section; readable ones are refused (an app from before).
    let mut plain = SectionDoc::default();
    let at = Stamp::new(1000, "dev_old");
    plain.record_mut(SINGLETON).set("kind", json!("password"), &at);
    plain.record_mut(SINGLETON).set("password", json!("hunter2"), &at);
    let r = s.put_section(&phone, "cred:acc_1", 0, &serde_json::to_value(&plain).unwrap()).await;
    assert_eq!(r.code(), "bad_request", "{:?}", r.body);
    assert_eq!(s.put_section(&phone, "cred:acc_1", 0, &sealed_cred("vk_first1")).await.status, 200);
    let mut prefs = SectionDoc::default();
    prefs.record_mut(SINGLETON).set("theme", json!("dark"), &Stamp::new(1000, "dev_a"));
    assert_eq!(s.put_section(&phone, "prefs", 0, &serde_json::to_value(&prefs).unwrap()).await.status, 200);

    // Changing the password re-wraps the same vault key.
    let moved = vault("vk_first1", "bmV3c2FsdG5ld3NhbHRuZQ==");
    let r = s.post("/me/password", json!({"current": PASSWORD, "new": proof('c'), "vault": moved}), Some(&phone)).await;
    assert_eq!(r.code(), "update_required", "the current secret is a proof too");
    let r = s.post("/me/password", json!({"current": proof('a'), "new": proof('c'), "vault": vault("vk_other1", "bmV3c2FsdG5ld3NhbHRuZQ==")}), Some(&phone)).await;
    assert_eq!(r.status, 400, "another vault key would strand the sealed sign-ins");
    let r = s.post("/me/password", json!({"current": proof('a'), "new": proof('c')}), Some(&phone)).await;
    assert_eq!(r.code(), "update_required");
    let r = s.post("/me/password", json!({"current": proof('a'), "new": proof('c'), "vault": moved}), Some(&phone)).await;
    assert_eq!(r.status, 204, "{:?}", r.body);
    assert_eq!(login(&s, email, &proof('a')).await.code(), "invalid_credentials");
    let r = login(&s, email, &proof('c')).await;
    assert_eq!(r.body["vault"], moved);
    let phone = token(&r.body);
    assert_eq!(s.get("/sections/cred:acc_1", Some(&phone)).await.status, 200, "still there");

    // Forgot password: a new vault key, and the sealed sign-ins nobody can open are gone.
    assert_eq!(s.post("/auth/reset/request", json!({"email": email}), None).await.status, 202);
    let t = s.mailed_token(email, "reset").await;
    let fresh = vault("vk_second", "cmVzZXRzYWx0cmVzZXRzYQ==");
    assert_eq!(s.post("/auth/reset", json!({"token": t, "password": proof('d'), "vault": fresh}), None).await.status, 204);
    let r = login(&s, email, &proof('d')).await;
    assert_eq!(r.body["vault"], fresh);
    let phone = token(&r.body);
    assert_eq!(s.get("/sections/cred:acc_1", Some(&phone)).await.body["version"], 0, "deleted");
    assert_eq!(s.get("/sections/prefs", Some(&phone)).await.body["version"], 1, "other settings stay");

    // 2FA can be turned off with the proof (and not with a password).
    let r = s.post("/me/2fa/disable", json!({"password": PASSWORD, "code": "123456"}), Some(&phone)).await;
    assert_eq!(r.code(), "update_required");
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn converting_existing_accounts() {
    let Some(s) = start(Mode::Hosted).await else { return };

    // An account from before: prelogin says so, the password works as ever.
    let email = "old@example.com";
    let laptop = s.new_user(email, PASSWORD).await;
    assert_eq!(s.post("/auth/prelogin", json!({"email": email}), None).await.body["kdf"], Value::Null);
    let laptop = token(&laptop);
    assert_eq!(s.get("/me/vault", Some(&laptop)).await.body, Value::Null);

    // The new app converts it at sign-in: the password once more, then the proof.
    let v = vault("vk_convert", "Y29udmVydHNhbHRjb252ZQ==");
    let wrong = s.post("/auth/login", json!({"email": email, "password": "wrong password here", "device": device("Phone"), "upgrade": {"proof": proof('e'), "vault": v}}), None).await;
    assert_eq!(wrong.code(), "invalid_credentials");
    assert_eq!(s.post("/auth/prelogin", json!({"email": email}), None).await.body["kdf"], Value::Null, "a wrong password converts nothing");
    let r = s.post("/auth/login", json!({"email": email, "password": PASSWORD, "device": device("Phone"), "upgrade": {"proof": proof('e'), "vault": v}}), None).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    assert_eq!(r.body["vault"], v);
    assert_eq!(s.post("/auth/prelogin", json!({"email": email}), None).await.body["kdf"], v["kdf"]);
    assert_eq!(login(&s, email, PASSWORD).await.code(), "update_required");
    assert_eq!(login(&s, email, &proof('e')).await.status, 200);
    // The laptop stays signed in and can read the vault to unlock.
    assert_eq!(s.get("/me/vault", Some(&laptop)).await.body, v);
    let again = s.post("/me/vault", json!({"password": PASSWORD, "upgrade": {"proof": proof('f'), "vault": v}}), Some(&laptop)).await;
    assert_eq!(again.code(), "conflict", "converted once only");

    // A device signed in before the conversion converts it itself.
    let email = "signedin@example.com";
    let t = token(&s.new_user(email, PASSWORD).await);
    let v = vault("vk_signedin", "c2lnbmVkaW5zYWx0c2lnbg==");
    let wrong = s.post("/me/vault", json!({"password": "nope nope nope", "upgrade": {"proof": proof('g'), "vault": v}}), Some(&t)).await;
    assert_eq!(wrong.code(), "invalid_credentials");
    let r = s.post("/me/vault", json!({"password": PASSWORD, "upgrade": {"proof": proof('g'), "vault": v}}), Some(&t)).await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    assert_eq!(s.get("/me", Some(&t)).await.body["vaultKeyId"], "vk_signedin");
    assert_eq!(login(&s, email, &proof('g')).await.status, 200);

    // An old app resetting the password of a converted account: back to a
    // plain password, and the sealed sign-ins (unreadable now) are dropped.
    let t = token(&login(&s, email, &proof('g')).await.body);
    assert_eq!(s.put_section(&t, "cred:acc_2", 0, &sealed_cred("vk_signedin")).await.status, 200);
    assert_eq!(s.post("/auth/reset/request", json!({"email": email}), None).await.status, 202);
    let rt = s.mailed_token(email, "reset").await;
    let new = "violet canyon lamp ninety";
    assert_eq!(s.post("/auth/reset", json!({"token": rt, "password": new}), None).await.status, 204);
    assert_eq!(s.post("/auth/prelogin", json!({"email": email}), None).await.body["kdf"], Value::Null);
    let r = login(&s, email, new).await;
    assert_eq!(r.status, 200);
    assert_eq!(s.get("/sections/cred:acc_2", Some(&token(&r.body))).await.body["version"], 0);
    s.finish().await;
}
