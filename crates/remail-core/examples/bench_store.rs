//! Time the store's hot queries against a large synthetic mailbox.
//!
//! Usage: cargo run --release -p remail-core --example bench_store -- [messages] [db-path]
//! (default 100000 messages in a temporary database; an existing db-path is reused as is)

use std::time::Instant;

use remail_core::mime::parse;
use remail_core::store::{IncomingMessage, Store};
use remail_core::{NewAccount, Security, ServerConfig, ThreadQuery};

const SENDERS: &[(&str, &str, bool)] = &[
    ("Sara Ahmed", "sara@example.com", false),
    ("Omar Khalid", "omar@example.org", false),
    ("GitHub", "notifications@github.com", true),
    ("Amazon", "deals@amazon.com", true),
    ("LinkedIn", "messages-noreply@linkedin.com", true),
    ("عزيز", "aziz@example.sa", false),
    ("Bank", "alerts@bank.example", true),
    ("Team", "team@company.example", false),
];
const WORDS: &[&str] = &["invoice", "meeting", "project", "report", "update", "weekly", "تقرير", "اجتماع", "release", "budget"];

fn raw(i: usize, thread: usize, date: i64) -> Vec<u8> {
    let (name, email, bulk) = SENDERS[i % SENDERS.len()];
    let w = |k: usize| WORDS[(i / (k + 1) + k) % WORDS.len()];
    let list = if bulk { "List-Unsubscribe: <https://example.com/u>\r\nList-Id: <news.example.com>\r\nPrecedence: bulk\r\n" } else { "" };
    let refs = if i.is_multiple_of(3) { String::new() } else { format!("In-Reply-To: <t{thread}@bench>\r\nReferences: <t{thread}@bench>\r\n") };
    let date = chrono::DateTime::from_timestamp_millis(date).unwrap().to_rfc2822();
    format!(
        "From: {name} <{email}>\r\nTo: Me <me@example.com>\r\nSubject: {} {} #{thread}\r\nDate: {date}\r\nMessage-ID: <m{i}@bench>\r\n{refs}{list}MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello, this is about the {} and the {}. Message {i}.\r\n",
        w(0),
        w(1),
        w(2),
        w(3),
    )
    .into_bytes()
}

fn time<T>(label: &str, runs: u32, mut f: impl FnMut() -> T) -> T {
    let mut out = f();
    let start = Instant::now();
    for _ in 0..runs {
        out = f();
    }
    println!("{label:<40} {:>9.2} ms", start.elapsed().as_secs_f64() * 1000.0 / runs as f64);
    out
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let n: usize = args.next().map(|s| s.parse()).transpose()?.unwrap_or(100_000);
    let tmp = tempfile::tempdir()?;
    let path = args.next().map(Into::into).unwrap_or_else(|| tmp.path().join("bench.db"));
    let fresh = !path.exists();
    let store = Store::open(&path)?;
    let now = remail_core::store::now_ms();

    if fresh {
        let server = |port| ServerConfig { host: "localhost".into(), port, security: Security::Tls };
        let account = NewAccount {
            email: "me@example.com".into(),
            display_name: "Me".into(),
            username: String::new(),
            password: String::new(),
            imap: server(993),
            smtp: server(465),
            allow_invalid_certs: false,
        };
        let acc = store.insert_account(&account, false, "password")?;
        let start = Instant::now();
        for i in 0..n {
            let thread = i / 3;
            let date = now - (n - i) as i64 * 60_000;
            let parsed = parse(&raw(i, thread, date)).expect("parses");
            let (folder, path) = if i % 10 == 9 { ("ARCHIVE", "Archive") } else { ("INBOX", "INBOX") };
            store.upsert_message(&IncomingMessage {
                account_id: acc.id.clone(),
                thread_id: format!("t{thread}"),
                folder: folder.into(),
                path: path.into(),
                uid_validity: 1,
                uid: i as u32 + 1,
                date,
                is_unread: i.is_multiple_of(7),
                is_starred: i.is_multiple_of(50),
                is_draft: false,
                is_answered: false,
                parsed,
                raw: None,
            })?;
        }
        let secs = start.elapsed().as_secs_f64();
        println!("seeded {n} messages in {secs:.1} s ({:.0} msg/s)", n as f64 / secs);
    }

    let q = |label: &str, search: Option<&str>, category: Option<&str>| ThreadQuery {
        account_id: None,
        label: label.into(),
        unread_only: false,
        search: search.map(Into::into),
        limit: 100,
        before: None,
        before_account_id: None,
        before_thread_id: None,
        category: category.map(Into::into),
    };
    let first = time("list_threads INBOX (first page)", 20, || store.list_threads(&q("INBOX", None, None), now))?;
    let last = first.last().expect("a first page");
    let next = ThreadQuery {
        before: Some(last.date),
        before_account_id: Some(last.account_id.clone()),
        before_thread_id: Some(last.thread_id.clone()),
        ..q("INBOX", None, None)
    };
    time("list_threads INBOX (next page)", 20, || store.list_threads(&next, now))?;
    time("list_threads INBOX important", 20, || store.list_threads(&q("INBOX", None, Some("important")), now))?;
    time("list_threads INBOX other", 20, || store.list_threads(&q("INBOX", None, Some("other")), now))?;
    time("list_threads STARRED", 20, || store.list_threads(&q("STARRED", None, None), now))?;
    time("list_threads ARCHIVE", 20, || store.list_threads(&q("ARCHIVE", None, None), now))?;
    time("search \"invoice\"", 10, || store.list_threads(&q("ALL", Some("invoice"), None), now))?;
    time("search \"تقرير\"", 10, || store.list_threads(&q("ALL", Some("تقرير"), None), now))?;
    time("search from:sara is:unread", 10, || store.list_threads(&q("ALL", Some("from:sara is:unread"), None), now))?;
    time("unread_counts", 20, || store.unread_counts(now))?;
    time("smart_unread_counts", 20, || store.smart_unread_counts(None, now))?;
    let accounts = store.accounts()?;
    time("labels", 20, || store.labels(&accounts[0].id))?;
    time("suggest_contacts \"sa\"", 20, || store.suggest_contacts("sa", 8))?;
    time("newsletter_senders", 10, || store.newsletter_senders(None, 200))?;
    let t = &first[0];
    time("thread_messages", 20, || store.thread_messages(&t.account_id, &t.thread_id))?;
    Ok(())
}
