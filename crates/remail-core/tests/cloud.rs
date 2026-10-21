//! The Remail server connection against a mock server (`common::server`):
//! setup moving to a second device, the migration choices, writes that race
//! (412 → merge → retry), a token refresh racing an account edit, an account
//! removed while another device edits it offline, a revoked device being
//! wiped, the organisation's allow-list, and Google's server-side exchange.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::server::{self, Mock, SharedMock};
use common::*;
use remail_core::cloud::{Cloud, CloudEvent, CloudOptions, CloudSink, LoginResult, Phase};
use remail_core::engine::{AppSettings, AutoClean, CleanRule, UiPrefsPatch};
use remail_core::proto::api::DeviceInfo;
use remail_core::proto::sync::{SectionDoc, Stamp};
use remail_core::secrets::{MemoryStore, SecretStore};
use remail_core::store::{IncomingMessage, Store};
use remail_core::*;

#[derive(Default)]
struct Events(Mutex<Vec<String>>);

impl CloudSink for Events {
    fn cloud_event(&self, e: CloudEvent) {
        self.0.lock().unwrap().push(serde_json::to_string(&e).unwrap());
    }
}

struct Device {
    engine: Engine,
    cloud: Cloud,
    secrets: Arc<MemoryStore>,
    events: Arc<Events>,
}

async fn mock() -> (SharedMock, String) {
    let state: SharedMock = Arc::new(Mutex::new(Mock::default()));
    let port = serve(server::router(state.clone())).await;
    (state, format!("http://127.0.0.1:{port}"))
}

async fn device(url: &str) -> Device {
    let secrets = Arc::new(MemoryStore::default());
    let engine = Engine::with_store_locked(Store::open_in_memory().unwrap(), secrets.clone(), Arc::new(Quiet)).unwrap();
    let events = Arc::new(Events::default());
    let opts = CloudOptions {
        default_server: Some(url.to_string()),
        poll_with_notices: Duration::from_secs(3600),
        poll_without_notices: Duration::from_secs(3600),
        notices: false,
        first_sync_wait: Duration::from_secs(5),
    };
    let info = DeviceInfo { name: "test".into(), platform: "linux".into(), app_version: "0".into() };
    let cloud = Cloud::new(engine.clone(), events.clone(), info, opts).unwrap();
    cloud.start().await.unwrap();
    Device { engine, cloud, secrets, events }
}

