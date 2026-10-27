//! The real client against the real server, end to end.
//!
//! `remail-server` runs in-process on a fresh Postgres database (hosted mode,
//! mail kept in the dev outbox); two "computers" are full engines with their
//! own stores and secret stores, connected through `cloud::Cloud`. Mail comes
//! from GreenMail; Microsoft sign-in from the mock identity platform in
//! `common`. Device B talks to the server through a proxy the test can cut,
//! to take it offline.
//!
//! a. A signs up (verification link from the dev outbox), signs in, turns on
//!    two-step sign-in, adds a GreenMail mailbox and sets preferences.
//! b. B signs in with password + TOTP and gets the same setup (same ids,
//!    credentials) and the mailbox's mail; a snooze and its cancellation by
//!    new mail travel too.
//! c. Offline edits: different fields both survive, the same field goes to
//!    the later edit.
//! d. A removes the mailbox while B, offline, edits it: it goes from B too
//!    (worker stopped, credential deleted).
//! e. The admin turns IMAP off: the org notice reaches both devices over the
//!    notices socket, B can't add an IMAP mailbox, a raw PUT gets 422.
//! f. Microsoft: A adds an account, B gets it and its credential, B's token
//!    refresh (rotating) writes only `cred:<id>` while A edits the account.
//! g. A signs B out: B's setup and credentials are wiped.
//! h. Changing the password keeps the synced sign-ins readable (a new device
//!    gets them); a password reset signs A out everywhere and, the vault key
//!    being new, drops them: signing in again finds the rest of the setup.
//! j. An account from before end-to-end encryption converts at sign-in; a
//!    device without the vault key is locked until the password unlocks it.
//!
//! Throughout, mailbox sign-ins reach the server sealed with the vault key,
//! which only the password unwraps (docs/platform/zero-knowledge.md).
//!
//! Needs `REMAIL_PLATFORM_IT=1`, `REMAIL_TEST_DATABASE_URL`
//! (`scripts/test-postgres.sh`) and GreenMail (`scripts/test-server.sh`):
//!
//! ```sh
//! eval "$(scripts/test-postgres.sh)"; ./scripts/test-server.sh
//! REMAIL_PLATFORM_IT=1 cargo test -p remail-core --test platform_e2e -- --nocapture
//! ```

mod common;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::routing::post;
use axum::Router;
use common::*;
use remail_core::cloud::{password_proof, Cloud, CloudEvent, CloudOptions, CloudSink, LoginResult, Phase, VaultState, SESSION_ID};
use remail_core::engine::{ThreadRef, UiPrefsPatch};
use remail_core::proto::api::{DeviceInfo, Kdf, SignupRequest};
use remail_core::proto::org::OrgConfig;
use remail_core::proto::sections::account;
use remail_core::proto::sync::{SectionDoc, Stamp};
use remail_core::secrets::{MemoryStore, SecretStore};
use remail_core::store::Store;
use remail_core::*;
use remail_server::testing::TestServer;
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

const EMAIL: &str = "aziz@remail.test";
const PASSWORD: &str = "correct horse battery staple";
const NEW_PASSWORD: &str = "tangerine elephant quarterback";
const CHANGED_PASSWORD: &str = "marble orchard twilight sixty";

// ----------------------------------------------------------------------------
// Devices
// ----------------------------------------------------------------------------

#[derive(Default)]
struct Events(Mutex<Vec<String>>);

impl CloudSink for Events {
    fn cloud_event(&self, e: CloudEvent) {
        self.0.lock().unwrap().push(serde_json::to_string(&e).unwrap());
    }
}

impl Events {
    fn has(&self, needle: &str) -> bool {
        self.0.lock().unwrap().iter().any(|e| e.contains(needle))
    }
}

struct Device {
    name: &'static str,
    engine: Engine,
    cloud: Cloud,
    secrets: Arc<MemoryStore>,
    events: Arc<Events>,
    _dir: tempfile::TempDir,
}

async fn device(name: &'static str, server: &str) -> Device {
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemoryStore::default());
    let store = Store::open(&dir.path().join("remail.db")).unwrap();
    let engine = Engine::with_store_locked(store, secrets.clone(), Arc::new(Quiet)).unwrap();
    let events = Arc::new(Events::default());
    let opts = CloudOptions {
        default_server: None,
        // Only notices (and explicit syncs) move changes: polling would hide a broken socket.
        poll_with_notices: Duration::from_secs(3600),
        poll_without_notices: Duration::from_secs(3600),
        notices: true,
        first_sync_wait: Duration::from_secs(20),
    };
    let info = DeviceInfo { name: name.into(), platform: "linux".into(), app_version: "0.2.1".into() };
    let cloud = Cloud::new(engine.clone(), events.clone(), info, opts).unwrap();
    cloud.start().await.unwrap();
    assert_eq!(cloud.status().phase, Phase::SignedOut);
    let meta = cloud.set_server(server).await.unwrap();
    assert_eq!(meta.mode, "hosted");
    Device { name, engine, cloud, secrets, events, _dir: dir }
}

