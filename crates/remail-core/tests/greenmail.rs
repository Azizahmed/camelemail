//! End-to-end test against a real IMAP/SMTP server.
//!
//! Needs a GreenMail container (see `scripts/test-server.sh`) and
//! `REMAIL_IT=1`; skipped otherwise.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use remail_core::engine::{AppSettings, ThreadAction, ThreadRef};
use remail_core::secrets::MemoryStore;
use remail_core::store::Store;
use remail_core::*;

#[derive(Default)]
struct Collector(Mutex<Vec<EngineEvent>>);

impl EventSink for Collector {
    fn emit(&self, e: EngineEvent) {
        self.0.lock().unwrap().push(e);
    }
}

impl Collector {
    fn any(&self, f: impl Fn(&EngineEvent) -> bool) -> bool {
        self.0.lock().unwrap().iter().any(f)
    }
}

fn enabled() -> bool {
    std::env::var("REMAIL_IT").is_ok()
}

/// Per-run suffix so leftovers from earlier runs on the same server don't
/// leak in (GreenMail creates mailboxes on first login).
fn run_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()))
}

fn addr(user: &str) -> String {
    format!("{user}{}@remail.test", run_id())
}

fn account(user: &str, pw: &str, tls: bool) -> NewAccount {
    let host = std::env::var("REMAIL_IT_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let (imap_port, smtp_port, sec) = if tls { (3993, 3465, Security::Tls) } else { (3143, 3025, Security::None) };
    NewAccount {
        email: addr(user),
        display_name: format!("{}{}", user[..1].to_uppercase(), &user[1..]),
        username: addr(user),
        password: pw.into(),
        imap: ServerConfig { host: host.clone(), port: imap_port, security: sec },
        smtp: ServerConfig { host, port: smtp_port, security: sec },
        allow_invalid_certs: true,
    }
}

async fn wait_until<F: Fn() -> bool>(what: &str, secs: u64, f: F) {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn inbox(e: &Engine, account: &str) -> Vec<ThreadSummary> {
    list(e, account, labels::INBOX)
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

fn draft_to(e: &Engine, from: &Account, to: &str, subject: &str, body: &str) -> Draft {
    let mut d = e.new_draft(Some(&from.id)).unwrap();
    d.to = vec![EmailAddress::new("", to)];
    d.subject = subject.into();
    d.html_body = format!("<p>{body}</p>");
    d
}

async fn send_and_wait(e: &Engine, sink: &Collector, d: Draft) {
    let subject = d.subject.clone();
    e.send(d, None).unwrap();
    wait_until(&format!("'{subject}' to be sent"), 30, || {
        sink.any(|ev| matches!(ev, EngineEvent::MessageSent { subject: s, .. } if *s == subject))
    })
    .await;
    assert!(
        !sink.any(|ev| matches!(ev, EngineEvent::SendFailed { .. })),
        "send failed: {:?}",
        sink.0.lock().unwrap().iter().filter(|e| matches!(e, EngineEvent::SendFailed { .. })).collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_to_end() {
    if !enabled() {
        eprintln!("skipping: set REMAIL_IT=1 with a GreenMail server running");
        return;
    }
    let _ = tracing_subscriber_init();
    let sink = Arc::new(Collector::default());
    let engine = Engine::with_store(Store::open_in_memory().unwrap(), Arc::new(MemoryStore::default()), sink.clone()).unwrap();
    engine.set_settings(&AppSettings { undo_send_seconds: 0, ..Default::default() }).unwrap();

    // --- Accounts: Alice over TLS (self-signed), Bob over plain ports.
    // GreenMail accepts any password, so a rejected login can only be
    // checked against servers that enforce auth.
    // (A separate user: GreenMail creates accounts on first login, which
    // would otherwise give alice this password.)
    let bad = NewAccount { password: "wrong".into(), ..account("probe", "probepw", true) };
    match engine.add_account(bad).await {
        Err(err) => assert!(remail_core::engine::friendly_error(&err).contains("password"), "{err:#}"),
        Ok(a) => engine.remove_account(&a.id).unwrap(),
    }

    let alice = engine.add_account(account("alice", "alicepw", true)).await.unwrap();
    let bob = engine.add_account(account("bob", "bobpw", false)).await.unwrap();
    engine.sync_and_wait(&alice.id).await.unwrap();
    engine.sync_and_wait(&bob.id).await.unwrap();
    assert!(engine.labels(&alice.id).unwrap().iter().any(|l| l.id == labels::INBOX));

    // --- Bob → Alice, with an attachment and an unsubscribe header later.
    let mut d = draft_to(&engine, &bob, &addr("alice"), "Lunch on Friday?", "Are you free <b>Friday</b>?");
    d.attachments.push(OutgoingAttachment { filename: "menu.txt".into(), mime_type: "text/plain".into(), data_base64: "cGl6emE=".into() });
    send_and_wait(&engine, &sink, d).await;

    // Bob's copy went to Sent (APPEND, since GreenMail is not Gmail).
    engine.sync_and_wait(&bob.id).await.unwrap();
    let bob_sent = list(&engine, &bob.id, labels::SENT);
    assert_eq!(bob_sent.len(), 1, "sent copy");
    assert_eq!(bob_sent[0].unread_count, 0);

    engine.sync_and_wait(&alice.id).await.unwrap();
    let threads = inbox(&engine, &alice.id);
    assert_eq!(threads.len(), 1);
    let t = &threads[0];
    assert_eq!(t.subject, "Lunch on Friday?");
    assert_eq!(t.unread_count, 1);
    assert!(t.has_attachments);
    assert_eq!(t.participants, vec!["Bob"]);

    let detail = engine.get_thread(&alice.id, &t.thread_id).await.unwrap();
    assert_eq!(detail.messages.len(), 1);
    let m = &detail.messages[0];
    assert!(m.html.as_deref().unwrap().contains("<b>Friday</b>"));
    assert_eq!(m.attachments.len(), 1);
    let att = engine.attachment(&m.meta.id, m.attachments[0].index).await.unwrap();
    assert_eq!(att.data_base64, "cGl6emE=");

    // --- Mark read is reflected on the server (survives a re-sync).
    let tref = ThreadRef { account_id: alice.id.clone(), thread_id: t.thread_id.clone() };
    engine.modify_threads(std::slice::from_ref(&tref), &ThreadAction::MarkRead).unwrap();
    engine.sync_and_wait(&alice.id).await.unwrap();
    assert_eq!(inbox(&engine, &alice.id)[0].unread_count, 0, "read state survives sync");
    engine.modify_threads(std::slice::from_ref(&tref), &ThreadAction::Star).unwrap();
    engine.sync_and_wait(&alice.id).await.unwrap();
    assert!(inbox(&engine, &alice.id)[0].is_starred, "star survives sync");
    assert_eq!(list(&engine, &alice.id, labels::STARRED).len(), 1);

    // --- Alice replies; Bob sees one two-message conversation.
    let reply = engine.reply_draft(&m.meta.id, ReplyKind::Reply).await.unwrap();
    assert_eq!(reply.subject, "Re: Lunch on Friday?");
    assert_eq!(reply.to[0].email, addr("bob"));
    assert!(reply.quoted_html.contains("wrote:"));
    let mut reply = reply;
    reply.html_body = "<p>Yes, noon works.</p>".into();
    send_and_wait(&engine, &sink, reply).await;

    engine.sync_and_wait(&bob.id).await.unwrap();
    let bob_inbox = inbox(&engine, &bob.id);
    assert_eq!(bob_inbox.len(), 1);
    assert_eq!(bob_inbox[0].subject, "Re: Lunch on Friday?");
    assert_eq!(bob_inbox[0].message_count, 2, "reply threads with the original");
    let bob_thread = engine.get_thread(&bob.id, &bob_inbox[0].thread_id).await.unwrap();
    assert_eq!(bob_thread.messages.len(), 2);
    assert!(bob_thread.messages[1].html.as_deref().unwrap().contains("noon works"));
    assert!(bob_thread.messages[1].html.as_deref().unwrap().contains("gmail_quote"));

    // Alice's thread now also holds her sent reply.
    engine.sync_and_wait(&alice.id).await.unwrap();
    let t = &inbox(&engine, &alice.id)[0];
    assert_eq!(t.message_count, 2);
    assert_eq!(t.participants, vec!["Bob", "me"]);

    // --- Archive: moves out of INBOX on the server, into an Archive folder.
    engine.modify_threads(std::slice::from_ref(&tref), &ThreadAction::Archive).unwrap();
    assert!(inbox(&engine, &alice.id).is_empty(), "optimistic archive");
    engine.sync_and_wait(&alice.id).await.unwrap();
    assert!(inbox(&engine, &alice.id).is_empty(), "archive survives sync");
    let archived = list(&engine, &alice.id, labels::ARCHIVE);
    assert_eq!(archived.len(), 1, "thread in Archive");
    // Stable ids: the archived message can still be opened.
    let after = engine.get_thread(&alice.id, &tref.thread_id).await.unwrap();
    assert!(after.messages.iter().any(|m| m.meta.folder == labels::ARCHIVE));

    // And back to the inbox.
    engine.modify_threads(std::slice::from_ref(&tref), &ThreadAction::MoveToInbox { from: Some(labels::ARCHIVE.into()) }).unwrap();
    engine.sync_and_wait(&alice.id).await.unwrap();
    assert_eq!(inbox(&engine, &alice.id).len(), 1, "unarchive survives sync");

    // --- Snooze hides the thread and wakes it.
    let until = remail_core::store::now_ms() + 2_000;
    engine.snooze(std::slice::from_ref(&tref), until).unwrap();
    assert!(inbox(&engine, &alice.id).is_empty(), "snoozed thread hidden");
    assert_eq!(list(&engine, &alice.id, labels::SNOOZED).len(), 1);
    wait_until("snooze wake", 10, || sink.any(|e| matches!(e, EngineEvent::SnoozeWoke { .. }))).await;
    assert_eq!(inbox(&engine, &alice.id).len(), 1, "woken thread is back");
    assert!(list(&engine, &alice.id, labels::SNOOZED).is_empty());

    // --- Undo send: nothing is delivered.
    engine.set_settings(&AppSettings { undo_send_seconds: 5, ..Default::default() }).unwrap();
    let out = engine.send(draft_to(&engine, &alice, &addr("bob"), "Oops", "not this"), None).unwrap();
    let restored = engine.undo_send(&out.id).unwrap().expect("undo within window");
    assert_eq!(restored.subject, "Oops");
    assert!(engine.outbox().unwrap().is_empty());

    // --- Quitting inside the undo window still delivers (flush_outbox).
    engine.set_settings(&AppSettings { undo_send_seconds: 30, ..Default::default() }).unwrap();
    engine.send(draft_to(&engine, &alice, &addr("bob"), "Sent on quit", "flushed"), None).unwrap();
    engine.flush_outbox(Duration::from_secs(20)).await;
    assert!(sink.any(|e| matches!(e, EngineEvent::MessageSent { subject, .. } if subject == "Sent on quit")), "flushed before quit");
    assert!(engine.outbox().unwrap().is_empty());

    // --- Send later: queued, rescheduled, then sent now.
    engine.set_settings(&AppSettings { undo_send_seconds: 0, ..Default::default() }).unwrap();
    let later = remail_core::store::now_ms() + 3_600_000;
    let item = engine.send(draft_to(&engine, &alice, &addr("bob"), "Scheduled hello", "later"), Some(later)).unwrap();
    assert!(engine.send(draft_to(&engine, &alice, &addr("bob"), "too soon", "x"), Some(remail_core::store::now_ms() + 1000)).is_err());
    assert_eq!(engine.outbox().unwrap().len(), 1);
    engine.reschedule(&item.id, later + 60_000).unwrap();
    engine.send_now(&item.id).unwrap();
    wait_until("scheduled send", 30, || sink.any(|e| matches!(e, EngineEvent::MessageSent { subject, .. } if subject == "Scheduled hello"))).await;

    // --- Newsletter with one-click/mailto unsubscribe.
    for i in 0..2 {
        let raw = format!(
            "From: News <news@remail.test>\r\nTo: {alice}\r\nSubject: Weekly digest {i}\r\n\
             Message-ID: <digest{i}.{run}@remail.test>\r\nList-Unsubscribe: <mailto:{bob}?subject=unsubscribe>\r\n\
             Content-Type: text/plain\r\n\r\nNews number {i}\r\n",
            alice = addr("alice"),
            bob = addr("bob"),
            run = run_id()
        );
        send_raw_smtp("news@remail.test", &addr("alice"), &raw).await;
    }
    engine.sync_and_wait(&alice.id).await.unwrap();
    let senders = engine.newsletter_senders(Some(&alice.id)).unwrap();
    assert_eq!(senders.len(), 1);
    assert_eq!(senders[0].message_count, 2);
    let res = engine.unsubscribe(&senders[0].latest_message_id).await.unwrap();
    assert_eq!(res.method, "email");
    wait_until("unsubscribe email", 30, || sink.any(|e| matches!(e, EngineEvent::MessageSent { subject, .. } if subject == "unsubscribe"))).await;

    // --- Local search.
    let found = engine
        .list_threads(&ThreadQuery { account_id: Some(alice.id.clone()), label: String::new(), unread_only: false, search: Some("digest from:news".into()), limit: 50, before: None, before_account_id: None, before_thread_id: None, category: None })
        .unwrap();
    assert_eq!(found.len(), 2, "two digests (different threads)");
    let found = engine
        .list_threads(&ThreadQuery { account_id: None, label: String::new(), unread_only: false, search: Some("noon".into()), limit: 50, before: None, before_account_id: None, before_thread_id: None, category: None })
        .unwrap();
    assert!(found.iter().any(|t| t.subject.contains("Lunch")), "body text is indexed");
    let n = engine.server_search(Some(&alice.id), "digest").await.unwrap();
    assert_eq!(n, 0, "already cached locally");

    // --- Drafts are stored on the server.
    let mut draft = draft_to(&engine, &alice, &addr("bob"), "Draft idea", "half-written");
    draft = engine.save_draft(draft, true).await.unwrap();
    engine.sync_and_wait(&alice.id).await.unwrap();
    assert_eq!(list(&engine, &alice.id, labels::DRAFTS).len(), 1, "draft on server");
    draft.html_body = "<p>rewritten</p>".into();
    engine.save_draft(draft.clone(), true).await.unwrap();
    engine.sync_and_wait(&alice.id).await.unwrap();
    assert_eq!(list(&engine, &alice.id, labels::DRAFTS).len(), 1, "old server copy replaced");
    engine.delete_draft(&draft.id).unwrap();
    engine.sync_and_wait(&alice.id).await.unwrap();
    assert!(list(&engine, &alice.id, labels::DRAFTS).is_empty(), "draft deleted on server");

    // --- Trash, delete forever.
    let digests: Vec<ThreadRef> = inbox(&engine, &alice.id)
        .into_iter()
        .filter(|t| t.subject.starts_with("Weekly digest"))
        .map(|t| ThreadRef { account_id: t.account_id, thread_id: t.thread_id })
        .collect();
    assert_eq!(digests.len(), 2);
    engine.modify_threads(&digests, &ThreadAction::Trash).unwrap();
    engine.sync_and_wait(&alice.id).await.unwrap();
    assert_eq!(list(&engine, &alice.id, labels::TRASH).len(), 2);
    engine.empty_folder(&alice.id, labels::TRASH).unwrap();
    engine.sync_and_wait(&alice.id).await.unwrap();
    assert!(list(&engine, &alice.id, labels::TRASH).is_empty(), "trash emptied on server");

    // --- Unified inbox covers both accounts.
    let unified = engine
        .list_threads(&ThreadQuery { account_id: None, label: labels::INBOX.into(), unread_only: false, search: None, limit: 100, before: None, before_account_id: None, before_thread_id: None, category: None })
        .unwrap();
    assert!(unified.iter().any(|t| t.account_id == alice.id) && unified.iter().any(|t| t.account_id == bob.id));

    // --- Repeated syncs keep row ids (no silent folder rebuilds).
    let ids = |e: &Engine| inbox(e, &alice.id).iter().map(|t| t.latest_message_id.clone()).collect::<Vec<_>>();
    let before = ids(&engine);
    engine.sync_and_wait(&alice.id).await.unwrap();
    engine.sync_and_wait(&alice.id).await.unwrap();
    assert_eq!(before, ids(&engine), "row ids stable across syncs");

    // --- Removing an account cleans up.
    engine.remove_account(&bob.id).unwrap();
    assert_eq!(engine.accounts().unwrap().len(), 1);
}

/// A message sent through a branded layout arrives with the header, the footer
/// and the logo as an inline picture, and the sent copy keeps them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn branded_layout_end_to_end() {
    if !enabled() {
        eprintln!("skipping: set REMAIL_IT=1 with a GreenMail server running");
        return;
    }
    let sink = Arc::new(Collector::default());
    let engine = Engine::with_store(Store::open_in_memory().unwrap(), Arc::new(MemoryStore::default()), sink.clone()).unwrap();
    engine.set_settings(&AppSettings { undo_send_seconds: 0, ..Default::default() }).unwrap();
    let carol = engine.add_account(account("carol", "carolpw", false)).await.unwrap();
    let dave = engine.add_account(account("dave", "davepw", false)).await.unwrap();

    // 1x1 transparent PNG.
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
    let layout = engine
        .save_layout(BrandedLayout {
            id: String::new(),
            account_id: Some(carol.id.clone()),
            name: "Work".into(),
            org_name: "Acme & Sons".into(),
            about: "Family bakers since 1990.".into(),
            button: Some(LayoutButton { label: "Visit our shop".into(), url: "https://acme.test/shop".into() }),
            logo: Some(LayoutLogo { data: png.into(), mime: "image/png".into(), width: 120, height: 40, ..Default::default() }),
            sender_name: "Carol Hart".into(),
            position: "Head of Sales".into(),
            phone: "+1 555 010 2030".into(),
            email: addr("carol"),
            style: LayoutStyle::default(),
            use_on_replies: false,
            is_default: true,
            updated_at: 0,
        })
        .unwrap();

    // New mail takes the default layout; the composer's body stays plain.
    let mut d = engine.new_draft(Some(&carol.id)).unwrap();
    assert_eq!(d.layout_id.as_deref(), Some(layout.id.as_str()));
    d.to = vec![EmailAddress::new("", &addr("dave"))];
    d.subject = "Quarterly numbers".into();
    d.html_body = "<p>The figures are attached below.</p>".into();
    send_and_wait(&engine, &sink, d).await;

    engine.sync_and_wait(&dave.id).await.unwrap();
    let threads = inbox(&engine, &dave.id);
    assert_eq!(threads.len(), 1);
    let detail = engine.get_thread(&dave.id, &threads[0].thread_id).await.unwrap();
    let m = &detail.messages[0];
    let html = m.html.as_deref().unwrap();
    for part in ["Acme &amp; Sons", "Family bakers since 1990.", "Visit our shop", "https://acme.test/shop", "The figures are attached below.", "Carol Hart", "Head of Sales", "tel:+15550102030", &format!("mailto:{}", addr("carol"))] {
        assert!(html.contains(part), "{part} missing from {html}");
    }
    // The header comes before the message and the footer after it. (The hidden preview line at the
    // very top repeats the message's first words, so the last one counts; the logo's alt text names
    // the company in the header and again in the footer, so the first one counts.)
    let (header, body, footer) = (html.find("Acme &amp; Sons").unwrap(), html.rfind("The figures are").unwrap(), html.find("Carol Hart").unwrap());
    assert!(header < html.find("Family bakers").unwrap() && html.find("Family bakers").unwrap() < html.find("Visit our shop").unwrap(), "brief, then button");
    assert!(html.find("Visit our shop").unwrap() < body && body < footer, "header, message, footer");
    assert!(html.find("The figures are").unwrap() < header, "the preview line comes first");
    // The logo travelled as a picture inside the message (the reader shows its `cid:` part
    // as a data: image), not as a link out.
    // (The only web address is the header's button.)
    assert_eq!(html.matches("data:image/png;base64,").count(), 2, "the logo in the header and in the footer: {html}");
    assert_eq!(html.matches("https://").count(), 1, "{html}");
    assert!(m.text.as_deref().unwrap_or_default().contains("Head of Sales"), "text part: {:?}", m.text);

    // The copy in Carol's Sent folder has the same layout.
    engine.sync_and_wait(&carol.id).await.unwrap();
    let sent = list(&engine, &carol.id, labels::SENT);
    assert_eq!(sent.len(), 1);
    let sent_detail = engine.get_thread(&carol.id, &sent[0].thread_id).await.unwrap();
    assert!(sent_detail.messages[0].html.as_deref().unwrap().contains("Head of Sales"));
}

async fn send_raw_smtp(from: &str, to: &str, raw: &str) {
    remail_core::smtp::send(
        &ServerConfig { host: std::env::var("REMAIL_IT_HOST").unwrap_or_else(|_| "127.0.0.1".into()), port: 3025, security: Security::None },
        "",
        &remail_core::Login::Password(String::new()),
        false,
        from,
        &[to.to_string()],
        raw.as_bytes(),
    )
    .await
    .unwrap();
}

fn tracing_subscriber_init() -> Option<()> {
    None
}