async fn wait_phase(d: &Device, phase: Phase) {
    for _ in 0..200 {
        if d.cloud.status().phase == phase {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never reached {phase:?}: {:?}", d.cloud.status());
}

async fn sign_in(d: &Device) {
    match d.cloud.login("aziz@example.com", "correct horse").await.unwrap() {
        LoginResult::Ok { .. } => {}
        other => panic!("{other:?}"),
    }
}

/// A device signed in with nothing of its own: ready once the setup is down.
async fn ready_device(url: &str) -> Device {
    let d = device(url).await;
    sign_in(&d).await;
    wait_phase(&d, Phase::Ready).await;
    d
}

fn new_account(email: &str) -> NewAccount {
    // A closed local port: workers fail fast without touching the network.
    let srv = ServerConfig { host: "127.0.0.1".into(), port: 1, security: Security::None };
    NewAccount { email: email.into(), display_name: "Me".into(), username: String::new(), password: String::new(), imap: srv.clone(), smtp: srv, allow_invalid_certs: false }
}

fn add_local_account(d: &Device, email: &str, secret: &str) -> Account {
    let a = d.engine.store().insert_account(&new_account(email), false, "password").unwrap();
    d.secrets.set(&a.id, secret).unwrap();
    a
}

fn put_message(store: &Store, account_id: &str, message_id: &str) -> String {
    let raw = format!(
        "From: Sara <sara@acme.test>\r\nTo: me@x.com\r\nSubject: Lunch\r\nMessage-ID: <{message_id}>\r\nDate: Mon, 19 Oct 2026 09:00:00 +0000\r\n\r\nHi\r\n"
    )
    .into_bytes();
    let parsed = remail_core::mime::parse(&raw).unwrap();
    let thread_id = format!("t:{message_id}");
    store
        .upsert_message(&IncomingMessage {
            account_id: account_id.into(),
            thread_id: thread_id.clone(),
            folder: "INBOX".into(),
            path: "INBOX".into(),
            uid_validity: 1,
            uid: 1,
            date: remail_core::store::now_ms(),
            is_unread: true,
            is_starred: false,
            is_draft: false,
            is_answered: false,
            parsed,
            raw: Some(raw),
        })
        .unwrap();
    thread_id
}

fn puts(m: &SharedMock) -> Vec<(String, String, i64, u16)> {
    m.lock().unwrap().puts.clone()
}

/// What two devices must agree on.
fn setup_view(d: &Device) -> serde_json::Value {
    view_of(&d.engine, &d.secrets)
}

fn view_of(e: &Engine, secrets: &MemoryStore) -> serde_json::Value {
    let accounts: Vec<serde_json::Value> = e
        .accounts()
        .unwrap()
        .into_iter()
        .map(|mut a| {
            a.auto_bcc.sort(); // a set: its order isn't kept
            serde_json::json!([a.id, a.email, a.display_name, a.username, a.imap, a.smtp, a.color, a.sort_order, a.hidden_in_unified, a.auto_bcc, a.auth,
                secrets.get(&a.id).unwrap().map(|s| serde_json::from_str::<serde_json::Value>(&s).unwrap_or(serde_json::json!(s)))])
        })
        .collect();
    let s = e.settings();
    let ai = e.ai_settings();
    let cals: Vec<serde_json::Value> = e
        .cal_accounts()
        .unwrap()
        .into_iter()
        .map(|c| {
            let looks: Vec<_> = c.calendars.iter().map(|k| (k.name.clone(), k.color.clone(), k.enabled)).collect();
            serde_json::json!([c.id, c.kind, c.url, c.username, c.mail_account_id, looks, secrets.get(&format!("cal:{}", c.id)).unwrap()])
        })
        .collect();
    serde_json::json!({
        "accounts": accounts,
        "signatures": e.signatures().unwrap(),
        "templates": e.templates().unwrap().into_iter().map(|t| (t.id, t.name, t.subject, t.html)).collect::<Vec<_>>(),
        "senders": e.sender_rules().unwrap().into_iter().map(|r| (r.email, r.category)).collect::<Vec<_>>(),
        "prefs": [s.theme, s.undo_send_seconds.to_string(), s.language, s.auto_clean.enabled.to_string(),
                  serde_json::to_string(&s.auto_clean.rules.iter().map(|r| (&r.kind, r.after_minutes)).collect::<Vec<_>>()).unwrap(),
                  s.calendar.week_start.to_string()],
        "ai": [ai.enabled.to_string(), ai.instructions],
        "ui": e.ui_prefs(),
        "calendars": cals,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_moves_to_a_second_device() {
    let (m, url) = mock().await;
    // An iCal feed behind a password, for a subscription with its own credential.
    let feed = axum::Router::new().route(
        "/feed.ics",
        axum::routing::get(|| async { "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Team\r\nEND:VCALENDAR\r\n" }),
    );
    let feed_port = serve(feed).await;

    // Computer A has setup from before the Remail server.
    let a = device(&url).await;
    assert_eq!(a.cloud.status().phase, Phase::SignedOut);
    assert!(!a.engine.gate_open(), "no mail before signing in");
    let work = add_local_account(&a, "work@x.com", "pw-work");
    let ms = a.engine.store().insert_account(&new_account("me@outlook.com"), false, "microsoft").unwrap();
    a.secrets.set(&ms.id, r#"{"provider":"microsoft","clientId":"c","refreshToken":"rt-ms","subject":"t/o"}"#).unwrap();
    a.secrets.set("ai-api-key", "sk-local").unwrap();
    let e = &a.engine;
    e.update_account_prefs(&work.id, Some("Work".into()), Some("#ff0000".into()), Some(true), Some(vec!["bcc@x.com".into()])).unwrap();
    e.save_signature(Signature { id: String::new(), account_id: Some(work.id.clone()), name: "Work sig".into(), html: "<p>W</p>".into(), is_default: true }).unwrap();
    e.save_signature(Signature { id: String::new(), account_id: None, name: "Global".into(), html: "<p>G</p>".into(), is_default: false }).unwrap();
    e.save_template(Template { id: String::new(), name: "Thanks".into(), subject: "Thank you".into(), html: "<p>Thanks!</p>".into(), updated_at: 0 }).unwrap();
    e.set_sender_category("news@shop.test", Some("promotions")).unwrap();
    let mut s: AppSettings = e.settings();
    s.theme = "dark".into();
    s.undo_send_seconds = 10;
    s.language = "ar".into();
    s.notifications = false; // this computer only
    s.calendar.week_start = 1;
    s.auto_clean = AutoClean { enabled: true, action: "archive".into(), rules: vec![CleanRule { kind: "promotions".into(), after_minutes: 30, since: 0 }] };
    e.set_settings(&s).unwrap();
    let mut ai = e.ai_settings();
    ai.enabled = true;
    ai.instructions = "Be brief".into();
    e.set_ai_settings(&ai, None).unwrap();
    let shortcuts: BTreeMap<String, Vec<String>> = [("compose".to_string(), vec!["c".to_string()])].into();
    let patch = UiPrefsPatch {
        shortcuts: Some(shortcuts),
        sidebar_hidden: Some(Some(vec!["SPAM".into(), format!("folder:{}:Receipts", work.id)])),
        last_compose_account: Some(Some(work.id.clone())),
    };
    e.set_ui_prefs(patch).unwrap();
    let sub = e
        .cal_subscribe(remail_core::calendar::CalSubscribe {
            url: format!("http://127.0.0.1:{feed_port}/feed.ics"),
            name: None,
            username: Some("feeduser".into()),
            password: Some("feedpass".into()),
        })
        .await
        .unwrap();
    e.cal_set_calendar(&sub.calendars[0].id, Some(false), Some("#00ff00".into())).unwrap();
    let thread = put_message(e.store(), &work.id, "lunch@acme.test");
    let until = remail_core::store::now_ms() + 3_600_000;
    e.snooze(&[engine::ThreadRef { account_id: work.id.clone(), thread_id: thread }], until).unwrap();

    // Signing in offers to move it.
    sign_in(&a).await;
    assert_eq!(a.cloud.status().phase, Phase::Migrate);
    let info = a.cloud.migration().await.unwrap();
    assert_eq!((info.local_accounts, info.server_accounts), (2, 0));
    a.cloud.migrate(true).await.unwrap();
    wait_phase(&a, Phase::Ready).await;
    assert!(a.engine.gate_open());
    assert_eq!(a.secrets.get("ai-api-key").unwrap(), None, "the AI key moves to the organisation");
    let accounts_doc = m.lock().unwrap().doc("accounts");
    assert_eq!(accounts_doc.live().count(), 2);
    let rec = accounts_doc.record(&ms.id).unwrap();
    assert_eq!(rec.get("provider"), Some(&serde_json::json!("microsoft")));
    assert!(rec.get("allowInvalidCerts").is_none() && rec.get("status").is_none());
    assert!(m.lock().unwrap().sections.contains_key(&format!("cred:{}", work.id)));

    // A second computer signs in and gets all of it.
    let b = device(&url).await;
    sign_in(&b).await;
    assert_eq!(b.cloud.status().phase, Phase::Syncing);
    wait_phase(&b, Phase::Ready).await;
    assert!(b.engine.gate_open());
    assert!(b.engine.sync_started(&work.id), "mail started for each mailbox");
    assert_eq!(setup_view(&b), setup_view(&a));
    assert!(b.engine.settings().notifications, "device settings stay on their device");
    // The snoozed conversation's mail hasn't synced to B yet: it waits for it.
    let b_thread = put_message(b.engine.store(), &work.id, "lunch@acme.test");
    assert_eq!(b.engine.store().snoozed_until(&work.id, &b_thread).unwrap(), None);
    b.cloud.sync_now().await.unwrap();
    assert_eq!(b.engine.store().snoozed_until(&work.id, &b_thread).unwrap(), Some(until));

    // Settled: syncing again writes nothing (collect and apply agree).
    let before = puts(&m).len();
    a.cloud.sync_now().await.unwrap();
    b.cloud.sync_now().await.unwrap();
    a.cloud.sync_now().await.unwrap();
    assert_eq!(puts(&m)[before..], [], "no ping-pong");

    // A change on B reaches A, and A's windows are told to reload settings.
    let mut s = b.engine.settings();
    s.theme = "light".into();
    b.engine.set_settings(&s).unwrap();
    b.cloud.sync_now().await.unwrap();
    a.cloud.sync_now().await.unwrap();
    assert_eq!(a.engine.settings().theme, "light");
    assert!(!a.engine.settings().notifications);
}

#[tokio::test(flavor = "multi_thread")]
async fn migration_choices() {
    let (m, url) = mock().await;
    // The account already has a mailbox (from another computer).
    let first = ready_device(&url).await;
    let shared = add_local_account(&first, "same@x.com", "pw");
    first.cloud.sync_now().await.unwrap();
    assert!(m.lock().unwrap().doc("accounts").record(&shared.id).is_some());

    // This computer has the same mailbox under its own id, and another one.
    let c = device(&url).await;
    let mine = add_local_account(&c, "same@x.com", "pw-local");
    let thread = put_message(c.engine.store(), &mine.id, "m1@x");
    let other = add_local_account(&c, "other@x.com", "pw-other");
    sign_in(&c).await;
    assert_eq!(c.cloud.status().phase, Phase::Migrate);
    let info = c.cloud.migration().await.unwrap();
    assert_eq!((info.local_accounts, info.server_accounts), (2, 1));
    c.cloud.migrate(true).await.unwrap();
    wait_phase(&c, Phase::Ready).await;
    let ids: Vec<String> = c.engine.accounts().unwrap().into_iter().map(|a| a.id).collect();
    assert!(ids.contains(&shared.id) && ids.contains(&other.id) && !ids.contains(&mine.id), "{ids:?}");
    // Its mail moved with it.
    assert_eq!(c.engine.store().thread_messages(&shared.id, &thread).unwrap().len(), 1);
    assert_eq!(m.lock().unwrap().doc("accounts").live().count(), 2);

    // Another computer discards its own setup for the account's.
    let d = device(&url).await;
    add_local_account(&d, "local-only@x.com", "pw");
    sign_in(&d).await;
    assert_eq!(d.cloud.status().phase, Phase::Migrate);
    d.cloud.migrate(false).await.unwrap();
    wait_phase(&d, Phase::Ready).await;
    let mut emails: Vec<String> = d.engine.accounts().unwrap().into_iter().map(|a| a.email).collect();
    emails.sort();
    assert_eq!(emails, ["other@x.com", "same@x.com"]);
    assert!(m.lock().unwrap().doc("accounts").live().all(|(_, r)| r.get("email") != Some(&serde_json::json!("local-only@x.com"))));
}

#[tokio::test(flavor = "multi_thread")]
async fn racing_writes_merge_and_retry() {
    let (m, url) = mock().await;
    let a = ready_device(&url).await;
    a.engine.save_template(Template { id: "tmpl_a".into(), name: "From A".into(), subject: String::new(), html: "a".into(), updated_at: 0 }).unwrap();
    // Another device writes the templates between A's read and A's write.
    let mut theirs = SectionDoc::default();
    let at = Stamp::new(remail_core::store::now_ms(), "dev_z");
    let r = theirs.record_mut("tmpl_z");
    r.set("name", serde_json::json!("From Z"), &at);
    r.set("subject", serde_json::json!(""), &at);
    r.set("html", serde_json::json!("z"), &at);
    m.lock().unwrap().race.insert("templates".into(), theirs);
    let before = puts(&m).len();
    a.cloud.sync_now().await.unwrap();
    let mine: Vec<(String, u16)> = puts(&m)[before..].iter().filter(|p| p.1 == "templates").map(|p| (p.1.clone(), p.3)).collect();
    assert_eq!(mine, [("templates".to_string(), 412), ("templates".to_string(), 200)]);
    let server = m.lock().unwrap().doc("templates");
    assert!(server.record("tmpl_a").is_some() && server.record("tmpl_z").is_some());
    let mut names: Vec<String> = a.engine.templates().unwrap().into_iter().map(|t| t.name).collect();
    names.sort();
    assert_eq!(names, ["From A", "From Z"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn token_refresh_and_account_edit_both_survive() {
    let (m, url) = mock().await;
    let a = ready_device(&url).await;
    let acc = a.engine.store().insert_account(&new_account("me@outlook.com"), false, "microsoft").unwrap();
    let grant = |rt: &str| format!(r#"{{"provider":"microsoft","clientId":"c","refreshToken":"{rt}","subject":"t/o"}}"#);
    a.secrets.set(&acc.id, &grant("rt-1")).unwrap();
    a.cloud.sync_now().await.unwrap();
    let b = ready_device(&url).await;
    assert_eq!(b.engine.accounts().unwrap().len(), 1);

    // B edits the account while A's Microsoft refresh token rotates.
    b.engine.update_account_prefs(&acc.id, None, Some("#123456".into()), None, None).unwrap();
    b.cloud.sync_now().await.unwrap();
    a.secrets.set(&acc.id, &grant("rt-2")).unwrap();
    let before = puts(&m).len();
    a.cloud.sync_now().await.unwrap();
    let written: Vec<String> = puts(&m)[before..].iter().map(|p| format!("{}:{}", p.1, p.3)).collect();
    assert_eq!(written, [format!("cred:{}:200", acc.id)], "only the credential is written, with no conflict");
    b.cloud.sync_now().await.unwrap();
    for d in [&a, &b] {
        assert_eq!(d.engine.accounts().unwrap()[0].color, "#123456");
        let g: serde_json::Value = serde_json::from_str(&d.secrets.get(&acc.id).unwrap().unwrap()).unwrap();
        assert_eq!(g["refreshToken"], "rt-2");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn removed_account_stays_removed() {
    let (m, url) = mock().await;
    let a = ready_device(&url).await;
    let acc = add_local_account(&a, "gone@x.com", "pw");
    a.cloud.sync_now().await.unwrap();
    let b = ready_device(&url).await;
    // B edits it without reaching the server; then A removes it.
    b.engine.update_account_prefs(&acc.id, Some("Renamed".into()), None, None, None).unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    a.engine.remove_account(&acc.id).unwrap();
    a.cloud.sync_now().await.unwrap();
    // B comes back.
    b.cloud.sync_now().await.unwrap();
    assert!(b.engine.accounts().unwrap().is_empty());
    assert_eq!(b.secrets.get(&acc.id).unwrap(), None);
    let server = m.lock().unwrap().doc("accounts");
    assert!(server.record(&acc.id).is_none(), "still removed on the server");
    assert!(m.lock().unwrap().doc(&format!("cred:{}", acc.id)).record("_").is_none(), "its credential is gone too");
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_out_devices_are_wiped() {
    let (m, url) = mock().await;
    let a = ready_device(&url).await;
    let acc = add_local_account(&a, "me@x.com", "pw");
    a.engine.save_template(Template { id: String::new(), name: "T".into(), subject: String::new(), html: String::new(), updated_at: 0 }).unwrap();
    a.cloud.sync_now().await.unwrap();
    let b = ready_device(&url).await;
    assert_eq!(b.engine.accounts().unwrap().len(), 1);
    assert!(b.secrets.get(remail_core::cloud::SESSION_ID).unwrap().is_some());

    // The server signs B out: its next contact ends the session and wipes the setup.
    let device = serde_json::from_str::<serde_json::Value>(&b.secrets.get(remail_core::cloud::SESSION_ID).unwrap().unwrap()).unwrap()["deviceId"]
        .as_str()
        .unwrap()
        .to_string();
    m.lock().unwrap().revoke(&device);
    assert!(b.cloud.sync_now().await.is_err());
    wait_phase(&b, Phase::SignedOut).await;
    assert!(!b.engine.gate_open());
    assert!(b.engine.accounts().unwrap().is_empty() && b.engine.templates().unwrap().is_empty());
    assert_eq!(b.secrets.get(&acc.id).unwrap(), None);
    assert_eq!(b.secrets.get(remail_core::cloud::SESSION_ID).unwrap(), None);
    assert!(b.events.0.lock().unwrap().iter().any(|e| e.contains(r#""kind":"signedOut","reason":"expired""#)));

    // Signing out by hand does the same.
    a.cloud.logout().await.unwrap();
    assert_eq!(a.cloud.status().phase, Phase::SignedOut);
    assert!(a.engine.accounts().unwrap().is_empty());
    assert!(a.events.0.lock().unwrap().iter().any(|e| e.contains(r#""reason":"user""#)));
    // The server's copy is untouched: signing in again brings it back.
    sign_in(&a).await;
    wait_phase(&a, Phase::Ready).await;
    assert_eq!(a.engine.accounts().unwrap()[0].id, acc.id);
    assert_eq!(a.secrets.get(&acc.id).unwrap().as_deref(), Some("pw"));
}

#[tokio::test(flavor = "multi_thread")]
async fn organisation_allow_list() {
    let (m, url) = mock().await;
    {
        let mut mm = m.lock().unwrap();
        mm.org.providers.google = false;
        mm.org.providers.manual_servers = false;
        mm.org.providers.presets.push(remail_core::proto::org::ServerPreset { imap_host: "mail.acme.com".into(), ..Default::default() });
    }
    let a = ready_device(&url).await;
    let providers = a.cloud.providers().await.unwrap();
    let google = providers.iter().find(|p| p.kind == remail_core::proto::org::ProviderKind::Google).unwrap();
    assert!(!google.enabled);
    let mut gmail = new_account("me@gmail.com");
    gmail.imap.host = "imap.gmail.com".into();
    let e = engine::friendly_error(&a.engine.add_account(gmail).await.unwrap_err());
    assert!(e.starts_with("cloud:provider_not_allowed:"), "{e}");
    let e = engine::friendly_error(&a.engine.add_account(new_account("me@elsewhere.com")).await.unwrap_err());
    assert!(e.starts_with("cloud:provider_not_allowed:"), "{e}");
    assert!(a.engine.microsoft_client_id().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn google_code_exchanged_by_the_server() {
    let (m, url) = mock().await;
    {
        let mut mm = m.lock().unwrap();
        mm.org.providers.google_app.client_id = "gclient".into();
        mm.google_server_exchange = true;
    }
    let idp: Shared = Arc::new(Mutex::new(Idp::default()));
    std::env::set_var("REMAIL_GOOGLE_CALDAV", "http://127.0.0.1:1/caldav");
    // Google's own endpoint: a refresh must never come here (the org's client has a secret).
    let google_port = serve(axum::Router::new().route("/token", axum::routing::post(google_token)).with_state(idp.clone())).await;
    std::env::set_var("REMAIL_GOOGLE_AUTHORITY", format!("http://127.0.0.1:{google_port}"));
    let accepted = Arc::new(Mutex::new(vec!["gat-1".to_string(), "gat-2".to_string()]));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let imap = ServerConfig { host: "127.0.0.1".into(), port: fake_imap(accepted.clone(), seen.clone()).await, security: Security::None };
    let smtp = ServerConfig { host: "127.0.0.1".into(), port: fake_smtp(accepted).await, security: Security::None };
    let a = ready_device(&url).await;
    let req = OAuthAccountRequest { imap: Some(imap), smtp: Some(smtp), ..Default::default() };
    let added = a.engine.add_google_account(google_browser(idp.clone(), "code-s", "me@gmail.com", "sub-me"), req).await.unwrap();
    assert_eq!(added.account.auth, "google");
    assert_eq!(m.lock().unwrap().google_exchanges, ["code-s"]);
    let g: serde_json::Value = serde_json::from_str(&a.secrets.get(&added.account.id).unwrap().unwrap()).unwrap();
    assert_eq!((g["refreshToken"].as_str(), g["subject"].as_str()), (Some("grt-1"), Some("sub-me")));
    a.cloud.sync_now().await.unwrap();
    let doc = m.lock().unwrap().doc("accounts");
    assert_eq!(doc.record(&added.account.id).unwrap().get("provider"), Some(&serde_json::json!("google")));

    // A second computer has no access token yet: it refreshes through the server too.
    let b = ready_device(&url).await;
    let id = added.account.id.clone();
    for _ in 0..100 {
        if seen.lock().unwrap().iter().any(|t| t == "gat-2") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen.lock().unwrap().iter().any(|t| t == "gat-2"), "B signed in to IMAP with the token the server refreshed");
    assert_eq!(m.lock().unwrap().google_refreshes, ["grt-1"]);
    assert!(idp.lock().unwrap().token_requests.iter().all(|(grant, _)| grant != "refresh_token"), "Google itself was never asked");
    // Google didn't rotate it: the credential stays as it was.
    let g: serde_json::Value = serde_json::from_str(&b.secrets.get(&id).unwrap().unwrap()).unwrap();
    assert_eq!(g["refreshToken"], "grt-1");
}

#[tokio::test(flavor = "multi_thread")]
async fn mapping_round_trip_without_server() {
    use remail_core::cloud::mapping;
    use std::collections::BTreeSet;
    let a_secrets = Arc::new(MemoryStore::default());
    let a = Engine::with_store_locked(Store::open_in_memory().unwrap(), a_secrets.clone(), Arc::new(Quiet)).unwrap();
    let acc = a.store().insert_account(&new_account("me@x.com"), false, "password").unwrap();
    a_secrets.set(&acc.id, "pw").unwrap();
    a.update_account_prefs(&acc.id, Some("Mine".into()), Some("#abcdef".into()), None, Some(vec!["b@x.com".into(), "a@x.com".into()])).unwrap();
    a.save_signature(Signature { id: String::new(), account_id: Some(acc.id.clone()), name: "S".into(), html: "<b>s</b>".into(), is_default: true }).unwrap();
    a.save_template(Template { id: String::new(), name: "T".into(), subject: "s".into(), html: "h".into(), updated_at: 0 }).unwrap();
    a.set_sender_category("x@y.com", Some("primary")).unwrap();
    let mut s = a.settings();
    s.theme = "dark".into();
    s.language = "en".into();
    a.set_settings(&s).unwrap();
    let patch = UiPrefsPatch { shortcuts: Some([("archive".to_string(), vec!["e".to_string()])].into()), sidebar_hidden: Some(Some(vec!["SPAM".into()])), ..Default::default() };
    a.set_ui_prefs(patch).unwrap();

    let b_secrets = Arc::new(MemoryStore::default());
    let b = Engine::with_store_locked(Store::open_in_memory().unwrap(), b_secrets.clone(), Arc::new(Quiet)).unwrap();
    let mut sections: Vec<String> = remail_core::proto::sections::FIXED.iter().map(|s| s.to_string()).collect();
    sections.push(format!("cred:{}", acc.id));
    sections.sort_by_key(|s| mapping::apply_order(s));
    let mut collected = Vec::new();
    for (i, id) in sections.iter().enumerate() {
        let local = mapping::collect(&a, id).unwrap().unwrap();
        collected.push(local.clone());
        let mut doc = SectionDoc::default();
        mapping::stamp(&mut doc, &local, &BTreeSet::new(), &Stamp::new(1000 + i as i64, "dev_a"));
        // Through JSON, as it travels.
        let doc: SectionDoc = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        mapping::apply(&b, id, &doc, None, false).unwrap();
        // Applying is idempotent.
        mapping::apply(&b, id, &doc, None, false).unwrap();
    }
    // Collecting the copy gives the same records (nothing to send back).
    for (id, local) in sections.iter().zip(collected) {
        assert_eq!(mapping::collect(&b, id).unwrap().unwrap(), local, "{id}");
    }
    assert_eq!(view_of(&b, &b_secrets), view_of(&a, &a_secrets));
}