impl Device {
    fn account(&self, id: &str) -> Option<Account> {
        self.engine.accounts().unwrap().into_iter().find(|a| a.id == id)
    }

    fn secret(&self, id: &str) -> Option<String> {
        self.secrets.get(id).unwrap()
    }

    fn grant(&self, id: &str) -> Value {
        serde_json::from_str(&self.secret(id).unwrap_or_else(|| panic!("{}: no credential for {id}", self.name))).unwrap()
    }

    /// What two devices of the same user must agree on.
    fn setup(&self) -> Value {
        let e = &self.engine;
        let accounts: Vec<Value> = e
            .accounts()
            .unwrap()
            .into_iter()
            .map(|a| {
                // Grants compare as JSON (writers order the keys differently).
                let secret = self.secret(&a.id).map(|s| serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s)));
                json!([a.id, a.email, a.display_name, a.username, a.imap, a.smtp, a.color, a.auth, secret])
            })
            .collect();
        let s = e.settings();
        json!({
            "accounts": accounts,
            "signatures": e.signatures().unwrap(),
            "templates": e.templates().unwrap().into_iter().map(|t| (t.id, t.name, t.subject, t.html)).collect::<Vec<_>>(),
            "prefs": [s.theme, s.undo_send_seconds],
            "ui": e.ui_prefs(),
        })
    }

    async fn wait_phase(&self, phase: Phase, secs: u64) {
        until(&format!("{} to reach {phase:?}", self.name), secs, || self.cloud.status().phase == phase).await;
    }
}

async fn until(what: &str, secs: u64, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ----------------------------------------------------------------------------
// A proxy that can be cut (device B going offline)
// ----------------------------------------------------------------------------

struct Proxy {
    url: String,
    cut: watch::Sender<bool>,
}

impl Proxy {
    async fn start(upstream: SocketAddr) -> Proxy {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let (cut, rx) = watch::channel(false);
        tokio::spawn(async move {
            loop {
                let Ok((mut inbound, _)) = l.accept().await else { return };
                let mut rx = rx.clone();
                if *rx.borrow() {
                    continue; // offline: the connection just drops
                }
                tokio::spawn(async move {
                    let Ok(mut out) = TcpStream::connect(upstream).await else { return };
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut inbound, &mut out) => {}
                        _ = rx.wait_for(|c| *c) => {}
                    }
                });
            }
        });
        Proxy { url, cut }
    }

    fn offline(&self) {
        self.cut.send_replace(true);
    }

    fn online(&self) {
        self.cut.send_replace(false);
    }
}

// ----------------------------------------------------------------------------
// Server-side helpers (what an admin, the CLI or another client does)
// ----------------------------------------------------------------------------

async fn dev_mail(server: &TestServer) -> Vec<Value> {
    reqwest::get(format!("{}/v1/dev/mail", server.url)).await.unwrap().json().await.unwrap()
}

