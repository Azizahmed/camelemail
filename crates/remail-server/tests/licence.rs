//! On-prem licence enforcement: expired blocks new sign-ins (not refresh,
//! not admins), seat limit blocks new users, tampering is caught.

mod common;

use chrono::{Duration, NaiveDate, Utc};
use common::*;
use ed25519_dalek::SigningKey;
use remail_proto::licence::{sign, Licence};
use remail_server::crypto::KeyProvider;
use remail_server::Mode;
use serde_json::json;

fn licence(deployment: &str, lid: &str, seats: u32, expires: NaiveDate) -> Licence {
    Licence {
        lid: lid.into(),
        customer: "Acme".into(),
        deployment: deployment.into(),
        seats,
        features: vec!["ai".into()],
        starts: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap(),
        expires,
        issued: Utc::now(),
        renew_url: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expiry_and_seats() {
    let key = SigningKey::generate(&mut rand_core::OsRng);
    let pk = key.verifying_key();
    let Some(s) = start_with(Mode::Onprem, |c| c.licence_keys = vec![pk]).await else { return };
    let dep = s.keys.deployment();
    let st = &s.state;

    // No licence yet: invalid, but the first admin can still get in.
    assert_eq!(s.get("/meta", None).await.body["licence"], "invalid");
    let invite = s.first_invite.clone().unwrap().split("t=").nth(1).unwrap().to_string();
    let r = s.post("/auth/signup", json!({"name": "Admin", "email": "admin@acme.com", "password": PASSWORD, "invite": invite}), None).await;
    assert_eq!(r.status, 202, "{:?}", r.body);
    assert_eq!(s.login("admin@acme.com", PASSWORD, "L").await.status, 200, "admins may sign in to install a licence");

    // Wrong deployment or a forged licence is refused at install.
    let far = (Utc::now() + Duration::days(365)).date_naive();
    let wrong = sign(&licence("dep_other", "lic_w", 3, far), &key);
    assert!(remail_server::licence::install(&st.cfg, &s.db, &st.env, &wrong).await.is_err());
    let forged = sign(&licence(&dep, "lic_f", 3, far), &SigningKey::generate(&mut rand_core::OsRng));
    assert!(remail_server::licence::install(&st.cfg, &s.db, &st.env, &forged).await.is_err());

    // A valid licence with 3 seats.
    let raw = sign(&licence(&dep, "lic_1", 3, far), &key);
    remail_server::licence::install(&st.cfg, &s.db, &st.env, &raw).await.unwrap();
    assert_eq!(s.get("/meta", None).await.body["licence"], "valid");
    let org = s.org_id().await;
    for who in ["u1", "u2"] {
        let inv = s.invite(&org, None, "user").await;
        let r = s.post("/auth/signup", json!({"name": who, "email": format!("{who}@acme.com"), "password": PASSWORD, "invite": inv}), None).await;
        assert_eq!(r.status, 202, "{:?}", r.body);
    }
    // Seat 4: refused.
    let inv = s.invite(&org, None, "user").await;
    let r = s.post("/auth/signup", json!({"name": "u3", "email": "u3@acme.com", "password": PASSWORD, "invite": inv}), None).await;
    assert_eq!(r.code(), "seat_limit");
    assert!(remail_server::licence::check_seat(&st.cfg, &s.db, &st.env).await.is_err(), "CLI invite refused too");

    let user = s.login("u1@acme.com", PASSWORD, "Laptop").await;
    assert_eq!(user.status, 200);

    // Expired (past the 14-day grace): new sign-ins refused, refresh still fine.
    let old = (Utc::now() - Duration::days(30)).date_naive();
    let raw = sign(&licence(&dep, "lic_2", 3, old), &key);
    remail_server::licence::install(&st.cfg, &s.db, &st.env, &raw).await.unwrap();
    assert_eq!(s.get("/meta", None).await.body["licence"], "expired");
    let r = s.login("u2@acme.com", PASSWORD, "Laptop").await;
    assert_eq!(r.status, 403);
    assert_eq!(r.code(), "licence_expired");
    let r = s.post("/auth/refresh", json!({"refreshToken": refresh_token(&user.body)}), None).await;
    assert_eq!(r.status, 200, "existing sessions keep working: {:?}", r.body);
    let access = token(&r.body);
    assert_eq!(s.get("/sections", Some(&access)).await.status, 200);
    assert_eq!(s.login("admin@acme.com", PASSWORD, "L2").await.status, 200);

    // Grace: still works, says so.
    let recent = (Utc::now() - Duration::days(3)).date_naive();
    let raw = sign(&licence(&dep, "lic_3", 3, recent), &key);
    remail_server::licence::install(&st.cfg, &s.db, &st.env, &raw).await.unwrap();
    assert_eq!(s.get("/meta", None).await.body["licence"], "grace");
    assert_eq!(s.login("u2@acme.com", PASSWORD, "Laptop").await.status, 200);

    // Tampering with the clock record makes the licence invalid.
    sqlx::query("update deployment set last_seen_clock = last_seen_clock - interval '1 hour'").execute(&s.db).await.unwrap();
    assert_eq!(s.get("/meta", None).await.body["licence"], "invalid");
    assert_eq!(s.login("u2@acme.com", PASSWORD, "Laptop").await.code(), "licence_invalid");
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clock_rollback() {
    let key = SigningKey::generate(&mut rand_core::OsRng);
    let pk = key.verifying_key();
    let Some(s) = start_with(Mode::Onprem, |c| c.licence_keys = vec![pk]).await else { return };
    let st = &s.state;
    let dep = s.keys.deployment();
    let raw = sign(&licence(&dep, "lic_1", 10, (Utc::now() + Duration::days(365)).date_naive()), &key);
    remail_server::licence::install(&st.cfg, &s.db, &st.env, &raw).await.unwrap();
    let now = Utc::now();
    let ok = remail_server::licence::status_at(&st.cfg, &s.db, &st.env, now).await.unwrap();
    assert_eq!(ok.state, remail_proto::api::LicenceState::Valid);
    // Two days earlier than the high-water mark: rolled back.
    let back = remail_server::licence::status_at(&st.cfg, &s.db, &st.env, now - Duration::days(2)).await.unwrap();
    assert_eq!(back.state, remail_proto::api::LicenceState::Invalid);
    assert!(back.problem.unwrap().contains("clock"));
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hosted_and_unlicensed_builds_enforce_nothing() {
    let Some(s) = start(Mode::Hosted).await else { return };
    assert_eq!(s.get("/meta", None).await.body["licence"], "none");
    s.finish().await;
    let Some(s) = start(Mode::Onprem).await else { return };
    assert_eq!(s.get("/meta", None).await.body["licence"], "none", "no issuer keys compiled in");
    s.finish().await;
}
