//! Mailbox tools for language models. The in-app assistant uses them (asking
//! the user before anything that sends or moves mail), and the MCP server
//! exposes them to other agents, gated by the user's permissions.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::ai::{self, ToolDef};
use crate::engine::{Engine, ThreadAction, ThreadRef};
use crate::model::*;

/// What an outside agent may do (the in-app assistant may do everything, with confirmation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Permissions {
    pub drafts: bool,
    pub send: bool,
    pub organize: bool,
}

impl Default for Permissions {
    fn default() -> Self {
        Self { drafts: true, send: false, organize: false }
    }
}

impl Permissions {
    pub const ALL: Permissions = Permissions { drafts: true, send: true, organize: true };
}

/// Result of a tool call, as text for the model.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
}

impl ToolResult {
    fn ok(v: Value) -> Self {
        Self { text: v.to_string(), is_error: false }
    }

    fn err(msg: impl Into<String>) -> Self {
        Self { text: msg.into(), is_error: true }
    }
}

/// Streamed to the chat panel while the assistant works.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChatEvent {
    Text { delta: String },
    ToolStart { id: String, name: String, title: String, input: Value, server: Option<String> },
    ToolEnd { id: String, ok: bool, summary: String },
    /// The user must allow or deny this call (`Engine::ai_approve`). `reason` says why
    /// when the user's settings would otherwise have run it without asking.
    Approval {
        id: String,
        name: String,
        title: String,
        input: Value,
        server: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Done,
    Error { message: String },
}

/// Tools that send or move mail; the in-app assistant asks before running them.
pub fn needs_approval(name: &str) -> bool {
    matches!(name, "send_email" | "modify_emails" | "snooze_emails")
}

/// Whether a built-in tool's result carries email content, which anyone can write and
/// which may hold instructions aimed at the model. Unknown tools count as carrying it.
pub fn exposes_mail(name: &str) -> bool {
    !matches!(name, "list_accounts" | "list_folders" | "create_draft" | "send_email" | "modify_emails" | "snooze_emails")
}

fn thread_refs_schema() -> Value {
    json!({
        "type": "array",
        "description": "Conversations to act on, as returned by list_emails/search_emails.",
        "items": {
            "type": "object",
            "properties": { "account_id": { "type": "string" }, "thread_id": { "type": "string" } },
            "required": ["account_id", "thread_id"]
        }
    })
}

fn compose_schema(send: bool) -> Value {
    let mut props = json!({
        "to": { "type": "array", "items": { "type": "string" }, "description": "Recipients as \"Name <email>\" or bare addresses. Optional when replying (defaults to the original sender(s))." },
        "cc": { "type": "array", "items": { "type": "string" } },
        "bcc": { "type": "array", "items": { "type": "string" } },
        "subject": { "type": "string", "description": "Optional when replying." },
        "body": { "type": "string", "description": "Message body as plain text (simple Markdown: **bold**, '- ' bullets). Don't add a signature; Remail appends the user's." },
        "account_id": { "type": "string", "description": "Account to send from; defaults to the first account (or the replied-to message's account)." },
        "reply_to_message_id": { "type": "string", "description": "Message id (from read_thread) to reply to or forward. Keeps threading and quotes the original." },
        "reply_mode": { "type": "string", "enum": ["reply", "reply_all", "forward"], "description": "With reply_to_message_id. Default reply." }
    });
    if send {
        props["send_at"] = json!({ "type": "string", "description": "Optional ISO 8601 time to schedule the send (Send Later). Omit to send now." });
    } else {
        props["open_window"] = json!({ "type": "boolean", "description": "Also open the draft in a Remail compose window for the user. Default true." });
    }
    json!({ "type": "object", "properties": props, "required": ["body"] })
}