/// The token of the newest `remail://<kind>?t=` link mailed to `to`.
async fn mailed_token(server: &TestServer, to: &str, kind: &str) -> String {
    let marker = format!("remail://{kind}?t=");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mails = dev_mail(server).await;
        if let Some(body) = mails.iter().rev().filter(|m| m["to"] == to).filter_map(|m| m["body"].as_str()).find(|b| b.contains(&marker)) {
            let start = body.find(&marker).unwrap() + marker.len();
            return body[start..].split_whitespace().next().unwrap().to_string();
        }
        assert!(Instant::now() < deadline, "no {kind} mail to {to}: {mails:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The proof `password` gives for `email` (what an app sends instead of the password).
async fn proof_for(server: &TestServer, email: &str, password: &str) -> String {
    let r: Value =
        reqwest::Client::new().post(format!("{}/v1/auth/prelogin", server.url)).json(&json!({ "email": email })).send().await.unwrap().json().await.unwrap();
    let kdf: Kdf = serde_json::from_value(r["kdf"].clone()).unwrap_or_else(|_| panic!("no key settings for {email}: {r}"));
    password_proof(password, &kdf).unwrap()
}

/// Signs in over plain HTTP (a client that isn't Remail) with a backup code.
async fn raw_sign_in(server: &TestServer, password: &str, backup_code: &str) -> String {
    let http = reqwest::Client::new();
    let proof = proof_for(server, EMAIL, password).await;
    let r: Value = http
        .post(format!("{}/v1/auth/login", server.url))
        .json(&json!({"email": EMAIL, "password": proof, "device": {"name": "curl", "platform": "linux", "appVersion": "0"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let challenge = r["challenge"].as_str().unwrap_or_else(|| panic!("expected a 2FA challenge: {r}"));
    let t: Value = http
        .post(format!("{}/v1/auth/login/2fa", server.url))
        .json(&json!({"challenge": challenge, "code": backup_code}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    t["accessToken"].as_str().unwrap_or_else(|| panic!("2fa: {t}")).to_string()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

// ----------------------------------------------------------------------------
// Mail
// ----------------------------------------------------------------------------

fn run_id() -> String {
    format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis())
}

fn greenmail_account(email: &str) -> NewAccount {
    let host = std::env::var("REMAIL_IT_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    NewAccount {
        email: email.into(),
        display_name: "Aziz".into(),
        username: email.into(),
        password: "greenmail-pw".into(),
        imap: ServerConfig { host: host.clone(), port: 3143, security: Security::None },
        smtp: ServerConfig { host, port: 3025, security: Security::None },
        allow_invalid_certs: false,
    }
}

async fn smtp_send(to: &str, subject: &str, message_id: &str, in_reply_to: Option<&str>) {
    use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
    let host = std::env::var("REMAIL_IT_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let mut m = Message::builder()
        .from("Sara <sara@acme.test>".parse().unwrap())
        .to(to.parse().unwrap())
        .subject(subject)
        .message_id(Some(format!("<{message_id}>")));
    if let Some(parent) = in_reply_to {
        m = m.in_reply_to(format!("<{parent}>")).references(format!("<{parent}>"));
    }
    let m = m.body(format!("{subject}\r\n")).unwrap();
    let smtp = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host).port(3025).build();
    smtp.send(m).await.expect("GreenMail SMTP (scripts/test-server.sh)");
}

fn inbox(e: &Engine, account_id: &str) -> Vec<ThreadSummary> {
    e.list_threads(&ThreadQuery {
        account_id: Some(account_id.into()),
        label: labels::INBOX.into(),
        unread_only: false,
        search: None,
        limit: 100,
        before: None,
        before_account_id: None,
        before_thread_id: None,
        category: None,
    })
    .unwrap()
}

fn thread_with(e: &Engine, account_id: &str, subject: &str) -> Option<String> {
    inbox(e, account_id).into_iter().find(|t| t.subject == subject).map(|t| t.thread_id)
}

// ----------------------------------------------------------------------------
// The scenario
// ----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn platform_end_to_end() {
    if std::env::var("REMAIL_PLATFORM_IT").is_err() {
        eprintln!("skipped: set REMAIL_PLATFORM_IT=1 (with REMAIL_TEST_DATABASE_URL and GreenMail running) to run the platform e2e test");
        return;
    }
    let Some(db_url) = remail_server::testing::database_url() else {
        eprintln!("skipped: REMAIL_TEST_DATABASE_URL is not set (eval \"$(scripts/test-postgres.sh)\")");
        return;
    };
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_test_writer().try_init();

    // The mock Microsoft identity platform (process-wide: this is the only test here).
    let idp: Shared = Arc::new(Mutex::new(Idp::default()));
    let idp_port = serve(Router::new().route("/common/oauth2/v2.0/token", post(token)).with_state(idp.clone())).await;
    std::env::set_var("REMAIL_MS_AUTHORITY", format!("http://127.0.0.1:{idp_port}/common"));
    std::env::set_var("REMAIL_MS_GRAPH", format!("http://127.0.0.1:{idp_port}/v1.0"));

    let server = remail_server::testing::start(&db_url, remail_server::Mode::Hosted, |_| {}).await.expect("start the server");
    let upstream: SocketAddr = server.url.trim_start_matches("http://").parse().unwrap();
    let proxy = Proxy::start(upstream).await;

    // ------------------------------------------------------------------
    // a. Device A: sign up, verify, sign in, 2FA, a mailbox, preferences
    // ------------------------------------------------------------------
    let a = device("Device A", &server.url).await;
    let signup = a
        .cloud
        .signup(SignupRequest { name: "Aziz".into(), email: EMAIL.into(), password: PASSWORD.into(), invite: None, vault: None, locale: Some("en".into()) })
        .await
        .unwrap();
    assert_eq!(signup.verify, "email");
    let pre: Value = reqwest::Client::new().post(format!("{}/v1/auth/prelogin", server.url)).json(&json!({"email": EMAIL})).send().await.unwrap().json().await.unwrap();
    assert_eq!(pre["kdf"]["alg"], "argon2id", "the account signs in with a proof from the start");
    // Not before the address is confirmed.
    let early = a.cloud.login(EMAIL, PASSWORD).await.unwrap_err();
    assert_eq!(early.code, "email_not_verified", "{early}");
    let verify = mailed_token(&server, EMAIL, "verify").await;
    a.cloud.verify_email(&verify).await.unwrap();
    assert_eq!(a.cloud.login(EMAIL, "not the password at all").await.unwrap_err().code, "invalid_credentials");
    match a.cloud.login(EMAIL, PASSWORD).await.unwrap() {
        LoginResult::Ok { me } => assert_eq!(me.email, EMAIL),
        other => panic!("{other:?}"),
    }
    a.wait_phase(Phase::Ready, 20).await;
    assert!(a.engine.gate_open());
    assert_eq!(a.cloud.status().vault, VaultState::Ready, "A holds the vault key");

    // The organisation allows IMAP with any server (an admin's `org config set`).
    let org = server.org_of(EMAIL).await.unwrap();
    let mut cfg: OrgConfig = server.org_config(&org.id).await.unwrap();
    cfg.providers.imap = true;
    cfg.providers.manual_servers = true;
    server.set_org_config(&org.id, &cfg).await.unwrap();
    until("A to hear about the org config", 10, || a.engine.org().is_some_and(|o| o.providers.imap)).await;

    // Two-step sign-in.
    let totp = a.cloud.totp_setup(None).await.unwrap();
    assert!(totp.otpauth_url.starts_with("otpauth://totp/"));
    let codes = a.cloud.totp_enable(&TestServer::totp_code(&totp.secret, now_secs()).unwrap(), None).await.unwrap();
    assert_eq!(codes.backup_codes.len(), 10);
    assert!(a.cloud.me().await.unwrap().totp);

    // A GreenMail mailbox and preferences.
    let run = run_id();
    let mailbox = format!("aziz{run}@remail.test");
    // Mail already waiting (GreenMail makes the mailbox on delivery).
    let hello_id = format!("hello-{run}@acme.test");
    smtp_send(&mailbox, "Hello from Sara", &hello_id, None).await;
    let acc = a.engine.add_account(greenmail_account(&mailbox)).await.unwrap();
    let e = &a.engine;
    let mut s = e.settings();
    s.theme = "dark".into();
    s.undo_send_seconds = 10;
    e.set_settings(&s).unwrap();
    e.save_template(Template { id: String::new(), name: "Thanks".into(), subject: "Thank you".into(), html: "<p>Thanks!</p>".into(), updated_at: 0 }).unwrap();
    e.save_signature(Signature { id: String::new(), account_id: Some(acc.id.clone()), name: "Work".into(), html: "<p>— Aziz</p>".into(), is_default: true })
        .unwrap();
    let shortcuts: BTreeMap<String, Vec<String>> = [("compose".to_string(), vec!["c".to_string()]), ("archive".to_string(), vec!["e".to_string(), "y".to_string()])].into();
    e.set_ui_prefs(UiPrefsPatch {
        shortcuts: Some(shortcuts.clone()),
        sidebar_hidden: Some(Some(vec!["SPAM".into(), format!("folder:{}:Receipts", acc.id)])),
        last_compose_account: Some(Some(acc.id.clone())),
        ..Default::default()
    })
    .unwrap();
    // Everything reaches the server without anyone asking for a sync.
    for section in ["accounts", "prefs", "shortcuts", "templates", "signatures", &format!("cred:{}", acc.id)] {
        let deadline = Instant::now() + Duration::from_secs(10);
        while server.section_meta(EMAIL, section).await.unwrap().is_none() {
            assert!(Instant::now() < deadline, "{section} never reached the server");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    // The server holds ciphertext only.
    let raw = String::from_utf8_lossy(&server.raw_sections(EMAIL).await.unwrap()).to_string();
    assert!(!raw.contains("greenmail-pw") && !raw.contains(&mailbox) && !raw.contains("Thank you"), "sections are encrypted at rest");


    // ------------------------------------------------------------------
    // b. Device B: password + TOTP, the same setup, the same mail
    // ------------------------------------------------------------------
    let b = device("Device B", &proxy.url).await;
    let challenge = match b.cloud.login(EMAIL, PASSWORD).await.unwrap() {
        LoginResult::TwoFactor { challenge, setup_required } => {
            assert!(!setup_required);
            challenge
        }
        other => panic!("expected two-step sign-in: {other:?}"),
    };
    assert_eq!(b.cloud.login_2fa(&challenge, "000000").await.unwrap_err().code, "invalid_code");
    // (The code A just used can't be replayed; the next step's is fine.)
    b.cloud.login_2fa(&challenge, &TestServer::totp_code(&totp.secret, now_secs() + 30).unwrap()).await.unwrap();
    assert_eq!(b.cloud.status().phase, Phase::Syncing);
    b.wait_phase(Phase::Ready, 30).await;
    assert_eq!(b.cloud.status().vault, VaultState::Ready, "the password unwrapped the vault key on B too");
    assert!(b.events.has(r#""phase":"syncing""#), "B showed the first sync");
    assert!(b.engine.gate_open());
    let b_acc = b.account(&acc.id).expect("B has the mailbox under the same id");
    assert_eq!((b_acc.email.as_str(), b_acc.auth.as_str()), (mailbox.as_str(), "password"));
    assert_eq!(b.secret(&acc.id).as_deref(), Some("greenmail-pw"), "B has its credential");
    assert_eq!(b.setup(), a.setup(), "template, signature, prefs, shortcuts and hidden folders are the same");
    assert_eq!(b.engine.ui_prefs().shortcuts, shortcuts);
    assert_eq!(b.engine.settings().theme, "dark");
    until("B to sync Sara's mail from GreenMail", 30, || thread_with(&b.engine, &acc.id, "Hello from Sara").is_some()).await;
    until("A to sync Sara's mail", 30, || thread_with(&a.engine, &acc.id, "Hello from Sara").is_some()).await;

    // A change on A reaches B through a notice (B never polls in this test).
    a.engine
        .set_ui_prefs(UiPrefsPatch {
            sidebar_hidden: Some(Some(vec!["SPAM".into(), format!("folder:{}:Receipts", acc.id), format!("folder:{}:Old", acc.id)])),
            ..Default::default()
        })
        .unwrap();
    until("B to get the hidden folder", 10, || b.engine.ui_prefs().sidebar_hidden.is_some_and(|h| h.iter().any(|e| e.ends_with(":Old")))).await;

    // A snooze travels; new mail in the conversation cancels it everywhere.
    let a_thread = thread_with(&a.engine, &acc.id, "Hello from Sara").unwrap();
    let b_thread = thread_with(&b.engine, &acc.id, "Hello from Sara").unwrap();
    let wake = remail_core::store::now_ms() + 3_600_000;
    a.engine.snooze(&[ThreadRef { account_id: acc.id.clone(), thread_id: a_thread.clone() }], wake).unwrap();
    until("B to get the snooze", 10, || b.engine.store().snoozed_until(&acc.id, &b_thread).unwrap() == Some(wake)).await;
    let snoozes_v = server.section_meta(EMAIL, "snoozes").await.unwrap().unwrap().0;
    smtp_send(&mailbox, "Re: Hello from Sara", &format!("reply-{run}@acme.test"), Some(&hello_id)).await;
    until("the reply to cancel A's snooze", 30, || a.engine.store().snoozed_until(&acc.id, &a_thread).unwrap().is_none()).await;
    until("the reply to cancel B's snooze", 30, || b.engine.store().snoozed_until(&acc.id, &b_thread).unwrap().is_none()).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (v, _) = server.section_meta(EMAIL, "snoozes").await.unwrap().unwrap();
        if v > snoozes_v {
            break;
        }
        assert!(Instant::now() < deadline, "the cancelled snooze never reached the server");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // ------------------------------------------------------------------
    // c. Offline edits: different fields merge, the same field goes to the later edit
    // ------------------------------------------------------------------
    proxy.offline();
    b.engine.update_account_prefs(&acc.id, Some("Aziz at work".into()), None, None, None).unwrap();
    until("B to notice it's offline", 10, || b.cloud.status().offline).await;
    a.engine.update_account_prefs(&acc.id, None, Some("#ff0000".into()), None, None).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(b.account(&acc.id).unwrap().color, acc.color, "B is offline");
    proxy.online();
    b.cloud.sync_now().await.unwrap();
    assert!(!b.cloud.status().offline);
    let both = |d: &Device| d.account(&acc.id).is_some_and(|x| x.display_name == "Aziz at work" && x.color == "#ff0000");
    until("both edits to survive on B", 10, || both(&b)).await;
    until("both edits to survive on A", 10, || both(&a)).await;

    proxy.offline();
    a.engine.update_account_prefs(&acc.id, None, Some("#111111".into()), None, None).unwrap();
    until("A's colour to reach the server", 10, || true).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    b.engine.update_account_prefs(&acc.id, None, Some("#222222".into()), None, None).unwrap();
    proxy.online();
    b.cloud.sync_now().await.unwrap();
    let later = |d: &Device| d.account(&acc.id).is_some_and(|x| x.color == "#222222");
    until("B's later colour to win on B", 10, || later(&b)).await;
    until("B's later colour to win on A", 10, || later(&a)).await;

    // ------------------------------------------------------------------
    // d. Removed on A while B, offline, edits it: the removal wins
    // ------------------------------------------------------------------
    assert!(b.engine.worker_running(&acc.id));
    proxy.offline();
    b.engine.update_account_prefs(&acc.id, Some("Renamed offline".into()), None, None, None).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    a.engine.remove_account(&acc.id).unwrap();
    assert!(a.secret(&acc.id).is_none());
    proxy.online();
    b.cloud.sync_now().await.unwrap();
    until("the account to go from B", 10, || b.account(&acc.id).is_none()).await;
    assert!(!b.engine.worker_running(&acc.id), "B's worker for it stopped");
    assert_eq!(b.secret(&acc.id), None, "B's credential for it is gone");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(a.account(&acc.id).is_none(), "and it stays gone on A");
    assert!(b.engine.signatures().unwrap().iter().all(|s| s.account_id.as_deref() != Some(acc.id.as_str())));
    let leftover = |d: &Device| d.engine.ui_prefs().sidebar_hidden.unwrap_or_default().iter().any(|e| e.contains(&acc.id));
    assert!(!leftover(&a) && !leftover(&b), "its hidden folders went with it");

    // ------------------------------------------------------------------
    // e. The admin turns IMAP off
    // ------------------------------------------------------------------
    cfg.providers.imap = false;
    server.set_org_config(&org.id, &cfg).await.unwrap();
    until("the org notice to reach B", 10, || b.engine.org().is_some_and(|o| !o.providers.imap)).await;
    until("the org notice to reach A", 10, || a.engine.org().is_some_and(|o| !o.providers.imap)).await;
    let refused = b.engine.add_account(greenmail_account(&format!("other{run}@remail.test"))).await.unwrap_err();
    assert!(refused.to_string().starts_with("cloud:provider_not_allowed:"), "{refused:#}");
    let providers = b.cloud.providers().await.unwrap();
    assert!(providers.iter().any(|p| p.kind == remail_core::proto::org::ProviderKind::Imap && !p.enabled));
    // A client that isn't Remail is refused by the server itself.
    let raw_token = raw_sign_in(&server, PASSWORD, &codes.backup_codes[0]).await;
    let http = reqwest::Client::new();
    let current: Value = http.get(format!("{}/v1/sections/accounts", server.url)).bearer_auth(&raw_token).send().await.unwrap().json().await.unwrap();
    let mut doc: SectionDoc = serde_json::from_value(current["doc"].clone()).unwrap();
    let at = Stamp::new(remail_core::store::now_ms(), "curl");
    let r = doc.record_mut("acc_sneaky");
    r.set(account::EMAIL, json!("sneaky@remail.test"), &at);
    r.set(account::PROVIDER, json!("imap"), &at);
    r.set(account::IMAP, json!({"host": "127.0.0.1", "port": 3143, "security": "none"}), &at);
    let put = http
        .put(format!("{}/v1/sections/accounts", server.url))
        .bearer_auth(&raw_token)
        .header("If-Match", format!("\"v{}\"", current["version"]))
        .json(&json!({ "doc": doc }))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status().as_u16(), 422);
    let body: Value = put.json().await.unwrap();
    assert_eq!(body["error"]["code"], "provider_not_allowed");

    // ------------------------------------------------------------------
    // f. Microsoft: a credential refresh on B races an edit on A
    // ------------------------------------------------------------------
    cfg.providers.microsoft = true;
    cfg.providers.microsoft_app.client_id = "client-123".into();
    cfg.providers.microsoft_app.tenant = "common".into();
    server.set_org_config(&org.id, &cfg).await.unwrap();
    until("A to get the Microsoft app id", 10, || a.engine.microsoft_client_id().as_deref() == Some("client-123")).await;
    until("B to get the Microsoft app id", 10, || b.engine.microsoft_client_id().as_deref() == Some("client-123")).await;

    let accepted = Arc::new(Mutex::new(vec!["at-1".to_string(), "at-2".to_string()]));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let imap = ServerConfig { host: "127.0.0.1".into(), port: fake_imap(accepted.clone(), seen.clone()).await, security: Security::None };
    let smtp = ServerConfig { host: "127.0.0.1".into(), port: fake_smtp(accepted.clone()).await, security: Security::None };
    // B's first refresh rotates the refresh token.
    idp.lock().unwrap().refresh.insert("rt-1".into(), ("at-2".into(), "rt-2".into()));
    let req = OAuthAccountRequest { imap: Some(imap), smtp: Some(smtp), ..Default::default() };
    let ms = a.engine.add_microsoft_account(browser(idp.clone(), "code-1"), req).await.unwrap().account;
    assert_eq!(a.grant(&ms.id)["refreshToken"], "rt-1");
    until("B to get the Microsoft account", 15, || b.account(&ms.id).is_some()).await;
    let accounts_v = server.section_meta(EMAIL, "accounts").await.unwrap().unwrap().0;
    a.engine.update_account_prefs(&ms.id, None, Some("#00aa00".into()), None, None).unwrap();
    until("B's refresh to rotate its refresh token", 15, || b.secret(&ms.id).is_some_and(|g| g.contains("rt-2"))).await;
    until("A to get the rotated refresh token", 15, || a.secret(&ms.id).is_some_and(|g| g.contains("rt-2"))).await;
    until("A's colour to reach B", 15, || b.account(&ms.id).is_some_and(|x| x.color == "#00aa00")).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(a.account(&ms.id).unwrap().color, "#00aa00", "the refresh didn't undo the edit");
    assert_eq!(b.grant(&ms.id)["refreshToken"], "rt-2");
    assert_eq!(a.grant(&ms.id), b.grant(&ms.id));
    let devices = a.cloud.devices().await.unwrap();
    let dev = |name: &str| devices.iter().find(|d| d.name == name).unwrap_or_else(|| panic!("{name} in {devices:?}")).id.clone();
    let (dev_a, dev_b) = (dev("Device A"), dev("Device B"));
    let (_, cred_by) = server.section_meta(EMAIL, &format!("cred:{}", ms.id)).await.unwrap().unwrap();
    assert_eq!(cred_by.as_deref(), Some(dev_b.as_str()), "the refresh was written by B, to cred: only");
    let (accounts_now, accounts_by) = server.section_meta(EMAIL, "accounts").await.unwrap().unwrap();
    assert_eq!(accounts_by.as_deref(), Some(dev_a.as_str()), "B never rewrote the accounts");
    assert!(accounts_now - accounts_v <= 2, "no write loop on accounts ({accounts_v} → {accounts_now})");
    let refreshes = idp.lock().unwrap().token_requests.iter().filter(|(g, s)| g == "refresh_token" && s.contains("IMAP")).count();
    assert_eq!(refreshes, 1, "one refresh, no loop");
    assert!(seen.lock().unwrap().contains(&"at-2".to_string()), "B's worker signed in with the refreshed token");
    // What the server holds (and hands any signed-in client) is sealed: no grant, no token.
    let cred: Value =
        http.get(format!("{}/v1/sections/cred:{}", server.url, ms.id)).bearer_auth(&raw_token).send().await.unwrap().json().await.unwrap();
    let cred = cred["doc"].to_string();
    assert!(cred.contains(r#""sealed""#) && !cred.contains("rt-2") && !cred.contains("refreshToken") && !cred.contains("client-123"), "{cred}");

    // ------------------------------------------------------------------
    // g. A signs B out
    // ------------------------------------------------------------------
    a.cloud.revoke_device(&dev_b).await.unwrap();
    b.wait_phase(Phase::SignedOut, 10).await;
    // (The phase changes just before the event goes out.)
    until("B to hear it was signed out by another device", 5, || b.events.has(r#""kind":"signedOut","reason":"revoked""#)).await;
    assert!(b.engine.accounts().unwrap().is_empty(), "B's synced setup is gone");
    assert!(b.engine.templates().unwrap().is_empty() && b.engine.signatures().unwrap().is_empty());
    assert_eq!(b.secret(&ms.id), None, "B's credentials are gone");
    assert_eq!(b.secret(SESSION_ID), None);
    assert!(!b.engine.gate_open() && !b.engine.worker_running(&ms.id), "B's mail stopped");
    assert!(!server.live_devices(EMAIL).await.unwrap().contains(&dev_b));
    // A is unaffected.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(a.cloud.status().phase, Phase::Ready);

    // ------------------------------------------------------------------
    // h. Changing the password keeps the sign-ins; a reset drops them
    // ------------------------------------------------------------------
    a.cloud.change_password(PASSWORD, CHANGED_PASSWORD).await.unwrap();
    assert_eq!(a.cloud.status().vault, VaultState::Ready);
    let c = device("Device C", &server.url).await;
    assert_eq!(c.cloud.login(EMAIL, PASSWORD).await.unwrap_err().code, "invalid_credentials");
    let challenge = match c.cloud.login(EMAIL, CHANGED_PASSWORD).await.unwrap() {
        LoginResult::TwoFactor { challenge, .. } => challenge,
        other => panic!("{other:?}"),
    };
    c.cloud.login_2fa(&challenge, &codes.backup_codes[2]).await.unwrap();
    c.wait_phase(Phase::Ready, 30).await;
    until("C to get the Microsoft sign-in, sealed before the change", 15, || c.secret(&ms.id).is_some()).await;
    assert_eq!(c.grant(&ms.id)["clientId"], "client-123");
    c.cloud.logout().await.unwrap();

    let before = a.setup();
    // (A fresh sign-in has no access token yet: its first refresh rotates again.)
    idp.lock().unwrap().refresh.insert("rt-2".into(), ("at-3".into(), "rt-3".into()));
    accepted.lock().unwrap().push("at-3".into());
    a.cloud.request_reset(EMAIL).await.unwrap();
    let reset = mailed_token(&server, EMAIL, "reset").await;
    a.cloud.reset_password(&reset, NEW_PASSWORD).await.unwrap();
    a.wait_phase(Phase::SignedOut, 10).await;
    assert!(a.engine.accounts().unwrap().is_empty() && a.secret(&ms.id).is_none(), "A was signed out and wiped");
    assert!(server.live_devices(EMAIL).await.unwrap().is_empty(), "every device was signed out");
    assert_eq!(a.cloud.login(EMAIL, CHANGED_PASSWORD).await.unwrap_err().code, "invalid_credentials");
    let challenge = match a.cloud.login(EMAIL, NEW_PASSWORD).await.unwrap() {
        LoginResult::TwoFactor { challenge, .. } => challenge,
        other => panic!("{other:?}"),
    };
    a.cloud.login_2fa(&challenge, &codes.backup_codes[1]).await.unwrap();
    a.wait_phase(Phase::Ready, 30).await;
    // The new password has a new vault key: what the old one sealed is gone,
    // so the mailbox needs signing in again. Everything else is intact.
    let without_secrets = |v: Value| {
        let mut v = v;
        for acc in v["accounts"].as_array_mut().unwrap() {
            acc.as_array_mut().unwrap().pop();
        }
        v
    };
    assert_eq!(without_secrets(a.setup()), without_secrets(before), "the setup is intact after the reset");
    assert_eq!(a.account(&ms.id).unwrap().color, "#00aa00");
    assert_eq!(a.secret(&ms.id), None, "the sealed sign-in went with the old vault key");
    until("the mailbox to ask for signing in again", 15, || a.account(&ms.id).is_some_and(|x| x.status == AccountStatus::AuthError)).await;

    // ------------------------------------------------------------------
    // i. A signs itself out: the server's notice about that same sign-out
    //    doesn't turn it into "signed out by another device"
    // ------------------------------------------------------------------
    a.events.0.lock().unwrap().clear();
    a.cloud.logout().await.unwrap();
    assert_eq!(a.cloud.status().phase, Phase::SignedOut);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let seen_events = a.events.0.lock().unwrap().clone();
    assert!(a.events.has(r#""kind":"signedOut","reason":"user""#), "{seen_events:?}");
    assert!(!a.events.has(r#""reason":"revoked""#), "{seen_events:?}");

    // ------------------------------------------------------------------
    // j. An account from before end-to-end encryption, and a locked device
    // ------------------------------------------------------------------
    // An old app signs up with the password itself (no vault).
    let old_email = "before@remail.test";
    let http = reqwest::Client::new();
    let r = http
        .post(format!("{}/v1/auth/signup", server.url))
        .json(&json!({"name": "Old", "email": old_email, "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 202);
    let t = mailed_token(&server, old_email, "verify").await;
    a.cloud.verify_email(&t).await.unwrap();
    let pre: Value = http.post(format!("{}/v1/auth/prelogin", server.url)).json(&json!({"email": old_email})).send().await.unwrap().json().await.unwrap();
    assert_eq!(pre["kdf"], Value::Null);
    // The new app converts it while signing in.
    let d = device("Device D", &server.url).await;
    d.cloud.login(old_email, PASSWORD).await.unwrap();
    d.wait_phase(Phase::Ready, 20).await;
    assert_eq!(d.cloud.status().vault, VaultState::Ready);
    let pre: Value = http.post(format!("{}/v1/auth/prelogin", server.url)).json(&json!({"email": old_email})).send().await.unwrap().json().await.unwrap();
    assert_eq!(pre["kdf"]["alg"], "argon2id", "converted");
    let acc = d.engine.add_account(greenmail_account(&format!("d{run}@remail.test"))).await;
    // (IMAP is off for this organisation since e.; the public org of a fresh user may allow it.)
    let synced_cred = acc.is_ok();
    // A device without the vault key (here: it lost it; the app restarts) is
    // locked until the password unlocks it.
    d.secrets.delete("camel-vault-key").unwrap();
    d.cloud.start().await.unwrap();
    assert_eq!(d.cloud.status().vault, VaultState::Locked);
    assert_eq!(d.cloud.unlock("not the password").await.unwrap_err().code, "invalid_credentials");
    d.cloud.unlock(PASSWORD).await.unwrap();
    assert_eq!(d.cloud.status().vault, VaultState::Ready);
    if synced_cred {
        until("D's mailbox password to stay", 10, || d.secret(&acc.as_ref().unwrap().id).is_some()).await;
    }

    server.finish().await;
}
