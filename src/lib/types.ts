// Mirrors of the Rust models in crates/remail-core/src/model.rs (camelCase JSON).

export type Security = "tls" | "starttls" | "none";

export interface ServerConfig {
  host: string;
  port: number;
  security: Security;
}

export interface NewAccount {
  email: string;
  displayName: string;
  username: string;
  password: string;
  imap: ServerConfig;
  smtp: ServerConfig;
  allowInvalidCerts: boolean;
}

export type AccountStatus = "active" | "auth_error" | "offline";

export interface Account {
  id: string;
  email: string;
  displayName: string;
  username: string;
  imap: ServerConfig;
  smtp: ServerConfig;
  allowInvalidCerts: boolean;
  color: string;
  sortOrder: number;
  hiddenInUnified: boolean;
  autoBcc: string[];
  isGmail: boolean;
  /** "password", or the OAuth provider that signs it in ("microsoft"). */
  auth: string;
  status: AccountStatus;
  lastError: string | null;
  createdAt: number;
}

export interface OAuthAccountRequest {
  email?: string | null;
  displayName?: string | null;
  imap?: ServerConfig | null;
  smtp?: ServerConfig | null;
}

export interface OAuthAccountAdded {
  account: Account;
  /** Reading works but sending doesn't (e.g. SMTP turned off by an admin). */
  warning: string | null;
}

export interface Label {
  accountId: string;
  id: string;
  name: string;
  path: string | null;
  kind: "system" | "user";
  parentId: string | null;
  unreadCount: number;
  totalCount: number;
}

export interface EmailAddress {
  name: string;
  email: string;
}

export interface Unsubscribe {
  httpUrl: string | null;
  mailto: string | null;
  oneClick: boolean;
}

export interface AttachmentMeta {
  index: number;
  filename: string;
  mimeType: string;
  size: number;
  contentId: string | null;
  inline: boolean;
}

export interface MessageMeta {
  id: string;
  accountId: string;
  threadId: string;
  folder: string;
  uid: number;
  messageId: string | null;
  subject: string;
  from: EmailAddress;
  to: EmailAddress[];
  cc: EmailAddress[];
  bcc: EmailAddress[];
  replyTo: EmailAddress[];
  date: number;
  snippet: string;
  isUnread: boolean;
  isStarred: boolean;
  isDraft: boolean;
  isAnswered: boolean;
  hasAttachments: boolean;
  labels: string[];
  unsubscribe: Unsubscribe | null;
  bodySynced: boolean;
}

export interface MessageFull extends MessageMeta {
  html: string | null;
  text: string | null;
  attachments: AttachmentMeta[];
  inReplyTo: string | null;
  references: string[];
  hasRemoteImages: boolean;
}

export interface ThreadSummary {
  threadId: string;
  accountId: string;
  subject: string;
  participants: string[];
  snippet: string;
  date: number;
  messageCount: number;
  unreadCount: number;
  isStarred: boolean;
  hasAttachments: boolean;
  hasDraft: boolean;
  labels: string[];
  snoozedUntil: number | null;
  latestMessageId: string;
  unsubscribe: Unsubscribe | null;
  /** Smart Inbox bucket. */
  category: Category;
  senderEmail: string;
}

export type Category = "primary" | "updates" | "promotions" | "social" | "forums";

/** Smart Inbox buckets as the user sees them. */
export const BUCKET_NAMES: Record<Category, string> = { primary: "Important", updates: "Updates", promotions: "Promotions", social: "Social", forums: "Forums" };
/** Smart Inbox list filter. */
export type SmartFilter = "important" | "other" | Exclude<Category, "primary">;

export interface SmartCounts {
  important: number;
  other: number;
}

export interface SenderRule {
  email: string;
  category: Category;
  updatedAt: number;
}

export interface ThreadDetail {
  threadId: string;
  accountId: string;
  subject: string;
  messages: MessageFull[];
  labels: string[];
  snoozedUntil: number | null;
}

export type ReplyKind = "reply" | "replyAll" | "forward";

export interface OutgoingAttachment {
  filename: string;
  mimeType: string;
  dataBase64: string;
}

export interface Draft {
  id: string;
  accountId: string;
  to: EmailAddress[];
  cc: EmailAddress[];
  bcc: EmailAddress[];
  subject: string;
  htmlBody: string;
  quotedHtml: string;
  attachments: OutgoingAttachment[];
  replyToMessageId: string | null;
  threadId: string | null;
  replyKind: ReplyKind | null;
  signatureId: string | null;
  updatedAt: number;
}

export interface Signature {
  id: string;
  accountId: string | null;
  name: string;
  html: string;
  isDefault: boolean;
}

export interface Template {
  id: string;
  name: string;
  subject: string;
  html: string;
  updatedAt: number;
}

export interface OutboxItem {
  id: string;
  accountId: string;
  draft: Draft;
  sendAt: number;
  kind: "undo" | "scheduled";
  status: "pending" | "sending" | "failed";
  attempts: number;
  lastError: string | null;
  createdAt: number;
}

export interface NewsletterSender {
  accountId: string;
  sender: EmailAddress;
  messageCount: number;
  unreadCount: number;
  lastDate: number;
  latestMessageId: string;
  unsubscribe: Unsubscribe;
}

export interface Contact {
  email: string;
  name: string;
  frequency: number;
  lastSeen: number;
}

export interface ThreadQuery {
  accountId: string | null;
  label: string;
  unreadOnly?: boolean;
  search?: string | null;
  limit?: number;
  before?: number | null;
  /** With `before`: the last row shown, so threads sharing its date aren't skipped or repeated. */
  beforeAccountId?: string | null;
  beforeThreadId?: string | null;
  category?: SmartFilter | null;
}