/// Tool definitions; `perm` filters what an outside agent may use.
pub fn tool_defs(perm: Permissions) -> Vec<ToolDef> {
    let mut v = vec![
        ToolDef {
            name: "list_accounts".into(),
            description: "List the user's email accounts with their ids and unread Inbox counts.".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: "list_emails".into(),
            description: "List recent conversations in a folder, newest first. Use for questions like 'what's in my inbox', \
                          'unread emails', 'what did I send'."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "folder": { "type": "string", "description": "inbox (default), unread, starred, snoozed, sent, drafts, archive, spam, trash, all, or a folder id from list_folders." },
                    "account_id": { "type": "string", "description": "Limit to one account; default all accounts." },
                    "category": { "type": "string", "enum": ["important", "other", "updates", "promotions", "social", "forums"], "description": "Smart Inbox filter (Inbox only)." },
                    "unread_only": { "type": "boolean" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Default 20." }
                }
            }),
        },
        ToolDef {
            name: "search_emails".into(),
            description: "Search all downloaded mail (newest first). Supports Gmail-style operators: from:, to:, subject:, \
                          has:attachment, is:unread, is:read, is:starred, in:<folder>, before:YYYY/MM/DD, after:YYYY/MM/DD, \
                          older_than:7d, newer_than:2m, \"exact phrase\", -exclude. Plain words match subject, people and body."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "account_id": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Default 20." }
                },
                "required": ["query"]
            }),
        },
        ToolDef {
            name: "read_thread".into(),
            description: "Read a whole conversation: every message's sender, recipients, date, attachments and text (quoted history removed)."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": { "account_id": { "type": "string" }, "thread_id": { "type": "string" } },
                "required": ["account_id", "thread_id"]
            }),
        },
        ToolDef {
            name: "list_folders".into(),
            description: "List mail folders (labels) with ids and unread counts.".into(),
            input_schema: json!({ "type": "object", "properties": { "account_id": { "type": "string" } } }),
        },
    ];
    if perm.drafts {
        v.push(ToolDef {
            name: "create_draft".into(),
            description: "Write an email (new, reply or forward) and save it as a draft for the user to review and send. \
                          Prefer this over send_email unless the user explicitly asked to send."
                .into(),
            input_schema: compose_schema(false),
        });
    }
    if perm.send {
        v.push(ToolDef {
            name: "send_email".into(),
            description: "Send an email (new, reply or forward) right away or at send_at. Only when the user clearly asked to send. \
                          Remail's undo-send delay still applies."
                .into(),
            input_schema: compose_schema(true),
        });
    }
    if perm.organize {
        v.push(ToolDef {
            name: "modify_emails".into(),
            description: "Archive, delete, mark read/unread, star or move conversations.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "threads": thread_refs_schema(),
                    "action": { "type": "string", "enum": ["archive", "trash", "spam", "not_spam", "mark_read", "mark_unread", "star", "unstar", "move_to_inbox", "move"] },
                    "folder": { "type": "string", "description": "Destination folder id or name for action=move." }
                },
                "required": ["threads", "action"]
            }),
        });
        v.push(ToolDef {
            name: "snooze_emails".into(),
            description: "Snooze conversations: hide them from the Inbox until a time, when they come back as new.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "threads": thread_refs_schema(),
                    "until": { "type": "string", "description": "ISO 8601 date-time, e.g. 2026-10-20T09:00." }
                },
                "required": ["threads", "until"]
            }),
        });
    }
    v
}

/// Short human description of a call for the chat panel.
pub fn tool_title(name: &str, input: &Value) -> String {
    let s = |k: &str| input.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match name {
        "list_accounts" => "Checking accounts".into(),
        "list_emails" => {
            let f = s("folder");
            format!("Listing {}", if f.is_empty() { "inbox".into() } else { f })
        }
        "search_emails" => format!("Searching for “{}”", s("query")),
        "read_thread" => "Reading conversation".into(),
        "list_folders" => "Listing folders".into(),
        "create_draft" => format!("Writing draft “{}”", s("subject")),
        "send_email" => {
            let to = input.get("to").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
            if to.is_empty() { "Send email".into() } else { format!("Send email to {to}") }
        }
        "modify_emails" => {
            let n = input.get("threads").and_then(|t| t.as_array()).map(|a| a.len()).unwrap_or(0);
            format!("{} {n} conversation{}", s("action").replace('_', " "), if n == 1 { "" } else { "s" })
        }
        "snooze_emails" => format!("Snooze until {}", s("until")),
        other => other.to_string(),
    }
}

