//! Data types shared between the engine, the store and the UI layer.
//!
//! Everything here is `Serialize` so the Tauri shell can hand it to the
//! frontend unchanged. Timestamps are unix epoch **milliseconds** unless the
//! field name says otherwise.

use serde::{Deserialize, Serialize};

/// System label ids. Folders discovered over IMAP are mapped onto these when
/// their special-use attribute (or a well-known name) identifies them; every
/// other folder becomes a user label whose id is its IMAP path.
pub mod labels {
    pub const INBOX: &str = "INBOX";
    pub const SENT: &str = "SENT";
    pub const DRAFTS: &str = "DRAFTS";
    pub const TRASH: &str = "TRASH";
    pub const SPAM: &str = "SPAM";
    pub const ARCHIVE: &str = "ARCHIVE";
    pub const ALL: &str = "ALL";
    pub const STARRED: &str = "STARRED";
    pub const UNREAD: &str = "UNREAD";
    /// Virtual views computed locally; never stored in `message_labels`.
    pub const SNOOZED: &str = "SNOOZED";
    pub const SCHEDULED: &str = "SCHEDULED";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    /// TLS from the first byte (IMAPS 993 / SMTPS 465).
    Tls,
    /// Plain connection upgraded with STARTTLS (IMAP 143 / SMTP 587).
    Starttls,
    /// No encryption. Only offered for localhost test servers.
    None,
}

impl Security {
    pub fn as_str(self) -> &'static str {
        match self {
            Security::Tls => "tls",
            Security::Starttls => "starttls",
            Security::None => "none",
        }
    }

    pub fn parse(s: &str) -> Security {
        match s {
            "starttls" => Security::Starttls,
            "none" => Security::None,
            _ => Security::Tls,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub security: Security,
}

/// How to authenticate to IMAP and SMTP.
#[derive(Clone)]
pub enum Login {
    Password(String),
    /// An OAuth access token, sent with SASL XOAUTH2.
    OAuth(String),
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Login::Password(_) => "Login::Password(…)",
            Login::OAuth(_) => "Login::OAuth(…)",
        })
    }
}

/// Add an account by signing in with OAuth (Microsoft).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OAuthAccountRequest {
    /// Pre-fills the sign-in page; the account's address comes from the sign-in.
    pub email: Option<String>,
    pub display_name: Option<String>,
    /// Server overrides (otherwise Microsoft's).
    pub imap: Option<ServerConfig>,
    pub smtp: Option<ServerConfig>,
    pub allow_invalid_certs: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthAccountAdded {
    pub account: Account,
    /// Set when reading works but sending doesn't (e.g. SMTP turned off by an admin).
    pub warning: Option<String>,
}

/// Settings needed to add an account. The password never touches the
/// database; it goes to the OS keyring (see `secrets`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewAccount {
    pub email: String,
    pub display_name: String,
    /// IMAP/SMTP login; defaults to the email address when empty.
    #[serde(default)]
    pub username: String,
    pub password: String,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
    /// Accept self-signed certificates (local test servers only).
    #[serde(default)]
    pub allow_invalid_certs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub username: String,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
    pub allow_invalid_certs: bool,
    /// Accent colour shown next to the account in the unified inbox.
    pub color: String,
    pub sort_order: i64,
    /// Hidden from the unified inbox (still has its own inbox).
    pub hidden_in_unified: bool,
    /// Auto-BCC addresses pre-filled in compose.
    pub auto_bcc: Vec<String>,
    /// Server is Gmail: labels come from X-GM-LABELS and "All Mail" is the archive.
    pub is_gmail: bool,
    /// `password`, or the OAuth provider that signs it in (`microsoft`).
    pub auth: String,
    pub status: AccountStatus,
    pub last_error: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    Active,
    AuthError,
    Offline,
}

impl AccountStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AccountStatus::Active => "active",
            AccountStatus::AuthError => "auth_error",
            AccountStatus::Offline => "offline",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "auth_error" => AccountStatus::AuthError,
            "offline" => AccountStatus::Offline,
            _ => AccountStatus::Active,
        }
    }
}