export interface TimePreset {
  id: string;
  label: string;
  at: number;
}

export interface ProviderPreset {
  id: string;
  name: string;
  domains: string[];
  imap: ServerConfig;
  smtp: ServerConfig;
  passwordHelp: string;
  passwordHelpUrl: string | null;
  /** Signs in with this OAuth provider instead of a password. */
  oauth: string | null;
}

export interface AppSettings {
  undoSendSeconds: number;
  theme: "system" | "light" | "dark";
  loadRemoteImages: boolean;
  notifications: boolean;
  syncIntervalSeconds: number;
  defaultReply: "reply" | "replyAll";
  markReadOnOpen: boolean;
  notifyOnSnoozeWake: boolean;
  snoozeLaterMinutes: number;
  snoozeAfternoon: string;
  snoozeEvening: string;
  snoozeMorning: string;
  snoozeTomorrow: string;
  snoozeDayAfter: string;
  snoozeSaturday: string;
  snoozeMonday: string;
  confirmBeforeSend: boolean;
  closeToTray: boolean;
  afterRemove: "nextUnread" | "next" | "previous" | "list";
  layout: "split" | "list";
  smartInbox: boolean;
  notifyImportantOnly: boolean;
  autoClean: AutoClean;
  calendar: import("./calendar.svelte").CalendarPrefs;
  /** App (client) ID for "Sign in with Microsoft"; empty: the built-in one. */
  microsoftClientId: string;
}

export type CleanKind = "promotions" | "updates" | "social" | "forums" | "otp";

export interface AutoClean {
  enabled: boolean;
  action: "trash" | "archive";
  rules: { kind: CleanKind; afterMinutes: number; since: number }[];
}

export interface UnsubscribeResult {
  method: "one-click" | "url" | "email" | "none";
  url: string | null;
  error: string | null;
}

export interface ThreadRef {
  accountId: string;
  threadId: string;
}

export type ThreadAction =
  | { type: "archive" }
  | { type: "moveToInbox"; from: string | null }
  | { type: "trash" }
  | { type: "spam" }
  | { type: "notSpam" }
  | { type: "move"; from: string | null; to: string }
  | { type: "markRead" }
  | { type: "markUnread" }
  | { type: "star" }
  | { type: "unstar" }
  | { type: "deleteForever" };

export type EngineEvent =
  | { type: "mailboxChanged"; accountId: string }
  | { type: "syncStatus"; accountId: string; syncing: boolean; error: string | null }
  | { type: "newMail"; accountId: string; messages: MessageMeta[] }
  | { type: "outboxChanged" }
  | { type: "messageSent"; accountId: string; subject: string }
  | { type: "sendFailed"; accountId: string; subject: string; error: string }
  | { type: "snoozeWoke"; accountId: string; threadId: string; subject: string }
  | { type: "openCompose"; draftId: string }
  | { type: "autoCleaned"; threads: ThreadRef[]; messages: string[]; action: "trash" | "archive" };

/** System label ids (crates/remail-core/src/model.rs `labels`). */
export const L = {
  INBOX: "INBOX",
  SENT: "SENT",
  DRAFTS: "DRAFTS",
  TRASH: "TRASH",
  SPAM: "SPAM",
  ARCHIVE: "ARCHIVE",
  ALL: "ALL",
  STARRED: "STARRED",
  UNREAD: "UNREAD",
  SNOOZED: "SNOOZED",
  SCHEDULED: "SCHEDULED",
} as const;

// ---------------------------------------------------------------------------
// AI assistant
// ---------------------------------------------------------------------------

export interface AiSettings {
  enabled: boolean;
  provider: "openai" | "anthropic";
  baseUrl: string;
  model: string;
  maxTokens: number;
  instructions: string;
  smartSort: boolean;
}

export interface AiSettingsView {
  settings: AiSettings;
  hasKey: boolean;
}

export type AiTask =
  | { type: "summarize"; accountId: string; threadId: string }
  | { type: "reply"; messageId: string; kind: ReplyKind; instructions: string }
  | { type: "compose"; accountId: string | null; instructions: string; to: EmailAddress[]; subject: string; current: string }
  | { type: "rewrite"; accountId: string | null; text: string; mode: string; instructions: string };

export interface AiOutput {
  text: string;
  html: string | null;
  subject: string | null;
  draft: Draft | null;
}

export type ChatEvent =
  | { type: "text"; delta: string }
  | { type: "toolStart"; id: string; name: string; title: string; input: unknown; server: string | null }
  | { type: "toolEnd"; id: string; ok: boolean; summary: string }
  | { type: "approval"; id: string; name: string; title: string; input: unknown; server: string | null; reason?: string }
  | { type: "done" }
  | { type: "error"; message: string };

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

export type McpTransport =
  | { type: "stdio"; command: string; args: string[]; env: Record<string, string>; cwd: string | null }
  | { type: "http"; url: string; headers: Record<string, string> };

export interface McpServerConfig {
  id: string;
  name: string;
  enabled: boolean;
  transport: McpTransport;
  autoApprove: boolean;
}

export interface McpPermissions {
  drafts: boolean;
  send: boolean;
  organize: boolean;
}

export interface McpSettings {
  serverEnabled: boolean;
  port: number;
  permissions: McpPermissions;
  servers: McpServerConfig[];
}

export interface McpStatus {
  running: boolean;
  url: string | null;
  token: string;
  error: string | null;
  command: string;
}

export interface McpToolInfo {
  name: string;
  description: string;
  inputSchema: unknown;
}

export interface ImportedServer {
  source: string;
  path: string;
  config: McpServerConfig;
}