// ----------------------------------------------------------------------------
// Calls
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct CallOptions {
    pub permissions: Permissions,
    /// Whether drafts open in a compose window by default (in-app: yes).
    pub open_drafts: bool,
}

pub async fn call(engine: &Engine, name: &str, input: &Value, opts: CallOptions) -> ToolResult {
    match call_inner(engine, name, input, opts).await {
        Ok(v) => ToolResult::ok(v),
        Err(e) => ToolResult::err(format!("{e:#}")),
    }
}

fn str_arg<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())
}

fn limit_arg(input: &Value) -> i64 {
    input.get("limit").and_then(|v| v.as_i64()).unwrap_or(20).clamp(1, 50)
}

async fn call_inner(engine: &Engine, name: &str, input: &Value, opts: CallOptions) -> Result<Value> {
    let perm = opts.permissions;
    match name {
        "list_accounts" => {
            let unread = engine.unread_counts()?;
            let accounts: Vec<Value> = engine
                .accounts()?
                .into_iter()
                .map(|a| json!({ "account_id": a.id, "email": a.email, "name": a.display_name, "unread_inbox": unread.get(&a.id).copied().unwrap_or(0) }))
                .collect();
            Ok(json!({ "accounts": accounts }))
        }
        "list_emails" => {
            let folder = str_arg(input, "folder").unwrap_or("inbox");
            let label = crate::store::resolve_label_name(folder);
            let q = ThreadQuery {
                account_id: str_arg(input, "account_id").map(String::from),
                label,
                unread_only: input.get("unread_only").and_then(|v| v.as_bool()).unwrap_or(false),
                search: None,
                limit: limit_arg(input),
                before: None,
                before_account_id: None,
                before_thread_id: None,
                category: str_arg(input, "category").map(String::from),
            };
            Ok(json!({ "conversations": threads_json(&engine.list_threads(&q)?) }))
        }
        "search_emails" => {
            let query = str_arg(input, "query").context("query is required")?;
            let q = ThreadQuery {
                account_id: str_arg(input, "account_id").map(String::from),
                label: String::new(),
                unread_only: false,
                search: Some(query.to_string()),
                limit: limit_arg(input),
                before: None,
                before_account_id: None,
                before_thread_id: None,
                category: None,
            };
            let found = engine.list_threads(&q)?;
            Ok(json!({
                "conversations": threads_json(&found),
                "note": if found.is_empty() { "No matches in downloaded mail. Older mail may not be downloaded yet." } else { "" }
            }))
        }
        "read_thread" => {
            let account = str_arg(input, "account_id").context("account_id is required")?;
            let thread = str_arg(input, "thread_id").context("thread_id is required")?;
            let t = engine.get_thread(account, thread).await?;
            Ok(thread_json(&t))
        }
        "list_folders" => {
            let ids: Vec<String> = match str_arg(input, "account_id") {
                Some(a) => vec![a.to_string()],
                None => engine.accounts()?.into_iter().map(|a| a.id).collect(),
            };
            let mut out = Vec::new();
            for id in ids {
                for l in engine.labels(&id)? {
                    out.push(json!({ "account_id": id, "folder_id": l.id, "name": l.name, "unread": l.unread_count }));
                }
            }
            Ok(json!({ "folders": out }))
        }
        "create_draft" if perm.drafts => {
            let draft = build_draft(engine, input).await?;
            let open = input.get("open_window").and_then(|v| v.as_bool()).unwrap_or(opts.open_drafts);
            let draft = engine.save_draft(draft, true).await.context("couldn't save the draft")?;
            if open {
                engine.request_open_compose(&draft.id);
            }
            Ok(json!({
                "draft_id": draft.id,
                "status": if open { "Draft saved and opened in a compose window for the user to review." } else { "Draft saved in the Drafts folder." }
            }))
        }
        "send_email" if perm.send => {
            let draft = build_draft(engine, input).await?;
            if draft.to.is_empty() && draft.cc.is_empty() && draft.bcc.is_empty() {
                bail!("add at least one recipient");
            }
            let send_at = match str_arg(input, "send_at") {
                Some(t) => Some(parse_time(t)?),
                None => None,
            };
            let item = engine.send(draft, send_at)?;
            let when = if item.kind == OutboxKind::Scheduled {
                format!("scheduled for {}", ai::format_date(item.send_at))
            } else {
                "queued; it goes out after the undo-send delay".into()
            };
            Ok(json!({ "status": format!("Email {when}.") }))
        }
        "modify_emails" if perm.organize => {
            let threads = thread_refs(input)?;
            let action = str_arg(input, "action").context("action is required")?;
            let act = match action {
                "archive" => ThreadAction::Archive,
                "trash" | "delete" => ThreadAction::Trash,
                "spam" => ThreadAction::Spam,
                "not_spam" => ThreadAction::NotSpam,
                "mark_read" => ThreadAction::MarkRead,
                "mark_unread" => ThreadAction::MarkUnread,
                "star" => ThreadAction::Star,
                "unstar" => ThreadAction::Unstar,
                "move_to_inbox" => ThreadAction::MoveToInbox { from: None },
                "move" => {
                    let folder = str_arg(input, "folder").context("folder is required for move")?;
                    let account = &threads[0].account_id;
                    let labels = engine.labels(account)?;
                    let to = labels
                        .iter()
                        .find(|l| l.id == folder || l.name.eq_ignore_ascii_case(folder) || l.path.as_deref() == Some(folder))
                        .map(|l| l.id.clone())
                        .unwrap_or_else(|| crate::store::resolve_label_name(folder));
                    ThreadAction::Move { from: None, to }
                }
                other => bail!("unknown action {other}"),
            };
            engine.modify_threads(&threads, &act)?;
            Ok(json!({ "status": format!("{action} applied to {} conversation(s).", threads.len()) }))
        }
        "snooze_emails" if perm.organize => {
            let threads = thread_refs(input)?;
            let until = parse_time(str_arg(input, "until").context("until is required")?)?;
            engine.snooze(&threads, until)?;
            Ok(json!({ "status": format!("Snoozed until {}.", ai::format_date(until)) }))
        }
        "create_draft" | "send_email" | "modify_emails" | "snooze_emails" => {
            bail!("The user hasn't allowed this. They can enable it in Remail → Settings → Connections.")
        }
        other => bail!("unknown tool {other}"),
    }
}

