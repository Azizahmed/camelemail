//! A Microsoft mailbox that refuses IMAP ("User is authenticated but not
//! connected") read through Microsoft Graph instead, end to end against a
//! mock identity platform and a mock Graph: the fallback when the account is
//! added, change-tracked sync, flags and moves both ways, deletions,
//! sending (Bcc kept), server search, and a restart.

mod common;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use common::*;
use remail_core::engine::{ThreadAction, ThreadRef};
use remail_core::secrets::MemoryStore;
use remail_core::store::Store;
use remail_core::*;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

struct Msg {
    folder: String,
    read: bool,
    flagged: bool,
    received: String,
    mime: String,
    ver: u64,
}

#[derive(Default)]
struct Mail {
    base: String,
    ver: u64,
    msgs: BTreeMap<String, Msg>,
    /// (folder, id, version) of every message that left a folder.
    removed: Vec<(String, String, u64)>,
    /// Messages handed to sendMail, decoded.
    sent: Vec<String>,
    next: u64,
}
type M = Arc<Mutex<Mail>>;

const FOLDERS: [(&str, &str, &str); 7] = [
    ("inbox", "f-inbox", "Inbox"),
    ("drafts", "f-drafts", "Drafts"),
    ("sentitems", "f-sent", "Sent Items"),
    ("deleteditems", "f-deleted", "Deleted Items"),
    ("junkemail", "f-junk", "Junk Email"),
    ("archive", "f-archive", "Archive"),
    ("outbox", "f-outbox", "Outbox"),
];

fn mime(id: &str, subject: &str, from: &str) -> String {
    format!("From: {from}\r\nTo: me@outlook.com\r\nSubject: {subject}\r\nMessage-ID: <{id}@x.test>\r\nDate: Mon, 19 Oct 2026 09:00:00 +0000\r\n\r\nHello about {subject}\r\n")
}

impl Mail {
    fn put(&mut self, id: &str, folder: &str, subject: &str, read: bool, day: u32) {
        self.ver += 1;
        let m = Msg { folder: folder.into(), read, flagged: false, received: format!("2026-10-{day:02}T09:00:00Z"), mime: mime(id, subject, "Sara <sara@x.test>"), ver: self.ver };
        self.msgs.insert(id.into(), m);
    }

    fn json(&self, id: &str) -> Value {
        let m = &self.msgs[id];
        let mid = m.mime.split("Message-ID: ").nth(1).and_then(|r| r.split("\r\n").next()).unwrap_or_default();
        json!({
            "id": id, "isRead": m.read, "isDraft": m.folder == "f-drafts", "receivedDateTime": m.received,
            "internetMessageId": mid, "flag": { "flagStatus": if m.flagged { "flagged" } else { "notFlagged" } },
        })
    }

    fn take(&mut self, id: &str) -> Option<Msg> {
        let m = self.msgs.remove(id)?;
        self.ver += 1;
        self.removed.push((m.folder.clone(), id.into(), self.ver));
        Some(m)
    }
}

fn authorized(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer gat-rt-1")
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": {"code": "ErrorItemNotFound", "message": "not found"}}))).into_response()
}

fn folder_id(name: &str) -> Option<&'static str> {
    FOLDERS.iter().find(|(w, id, _)| *w == name || *id == name).map(|(_, id, _)| *id)
}