/// A folder (IMAP mailbox) as a label.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Label {
    pub account_id: String,
    /// System id (`INBOX`, `SENT`, …) or the IMAP path for user folders.
    pub id: String,
    pub name: String,
    /// The IMAP mailbox path backing this label (None for virtual labels).
    pub path: Option<String>,
    pub kind: LabelKind,
    pub parent_id: Option<String>,
    pub unread_count: i64,
    pub total_count: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LabelKind {
    System,
    User,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailAddress {
    pub name: String,
    pub email: String,
}

impl EmailAddress {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self { name: name.into(), email: email.into() }
    }

    /// `Name <email>` or bare `email` when there is no display name.
    pub fn to_header(&self) -> String {
        if self.name.trim().is_empty() {
            self.email.clone()
        } else {
            let needs_quotes = self.name.chars().any(|c| ",;:<>@\"()[]\\.".contains(c));
            if needs_quotes {
                format!("\"{}\" <{}>", self.name.replace('\\', "\\\\").replace('"', "\\\""), self.email)
            } else {
                format!("{} <{}>", self.name, self.email)
            }
        }
    }

    /// Name if present, otherwise the local part of the address.
    pub fn display(&self) -> String {
        if !self.name.trim().is_empty() {
            self.name.trim().to_string()
        } else {
            self.email.split('@').next().unwrap_or(&self.email).to_string()
        }
    }

    /// `Name <email>` for prompts and tool output (no header quoting).
    pub fn display_full(&self) -> String {
        if self.name.trim().is_empty() {
            self.email.clone()
        } else {
            format!("{} <{}>", self.name.trim(), self.email)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMeta {
    /// Index into the parsed message's attachment list.
    pub index: usize,
    pub filename: String,
    pub mime_type: String,
    pub size: usize,
    pub content_id: Option<String>,
    pub inline: bool,
}

/// List-row data for one message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageMeta {
    pub id: String,
    pub account_id: String,
    pub thread_id: String,
    pub folder: String,
    pub uid: u32,
    pub message_id: Option<String>,
    pub subject: String,
    pub from: EmailAddress,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    pub bcc: Vec<EmailAddress>,
    pub reply_to: Vec<EmailAddress>,
    pub date: i64,
    pub snippet: String,
    pub is_unread: bool,
    pub is_starred: bool,
    pub is_draft: bool,
    pub is_answered: bool,
    pub has_attachments: bool,
    pub labels: Vec<String>,
    pub unsubscribe: Option<Unsubscribe>,
    pub body_synced: bool,
}

/// Parsed `List-Unsubscribe` / `List-Unsubscribe-Post` headers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Unsubscribe {
    pub http_url: Option<String>,
    pub mailto: Option<String>,
    /// RFC 8058: `List-Unsubscribe-Post: List-Unsubscribe=One-Click`.
    pub one_click: bool,
}

impl Unsubscribe {
    pub fn is_actionable(&self) -> bool {
        self.http_url.is_some() || self.mailto.is_some()
    }
}

/// One message as displayed in the reading pane.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageFull {
    #[serde(flatten)]
    pub meta: MessageMeta,
    /// Sanitised HTML ready to render in a sandboxed frame.
    pub html: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<AttachmentMeta>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    /// The message has remote images that were stripped by the sanitiser.
    pub has_remote_images: bool,
}

/// Thread list row: the newest message plus aggregates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    pub thread_id: String,
    pub account_id: String,
    pub subject: String,
    /// Distinct participant display names, oldest first (like "Alice, Bob, me").
    pub participants: Vec<String>,
    pub snippet: String,
    pub date: i64,
    pub message_count: i64,
    pub unread_count: i64,
    pub is_starred: bool,
    pub has_attachments: bool,
    pub has_draft: bool,
    pub labels: Vec<String>,
    pub snoozed_until: Option<i64>,
    pub latest_message_id: String,
    pub unsubscribe: Option<Unsubscribe>,
    /// Smart Inbox bucket: `primary` (Important), `updates`, `promotions`, `social`, `forums`.
    pub category: String,
    /// Sender of the latest message (Smart Inbox "always Important/Other").
    pub sender_email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDetail {
    pub thread_id: String,
    pub account_id: String,
    pub subject: String,
    pub messages: Vec<MessageFull>,
    pub labels: Vec<String>,
    pub snoozed_until: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReplyKind {
    Reply,
    ReplyAll,
    Forward,
}

impl ReplyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ReplyKind::Reply => "reply",
            ReplyKind::ReplyAll => "replyAll",
            ReplyKind::Forward => "forward",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "reply" => Some(ReplyKind::Reply),
            "replyAll" => Some(ReplyKind::ReplyAll),
            "forward" => Some(ReplyKind::Forward),
            _ => None,
        }
    }
}

/// A file attached to an outgoing message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutgoingAttachment {
    pub filename: String,
    pub mime_type: String,
    /// Base64 payload (the UI reads files and sends them over IPC).
    pub data_base64: String,
}