fn thread_refs(input: &Value) -> Result<Vec<ThreadRef>> {
    let arr = input.get("threads").and_then(|v| v.as_array()).context("threads is required")?;
    let refs: Vec<ThreadRef> = arr
        .iter()
        .filter_map(|t| {
            Some(ThreadRef {
                account_id: t.get("account_id")?.as_str()?.to_string(),
                thread_id: t.get("thread_id")?.as_str()?.to_string(),
            })
        })
        .collect();
    if refs.is_empty() {
        bail!("no valid {{account_id, thread_id}} entries in threads");
    }
    Ok(refs)
}

/// ISO 8601 / RFC 3339, or a local `YYYY-MM-DD[ T]HH:MM[:SS]`.
pub fn parse_time(s: &str) -> Result<i64> {
    use chrono::{Local, NaiveDateTime, TimeZone};
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s.trim()) {
        return Ok(dt.timestamp_millis());
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s.trim(), fmt) {
            if let Some(dt) = Local.from_local_datetime(&n).earliest() {
                return Ok(dt.timestamp_millis());
            }
        }
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d") {
        if let Some(dt) = d.and_hms_opt(9, 0, 0).and_then(|n| Local.from_local_datetime(&n).earliest()) {
            return Ok(dt.timestamp_millis());
        }
    }
    Err(anyhow!("couldn't read the time {s:?}; use ISO 8601 like 2026-10-20T09:00"))
}

