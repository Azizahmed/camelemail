//! Envelope encryption against Postgres: round trip, rotation v1→v2 with
//! everything still decrypting, prune refused while referenced, verify.

mod common;

use common::*;
use remail_proto::sync::{SectionDoc, Stamp, SINGLETON};
use remail_server::crypto::{dk_aad, open, seal, section_aad, DataKey, KeyProvider};
use remail_server::{keyops, Mode};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rotation_rewraps_without_reencrypting() {
    let Some(s) = start(Mode::Hosted).await else { return };
    // A few users with sections, plus org secrets.
    let mut tokens = Vec::new();
    for i in 0..3 {
        let t = token(&s.new_user(&format!("k{i}@example.com"), PASSWORD).await);
        let mut d = SectionDoc::default();
        d.record_mut(SINGLETON).set("marker", json!(format!("user-{i}")), &Stamp::new(1, "d"));
        assert_eq!(s.put_section(&t, "prefs", 0, &serde_json::to_value(&d).unwrap()).await.status, 200);
        tokens.push(t);
    }
    let org = remail_server::orgs::by_slug(&s.db, "public").await.unwrap().unwrap();
    let secrets = remail_proto::org::OrgSecrets { google_client_secret: "gs".into(), ..Default::default() };
    remail_server::orgs::set_secrets(&s.db, &s.state.env, &org.id, &secrets).await.unwrap();

    let before: Vec<Vec<u8>> = sqlx::query_scalar("select ciphertext from user_sections order by user_id").fetch_all(&s.db).await.unwrap();
    let count = |v: i32| {
        let db = s.db.clone();
        async move {
            sqlx::query_scalar::<_, i64>("select count(*) from wrapped_keys where mk_version = $1").bind(v).fetch_one(&db).await.unwrap()
        }
    };
    assert_eq!(count(1).await, 4, "3 users + 1 org");

    // Rotate: MK2 active, rows re-wrapped in batches.
    assert_eq!(s.keys.rotate(), 2);
    s.state.env.register_master_keys(&s.db).await.unwrap();
    // Prune refuses while anything still references v1.
    let err = keyops::check_prunable(&s.db, s.keys.as_ref(), 1).await.unwrap_err();
    assert!(err.to_string().contains("still wrapped"), "{err}");
    assert!(keyops::check_prunable(&s.db, s.keys.as_ref(), 2).await.is_err(), "active key");

    let n = keyops::rewrap_all(&s.db, s.keys.as_ref(), |_| {}).await.unwrap();
    assert_eq!(n, 4);
    assert_eq!(count(1).await, 0);
    assert_eq!(count(2).await, 4);
    let after: Vec<Vec<u8>> = sqlx::query_scalar("select ciphertext from user_sections order by user_id").fetch_all(&s.db).await.unwrap();
    assert_eq!(before, after, "data is not re-encrypted");
    let retired: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("select retired_at from master_keys where version = 1").fetch_one(&s.db).await.unwrap();
    assert!(retired.is_some());

    // Everything decrypts (cold cache, so through the new wrapping).
    s.state.env.clear_cache();
    for (i, t) in tokens.iter().enumerate() {
        let r = s.get("/sections/prefs", Some(t)).await;
        assert_eq!(r.status, 200, "{:?}", r.body);
        assert_eq!(r.body["doc"]["records"]["_"]["fields"]["marker"]["value"], format!("user-{i}"));
    }
    assert_eq!(remail_server::orgs::secrets(&s.db, &s.state.env, &org.id).await.unwrap().google_client_secret, "gs");
    let report = keyops::verify(&s.db, s.keys.as_ref()).await.unwrap();
    assert!(report.ok(), "{report:?}");
    assert_eq!(report.total, 4);

    // New users after rotation get v2 wrappings.
    s.new_user("late@example.com", PASSWORD).await;
    assert_eq!(count(2).await, 5);

    // Now v1 can go; afterwards nothing needs it.
    keyops::check_prunable(&s.db, s.keys.as_ref(), 1).await.unwrap();
    s.keys.remove(1).unwrap();
    keyops::forget_version(&s.db, 1).await.unwrap();
    s.state.env.clear_cache();
    assert_eq!(s.get("/sections/prefs", Some(&tokens[0])).await.status, 200);
    assert!(keyops::verify(&s.db, s.keys.as_ref()).await.unwrap().ok());
    s.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_reports_a_broken_wrapping() {
    let Some(s) = start(Mode::Hosted).await else { return };
    s.new_user("v@example.com", PASSWORD).await;
    // Swap the user's wrapped key for the org's: the AAD names the owner.
    sqlx::query(
        "update wrapped_keys u set wrapped = o.wrapped from wrapped_keys o where u.owner_kind = 'user' and o.owner_kind = 'org'",
    )
    .execute(&s.db)
    .await
    .unwrap();
    let r = keyops::verify(&s.db, s.keys.as_ref()).await.unwrap();
    assert!(!r.ok());
    assert_eq!(r.failed.len(), 1);
    s.finish().await;
}

#[test]
fn envelope_round_trip_and_wrong_aad() {
    let p = remail_server::crypto::MemoryKeyProvider::new("dep_t");
    let dk = DataKey::generate();
    let (v, wrapped) = p.wrap("org_1", "usr_1", &dk).unwrap();
    assert!(open(&[0u8; 32], &dk_aad("org_1", "usr_1", v), &wrapped).is_err());
    let back = p.unwrap(v, "org_1", "usr_1", &wrapped).unwrap();
    let blob = seal(back.bytes(), &section_aad("usr_1", "prefs", 1), b"{}");
    assert_eq!(&**open(dk.bytes(), &section_aad("usr_1", "prefs", 1), &blob).unwrap(), b"{}");
    assert!(open(dk.bytes(), &section_aad("usr_1", "prefs", 2), &blob).is_err());
    assert!(p.unwrap(v, "org_1", "usr_2", &wrapped).is_err());
}

/// A server started with a different master key (a lost key file replaced by
/// `key init`, a mistyped secret) must refuse to run, not wrap new data under it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_refuses_a_foreign_master_key() {
    use base64::Engine as _;
    use remail_server::crypto::keys::KeyRing;
    use remail_server::crypto::{KeyFile, MemoryKeyProvider};

    let Some(s) = start(Mode::Hosted).await else { return };
    s.new_user("owner@example.com", PASSWORD).await;
    s.state.env.register_master_keys(&s.db).await.unwrap();
    keyops::check_keys(&s.db, s.keys.as_ref()).await.expect("the key the data was made with");

    let other = MemoryKeyProvider::from_ring(KeyRing::generate(remail_proto::new_id("dep")));
    let err = keyops::check_keys(&s.db, &other).await.unwrap_err().to_string();
    assert!(err.contains("not the one this database was set up with"), "{err}");

    // The secret form: key file JSON, or the same JSON in base64 (one line).
    let ring = KeyRing::generate(remail_proto::new_id("dep"));
    let json = serde_json::to_string(&ring.to_file()).unwrap();
    let from_json = KeyFile::parse_secret(&json).unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&json);
    let from_b64 = KeyFile::parse_secret(&format!(" {b64}\n")).unwrap();
    assert_eq!(from_json.fingerprint(1), ring.fingerprint(1));
    assert_eq!(from_b64.fingerprint(1), ring.fingerprint(1));
    assert!(KeyFile::parse_secret("not a key").is_err());
    s.finish().await;
}