/// A draft being composed. Also the payload for send / schedule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub id: String,
    pub account_id: String,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    pub bcc: Vec<EmailAddress>,
    pub subject: String,
    /// What the user typed, including the signature block.
    pub html_body: String,
    /// Quoted original (reply) or forwarded message, appended at send time.
    #[serde(default)]
    pub quoted_html: String,
    #[serde(default)]
    pub attachments: Vec<OutgoingAttachment>,
    /// Local message id this draft replies to / forwards.
    pub reply_to_message_id: Option<String>,
    pub thread_id: Option<String>,
    pub reply_kind: Option<ReplyKind>,
    pub signature_id: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Signature {
    pub id: String,
    pub account_id: Option<String>,
    pub name: String,
    pub html: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub id: String,
    pub name: String,
    pub subject: String,
    pub html: String,
    pub updated_at: i64,
}

/// An outgoing message waiting in the local outbox: either the undo-send
/// window (`send_at` a few seconds out) or a scheduled send.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxItem {
    pub id: String,
    pub account_id: String,
    pub draft: Draft,
    pub send_at: i64,
    pub kind: OutboxKind,
    pub status: OutboxStatus,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutboxKind {
    /// Undo-send hold.
    Undo,
    /// Send later.
    Scheduled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutboxStatus {
    Pending,
    Sending,
    Failed,
}

impl OutboxStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            OutboxStatus::Pending => "pending",
            OutboxStatus::Sending => "sending",
            OutboxStatus::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "sending" => OutboxStatus::Sending,
            "failed" => OutboxStatus::Failed,
            _ => OutboxStatus::Pending,
        }
    }
}

/// Newsletter sender aggregate for the unsubscribe list.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewsletterSender {
    pub account_id: String,
    pub sender: EmailAddress,
    pub message_count: i64,
    pub unread_count: i64,
    pub last_date: i64,
    pub latest_message_id: String,
    pub unsubscribe: Unsubscribe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    pub email: String,
    pub name: String,
    pub frequency: i64,
    pub last_seen: i64,
}

/// What a thread list is showing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadQuery {
    /// `None` = all accounts (unified, minus hidden accounts).
    pub account_id: Option<String>,
    /// Label id (`INBOX`, `STARRED`, `SNOOZED`, a user folder path, …).
    pub label: String,
    #[serde(default)]
    pub unread_only: bool,
    /// Free-text search with Gmail-style operators.
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// Return threads older than this timestamp (keyset pagination).
    #[serde(default)]
    pub before: Option<i64>,
    /// With `before`, the last thread shown: threads sharing its timestamp
    /// continue after it instead of being skipped.
    #[serde(default)]
    pub before_account_id: Option<String>,
    #[serde(default)]
    pub before_thread_id: Option<String>,
    /// Smart Inbox filter: `important`, `other`, or one bucket (`updates`, …).
    #[serde(default)]
    pub category: Option<String>,
}

fn default_limit() -> i64 {
    100
}

/// Events pushed from the engine to the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EngineEvent {
    /// Mailbox contents changed; the UI should refresh lists and counts.
    #[serde(rename_all = "camelCase")]
    MailboxChanged { account_id: String },
    #[serde(rename_all = "camelCase")]
    SyncStatus { account_id: String, syncing: bool, error: Option<String> },
    #[serde(rename_all = "camelCase")]
    NewMail { account_id: String, messages: Vec<MessageMeta> },
    #[serde(rename_all = "camelCase")]
    OutboxChanged,
    #[serde(rename_all = "camelCase")]
    MessageSent { account_id: String, subject: String },
    #[serde(rename_all = "camelCase")]
    SendFailed { account_id: String, subject: String, error: String },
    #[serde(rename_all = "camelCase")]
    SnoozeWoke { account_id: String, thread_id: String, subject: String },
    /// Something outside the UI (assistant, MCP agent) wants a draft opened.
    #[serde(rename_all = "camelCase")]
    OpenCompose { draft_id: String },
    /// Auto-clean moved read conversations out of the Inbox (`action`: trash | archive).
    #[serde(rename_all = "camelCase")]
    AutoCleaned { threads: Vec<crate::engine::ThreadRef>, messages: Vec<String>, action: String },
    /// Calendars or their events changed (sync, edits).
    CalendarChanged,
    /// An event's alert is due.
    #[serde(rename_all = "camelCase")]
    EventReminder { summary: String, start_ms: i64, location: String, all_day: bool, day: String },
    /// Preferences or shortcuts changed on another device: reload them.
    SettingsChanged,
    /// The organisation's AI setup (or this device's copy of its key) changed.
    AiChanged,
}