fn addresses(input: &Value, key: &str) -> Vec<EmailAddress> {
    match input.get(key) {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).filter_map(crate::compose::parse_address).collect(),
        Some(Value::String(s)) => s.split([',', ';']).filter_map(crate::compose::parse_address).collect(),
        _ => vec![],
    }
}

/// Replace a draft's body, keeping the signature block.
pub fn with_body(current: &str, body_html: &str) -> String {
    match current.find("<br><br><div class=\"signature\">") {
        Some(i) => format!("{body_html}{}", &current[i..]),
        None => body_html.to_string(),
    }
}

async fn build_draft(engine: &Engine, input: &Value) -> Result<Draft> {
    let body = input.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let html = ai::text_to_email_html(body);
    let mut d = match str_arg(input, "reply_to_message_id") {
        Some(mid) => {
            let kind = match str_arg(input, "reply_mode") {
                Some("reply_all") => ReplyKind::ReplyAll,
                Some("forward") => ReplyKind::Forward,
                _ => ReplyKind::Reply,
            };
            engine.reply_draft(mid, kind).await.context("couldn't find the message to reply to")?
        }
        None => engine.new_draft(str_arg(input, "account_id"))?,
    };
    if let Some(acc) = str_arg(input, "account_id") {
        if engine.accounts()?.iter().any(|a| a.id == acc) {
            d.account_id = acc.to_string();
        }
    }
    let to = addresses(input, "to");
    if !to.is_empty() || d.to.is_empty() {
        d.to = to;
    }
    let cc = addresses(input, "cc");
    if !cc.is_empty() {
        d.cc = cc;
    }
    d.bcc.extend(addresses(input, "bcc"));
    if let Some(s) = str_arg(input, "subject") {
        d.subject = s.to_string();
    }
    d.html_body = with_body(&d.html_body, &html);
    Ok(d)
}

fn threads_json(threads: &[ThreadSummary]) -> Vec<Value> {
    threads
        .iter()
        .map(|t| {
            json!({
                "account_id": t.account_id,
                "thread_id": t.thread_id,
                "subject": t.subject,
                "participants": t.participants.join(", "),
                "date": ai::format_date(t.date),
                "snippet": t.snippet,
                "messages": t.message_count,
                "unread": t.unread_count > 0,
                "starred": t.is_starred,
                "has_attachments": t.has_attachments,
                "folders": t.labels,
                "category": if t.category == "primary" { "important" } else { t.category.as_str() },
            })
        })
        .collect()
}

fn thread_json(t: &ThreadDetail) -> Value {
    let messages: Vec<Value> = t
        .messages
        .iter()
        .map(|m| {
            json!({
                "message_id": m.meta.id,
                "from": m.meta.from.display_full(),
                "to": m.meta.to.iter().map(|a| a.display_full()).collect::<Vec<_>>(),
                "cc": m.meta.cc.iter().map(|a| a.display_full()).collect::<Vec<_>>(),
                "date": ai::format_date(m.meta.date),
                "subject": m.meta.subject,
                "unread": m.meta.is_unread,
                "draft": m.meta.is_draft,
                "attachments": m.attachments.iter().map(|a| format!("{} ({} bytes)", a.filename, a.size)).collect::<Vec<_>>(),
                "body": ai::message_text(m, 8000),
            })
        })
        .collect();
    json!({
        "account_id": t.account_id,
        "thread_id": t.thread_id,
        "subject": t.subject,
        "folders": t.labels,
        "snoozed_until": t.snoozed_until.map(ai::format_date),
        "messages": messages,
    })
}