async fn folder(Path(f): Path<String>, h: HeaderMap) -> Response {
    if !authorized(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match folder_id(&f) {
        Some(id) => Json(json!({ "id": id })).into_response(),
        None => not_found(),
    }
}

async fn folders(h: HeaderMap) -> Response {
    if !authorized(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let value: Vec<Value> = FOLDERS.iter().map(|(_, id, name)| json!({ "id": id, "displayName": name, "childFolderCount": 0 })).collect();
    Json(json!({ "value": value })).into_response()
}

async fn delta(State(s): State<M>, Path(f): Path<String>, RawQuery(q): RawQuery, h: HeaderMap) -> Response {
    if !authorized(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let g = s.lock().unwrap();
    let since: u64 = q.as_deref().and_then(|q| q.split("deltatoken=").nth(1)).and_then(|t| t.parse().ok()).unwrap_or(0);
    let mut value: Vec<Value> = g.msgs.iter().filter(|(_, m)| m.folder == f && m.ver > since).map(|(id, _)| g.json(id)).collect();
    if since > 0 {
        for (folder, id, ver) in &g.removed {
            if *folder == f && *ver > since {
                value.push(json!({ "id": id, "@removed": { "reason": "deleted" } }));
            }
        }
    }
    Json(json!({ "value": value, "@odata.deltaLink": format!("{}/me/mailFolders/{f}/messages/delta?$deltatoken={}", g.base, g.ver) })).into_response()
}

async fn message_get(State(s): State<M>, Path((id, what)): Path<(String, String)>, h: HeaderMap) -> Response {
    if !authorized(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let g = s.lock().unwrap();
    match (g.msgs.get(&id), what.as_str()) {
        (Some(m), "$value") => m.mime.clone().into_response(),
        _ => not_found(),
    }
}

async fn message_move(State(s): State<M>, Path((id, what)): Path<(String, String)>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if !authorized(&h) || what != "move" {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut g = s.lock().unwrap();
    let Some(mut m) = g.take(&id) else { return not_found() };
    m.folder = folder_id(body["destinationId"].as_str().unwrap()).unwrap().into();
    g.ver += 1;
    m.ver = g.ver;
    g.msgs.insert(id.clone(), m);
    Json(g.json(&id)).into_response()
}

async fn message_patch(State(s): State<M>, Path(id): Path<String>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if !authorized(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut g = s.lock().unwrap();
    g.ver += 1;
    let ver = g.ver;
    let Some(m) = g.msgs.get_mut(&id) else { return not_found() };
    if let Some(r) = body["isRead"].as_bool() {
        m.read = r;
    }
    if let Some(f) = body["flag"]["flagStatus"].as_str() {
        m.flagged = f == "flagged";
    }
    m.ver = ver;
    Json(g.json(&id)).into_response()
}

async fn message_delete(State(s): State<M>, Path(id): Path<String>, h: HeaderMap) -> Response {
    if !authorized(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match s.lock().unwrap().take(&id) {
        Some(_) => StatusCode::NO_CONTENT.into_response(),
        None => not_found(),
    }
}

async fn send_mail(State(s): State<M>, h: HeaderMap, body: String) -> Response {
    if !authorized(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    assert_eq!(h.get("content-type").and_then(|v| v.to_str().ok()), Some("text/plain"));
    let raw = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(body.trim()).unwrap()).unwrap();
    let mut g = s.lock().unwrap();
    g.next += 1;
    let id = format!("sent-{}", g.next);
    g.ver += 1;
    let ver = g.ver;
    g.msgs.insert(id, Msg { folder: "f-sent".into(), read: true, flagged: false, received: "2026-10-26T09:00:00Z".into(), mime: raw.replace("Bcc: carol@x.test\r\n", ""), ver });
    g.sent.push(raw);
    StatusCode::ACCEPTED.into_response()
}

async fn search(State(s): State<M>, RawQuery(q): RawQuery, h: HeaderMap) -> Response {
    if !authorized(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let q: HashMap<String, String> = reqwest::Url::parse(&format!("http://x/?{}", q.unwrap_or_default())).unwrap().query_pairs().into_owned().collect();
    let word = q["$search"].trim_matches('"').to_string();
    let g = s.lock().unwrap();
    let value: Vec<Value> =
        g.msgs.iter().filter(|(_, m)| m.mime.contains(&word)).map(|(id, m)| json!({ "id": id, "parentFolderId": m.folder })).collect();
    Json(json!({ "value": value })).into_response()
}

/// IMAP that takes the sign-in but won't open the mailbox, like Exchange
/// Online does for these mailboxes.
async fn refusing_imap() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (sock, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut lines = BufReader::new(r).lines();
                w.write_all(b"* OK [CAPABILITY IMAP4rev1 AUTH=XOAUTH2] fake ready\r\n").await.unwrap();
                while let Ok(Some(line)) = lines.next_line().await {
                    let tag = line.split(' ').next().unwrap_or("*").to_string();
                    let reply = if line.to_uppercase().contains("AUTHENTICATE") {
                        w.write_all(b"+ \r\n").await.unwrap();
                        let _ = lines.next_line().await;
                        format!("{tag} NO User is authenticated but not connected.\r\n")
                    } else {
                        format!("{tag} NO not in this fake\r\n")
                    };
                    if w.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

fn list(e: &Engine, account: &str, label: &str) -> Vec<ThreadSummary> {
    e.list_threads(&ThreadQuery {
        account_id: Some(account.into()),
        label: label.into(),
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

fn subjects(e: &Engine, account: &str, label: &str) -> Vec<String> {
    let mut s: Vec<String> = list(e, account, label).into_iter().map(|t| t.subject).collect();
    s.sort();
    s
}

fn thread(e: &Engine, account: &str, label: &str, subject: &str) -> ThreadSummary {
    list(e, account, label).into_iter().find(|t| t.subject == subject).unwrap_or_else(|| panic!("no '{subject}' in {label}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn mailbox_without_imap_goes_through_graph() {
    let idp: Shared = Arc::new(Mutex::new(Idp::default()));
    idp.lock().unwrap().calendar_ok.insert("rt-1".into());
    let mail: M = Arc::new(Mutex::new(Mail::default()));
    let app = Router::new()
        .route("/common/oauth2/v2.0/token", post(token))
        .with_state(idp.clone())
        .merge(
            Router::new()
                .route("/v1.0/me/mailFolders", get(folders))
                .route("/v1.0/me/mailFolders/{f}", get(folder))
                .route("/v1.0/me/mailFolders/{f}/messages/delta", get(delta))
                .route("/v1.0/me/messages", get(search))
                .route("/v1.0/me/messages/{id}", axum::routing::patch(message_patch).delete(message_delete))
                .route("/v1.0/me/messages/{id}/{what}", get(message_get).post(message_move))
                .route("/v1.0/me/sendMail", post(send_mail))
                .with_state(mail.clone()),
        );
    let port = serve(app).await;
    std::env::set_var("REMAIL_MS_AUTHORITY", format!("http://127.0.0.1:{port}/common"));
    std::env::set_var("REMAIL_MS_GRAPH", format!("http://127.0.0.1:{port}/v1.0"));
    {
        let mut g = mail.lock().unwrap();
        g.base = format!("http://127.0.0.1:{port}/v1.0");
        g.put("m1", "f-inbox", "Lunch plans", false, 20);
        g.put("m2", "f-inbox", "Invoice 42", true, 21);
        // More than the first sync takes from a folder other than the inbox.
        for day in 1..=22 {
            let subject = if day == 1 { "Quarterly zebra report".to_string() } else { format!("Old note {day}") };
            g.put(&format!("a{day}"), "f-archive", &subject, true, day);
        }
    }

    let imap = ServerConfig { host: "127.0.0.1".into(), port: refusing_imap().await, security: Security::None };
    let smtp = ServerConfig { host: "127.0.0.1".into(), port: fake_smtp(Arc::new(Mutex::new(Vec::new()))).await, security: Security::None };
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemoryStore::default());
    let open = || {
        let e = Engine::with_store(Store::open(&dir.path().join("remail.db")).unwrap(), secrets.clone(), Arc::new(Quiet)).unwrap();
        e.set_org(microsoft_org());
        e
    };
    let engine = open();

    // Adding it: IMAP refuses the mailbox, Graph opens it, and the account reads through Graph.
    let added = engine
        .add_microsoft_account(browser(idp.clone(), "code-1"), OAuthAccountRequest { imap: Some(imap.clone()), smtp: Some(smtp.clone()), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(added.account.imap.host, remail_core::graphmail::HOST);
    assert!(added.warning.is_none(), "{:?}", added.warning);
    assert!(idp.lock().unwrap().authorize_scopes[0].contains("Mail.ReadWrite"), "the sign-in asks for Graph mail");
    let id = added.account.id.clone();

    engine.sync_and_wait(&id).await.unwrap();
    assert_eq!(subjects(&engine, &id, labels::INBOX), ["Invoice 42", "Lunch plans"]);
    assert_eq!(list(&engine, &id, labels::ARCHIVE).len(), 20, "the newest 20 of a folder");
    assert_eq!(thread(&engine, &id, labels::INBOX, "Lunch plans").unread_count, 1);
    let full = engine.message_with_images(&thread(&engine, &id, labels::INBOX, "Lunch plans").latest_message_id).await.unwrap();
    assert!(format!("{full:?}").contains("Hello about Lunch plans"));

    // Read, starred and archived here: Graph hears about each.
    let lunch = ThreadRef { account_id: id.clone(), thread_id: thread(&engine, &id, labels::INBOX, "Lunch plans").thread_id };
    engine.modify_threads(std::slice::from_ref(&lunch), &ThreadAction::MarkRead).unwrap();
    engine.modify_threads(std::slice::from_ref(&lunch), &ThreadAction::Star).unwrap();
    engine.sync_and_wait(&id).await.unwrap();
    {
        let g = mail.lock().unwrap();
        assert!(g.msgs["m1"].read && g.msgs["m1"].flagged);
    }
    engine.modify_threads(std::slice::from_ref(&lunch), &ThreadAction::Archive).unwrap();
    engine.sync_and_wait(&id).await.unwrap();
    assert_eq!(mail.lock().unwrap().msgs["m1"].folder, "f-archive");
    assert_eq!(subjects(&engine, &id, labels::INBOX), ["Invoice 42"]);
    assert!(thread(&engine, &id, labels::ARCHIVE, "Lunch plans").is_starred);

    // Changes made in Outlook: one read again as unread, a new arrival, a deletion.
    {
        let mut g = mail.lock().unwrap();
        g.ver += 1;
        let ver = g.ver;
        let m2 = g.msgs.get_mut("m2").unwrap();
        m2.read = false;
        m2.ver = ver;
        g.put("m3", "f-inbox", "Weekend trip", false, 25);
        g.take("a22");
    }
    engine.sync_and_wait(&id).await.unwrap();
    assert_eq!(subjects(&engine, &id, labels::INBOX), ["Invoice 42", "Weekend trip"]);
    assert_eq!(thread(&engine, &id, labels::INBOX, "Invoice 42").unread_count, 1);
    assert!(!subjects(&engine, &id, labels::ARCHIVE).contains(&"Old note 22".to_string()));

    // Searching at Microsoft finds mail older than what was synced.
    assert!(!subjects(&engine, &id, labels::ARCHIVE).contains(&"Quarterly zebra report".to_string()));
    assert_eq!(engine.server_search(Some(&id), "zebra").await.unwrap(), 1);
    assert!(subjects(&engine, &id, labels::ARCHIVE).contains(&"Quarterly zebra report".to_string()));

    // Sending goes through Graph, Bcc included; Microsoft files the copy in Sent Items.
    let mut d = engine.new_draft(Some(&id)).unwrap();
    d.to = vec![EmailAddress::new("", "bob@x.test")];
    d.bcc = vec![EmailAddress::new("", "carol@x.test")];
    d.subject = "Through Graph".into();
    d.html_body = "<p>hi</p>".into();
    engine.send(d, None).unwrap();
    engine.flush_outbox(Duration::from_secs(20)).await;
    {
        let g = mail.lock().unwrap();
        assert_eq!(g.sent.len(), 1);
        assert!(g.sent[0].starts_with("Bcc: carol@x.test\r\n") && g.sent[0].contains("Subject: Through Graph"), "{}", g.sent[0]);
    }
    engine.sync_and_wait(&id).await.unwrap();
    assert_eq!(subjects(&engine, &id, labels::SENT), ["Through Graph"], "one copy: none appended by us");

    // Deleting from Trash for good.
    let invoice = ThreadRef { account_id: id.clone(), thread_id: thread(&engine, &id, labels::INBOX, "Invoice 42").thread_id };
    engine.modify_threads(std::slice::from_ref(&invoice), &ThreadAction::Trash).unwrap();
    engine.sync_and_wait(&id).await.unwrap();
    assert_eq!(mail.lock().unwrap().msgs["m2"].folder, "f-deleted");
    engine.modify_threads(std::slice::from_ref(&invoice), &ThreadAction::DeleteForever).unwrap();
    engine.sync_and_wait(&id).await.unwrap();
    assert!(!mail.lock().unwrap().msgs.contains_key("m2"));

    // An account that read over IMAP until Microsoft started refusing it.
    let older = NewAccount {
        email: "old@outlook.com".into(),
        display_name: "Old".into(),
        username: "old@outlook.com".into(),
        password: String::new(),
        imap: imap.clone(),
        smtp: smtp.clone(),
        allow_invalid_certs: false,
    };
    let older = engine.store().insert_account(&older, false, "microsoft").unwrap().id;
    let grant = json!({ "provider": "microsoft", "clientId": "client-123", "refreshToken": "rt-1", "subject": null });
    remail_core::secrets::SecretStore::set(secrets.as_ref(), &older, &grant.to_string()).unwrap();
    idp.lock().unwrap().refresh.insert("rt-1".into(), ("at-imap".into(), "rt-1".into()));

    // After a restart it carries on where it was: nothing duplicated or lost.
    drop(engine);
    let engine = open();
    engine.sync_and_wait(&id).await.unwrap();
    assert_eq!(subjects(&engine, &id, labels::INBOX), ["Weekend trip"]);
    assert_eq!(subjects(&engine, &id, labels::SENT), ["Through Graph"]);
    assert_eq!(list(&engine, &id, labels::ARCHIVE).len(), 21);

    // The refused IMAP account moves to Graph by itself on its next sync.
    engine.sync_and_wait(&older).await.unwrap();
    let older = engine.accounts().unwrap().into_iter().find(|a| a.id == older).unwrap();
    assert_eq!(older.imap.host, remail_core::graphmail::HOST);
    assert_eq!(subjects(&engine, &older.id, labels::INBOX), ["Weekend trip"]);
    assert!(engine.accounts().unwrap().iter().all(|a| a.status == AccountStatus::Active));
}