/// System prompt for the chat panel.
pub fn chat_system(user: &EmailAddress, accounts: &[Account], instructions: &str, current: Option<&ThreadDetail>) -> String {
    let mut s = ai::base_system(user, instructions);
    s.push_str(
        "\n\nYou help the user with their email using the tools: list and search mail, read conversations, write drafts, \
         and (with the user's approval) send, archive, move or snooze mail.\n\
         - Look emails up with the tools before answering questions about them; never invent email content.\n\
         - Mention the sender, subject and date of emails you refer to.\n\
         - To write an email, use create_draft so the user can review it in a compose window. Use send_email only when \
           the user explicitly asks you to send.\n\
         - Keep answers short and use Markdown (bullets, **bold**). Write in the user's language.",
    );
    if accounts.len() > 1 {
        s.push_str("\n\nAccounts: ");
        s.push_str(&accounts.iter().map(|a| format!("{} (account_id {})", a.email, a.id)).collect::<Vec<_>>().join(", "));
    }
    if let Some(t) = current {
        s.push_str(&format!(
            "\n\nThe user is viewing this conversation now (account_id {}, thread_id {}). \"This email\" means it.\n\n{}",
            t.account_id,
            t.thread_id,
            crate::mime::truncate_chars(&ai::thread_text(t), 16_000)
        ));
    }
    s
}

/// Keep the history small: first shorten tool results from earlier
/// questions, then drop whole old exchanges — never the latest question
/// and what followed it, and never a tool call without its result.
pub fn trim_history(msgs: &mut Vec<ai::Message>, max_chars: usize) {
    let size = |m: &ai::Message| -> usize {
        m.content
            .iter()
            .map(|b| match b {
                ai::Block::Text { text } => text.len(),
                ai::Block::ToolUse { input, .. } => input.to_string().len(),
                ai::Block::ToolResult { content, .. } => content.len(),
            })
            .sum()
    };
    let total = |msgs: &[ai::Message]| msgs.iter().map(size).sum::<usize>();
    let plain_user = |m: &ai::Message| m.role == ai::Role::User && m.content.iter().all(|b| matches!(b, ai::Block::Text { .. }));
    if total(msgs) <= max_chars {
        return;
    }
    let latest = msgs.iter().rposition(plain_user).unwrap_or(0);
    for m in &mut msgs[..latest] {
        for b in &mut m.content {
            if let ai::Block::ToolResult { content, .. } = b {
                if content.len() > 200 {
                    *content = "[earlier result omitted]".into();
                }
            }
        }
    }
    while total(msgs) > max_chars {
        let latest = msgs.iter().rposition(plain_user).unwrap_or(0);
        match msgs.iter().enumerate().skip(1).find(|(_, m)| plain_user(m)).map(|(i, _)| i) {
            Some(next) if next <= latest => {
                msgs.drain(..next);
            }
            _ => break,
        }
    }
}

/// Keep tool output within what a model can take.
pub fn clip_tool_output(text: &str) -> String {
    const MAX: usize = 24_000;
    if text.len() <= MAX {
        text.to_string()
    } else {
        format!("{}\n…[truncated]", crate::mime::truncate_chars(text, MAX))
    }
}

/// One-line summary of a tool result for the chat panel.
pub fn result_summary(r: &ToolResult) -> String {
    if r.is_error {
        return crate::mime::truncate_chars(&r.text, 160);
    }
    if let Ok(v) = serde_json::from_str::<Value>(&r.text) {
        if let Some(n) = v.get("conversations").and_then(|c| c.as_array()).map(|a| a.len()) {
            return format!("{n} conversation{}", if n == 1 { "" } else { "s" });
        }
        if let Some(n) = v.get("messages").and_then(|c| c.as_array()).map(|a| a.len()) {
            return format!("{n} message{}", if n == 1 { "" } else { "s" });
        }
        if let Some(s) = v.get("status").and_then(|c| c.as_str()) {
            return s.to_string();
        }
    }
    crate::mime::truncate_chars(&r.text, 120)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_filter_tools() {
        let names = |p| tool_defs(p).into_iter().map(|t| t.name).collect::<Vec<_>>();
        let read_only = names(Permissions { drafts: false, send: false, organize: false });
        assert!(read_only.contains(&"search_emails".to_string()));
        assert!(!read_only.iter().any(|n| n == "create_draft" || n == "send_email" || n == "modify_emails"));
        let all = names(Permissions::ALL);
        assert!(all.contains(&"send_email".to_string()) && all.contains(&"snooze_emails".to_string()));
    }

    /// Every tool that isn't read-only or a draft must ask first: a new mutating tool fails here
    /// until it is added to `needs_approval` (or deliberately to this list).
    #[test]
    fn mutating_tools_need_approval() {
        let unattended = ["list_accounts", "list_emails", "search_emails", "read_thread", "list_folders", "create_draft"];
        for t in tool_defs(Permissions::ALL) {
            if !unattended.contains(&t.name.as_str()) {
                assert!(needs_approval(&t.name), "{} runs without asking", t.name);
            }
        }
        for name in ["list_emails", "search_emails", "read_thread", "some_new_tool"] {
            assert!(exposes_mail(name), "{name}");
        }
        assert!(!exposes_mail("list_accounts"));
    }

    /// Outside agents only get what the user allowed; the default doesn't include sending.
    #[tokio::test]
    async fn default_permissions_refuse_sending() {
        struct Quiet;
        impl crate::engine::EventSink for Quiet {
            fn emit(&self, _: EngineEvent) {}
        }
        let store = crate::store::Store::open_in_memory().unwrap();
        let engine =
            Engine::with_store(store, std::sync::Arc::new(crate::secrets::MemoryStore::default()), std::sync::Arc::new(Quiet)).unwrap();
        let input = json!({ "to": ["someone@example.com"], "subject": "hi", "body": "hello" });
        let opts = CallOptions { permissions: Permissions::default(), open_drafts: false };
        for tool in ["send_email", "modify_emails", "snooze_emails"] {
            let r = call(&engine, tool, &input, opts).await;
            assert!(r.is_error && r.text.contains("hasn't allowed"), "{tool}: {}", r.text);
        }
        assert!(engine.outbox().unwrap().is_empty());
    }

    #[test]
    fn body_keeps_signature() {
        let cur = "<div><br></div><br><br><div class=\"signature\">Aziz</div>";
        assert_eq!(with_body(cur, "<div>Hi</div>"), "<div>Hi</div><br><br><div class=\"signature\">Aziz</div>");
        assert_eq!(with_body("", "<div>Hi</div>"), "<div>Hi</div>");
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("2026-10-20T09:00:00Z").unwrap(), 1_792_486_800_000);
        assert!(parse_time("2026-10-20 09:00").is_ok());
        assert!(parse_time("tomorrow").is_err());
    }

    #[test]
    fn history_trim_keeps_pairs() {
        let mut msgs = vec![
            ai::Message::user("a".repeat(100)),
            ai::Message { role: ai::Role::Assistant, content: vec![ai::Block::ToolUse { id: "1".into(), name: "x".into(), input: json!({}) }] },
            ai::Message { role: ai::Role::User, content: vec![ai::Block::ToolResult { tool_use_id: "1".into(), content: "r".repeat(100), is_error: false }] },
            ai::Message { role: ai::Role::Assistant, content: vec![ai::Block::Text { text: "done".into() }] },
            ai::Message::user("next"),
            ai::Message { role: ai::Role::Assistant, content: vec![ai::Block::Text { text: "ok".into() }] },
        ];
        trim_history(&mut msgs, 50);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].text(), "next");

        // One big exchange is shortened, not deleted.
        let mut big = vec![
            ai::Message::user("which emails?"),
            ai::Message { role: ai::Role::Assistant, content: vec![ai::Block::ToolUse { id: "1".into(), name: "x".into(), input: json!({}) }] },
            ai::Message { role: ai::Role::User, content: vec![ai::Block::ToolResult { tool_use_id: "1".into(), content: "r".repeat(500), is_error: false }] },
            ai::Message { role: ai::Role::Assistant, content: vec![ai::Block::Text { text: "these".into() }] },
        ];
        trim_history(&mut big, 100);
        assert_eq!(big.len(), 4);
        assert_eq!(big[0].text(), "which emails?");
    }
}
