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
  /** "password", or the OAuth provider that signs it in ("microsoft", "google"). */
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
  /** The app's language (synced). Older servers or builds may leave it out. */
  language?: "en" | "ar" | "system";
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
  | { type: "autoCleaned"; threads: ThreadRef[]; messages: string[]; action: "trash" | "archive" }
  /** `prefs` or `shortcuts` changed on another device: reload settings and UI prefs. */
  | { type: "settingsChanged" };

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


// ---------------------------------------------------------------------------
// Remail account (docs/platform/client-contract.md, crates/remail-proto)
// ---------------------------------------------------------------------------

export interface AiPolicy {
  enabled: boolean;
  features: string[];
  monthlyRequestsPerUser: number;
  monthlyTokensPerUser: number;
}

export interface Policies {
  require2fa: boolean;
  sessionDays: number;
  platforms: string[];
  notificationPreview: boolean;
  deviceCacheDays: number;
  ai: AiPolicy;
  mcp: boolean;
}

/** The signed-in Remail user (proto `api::Me`). */
export interface Me {
  id: string;
  name: string;
  email: string;
  role: "owner" | "admin" | "user";
  orgId: string;
  orgName: string;
  /** Two-step verification is on. */
  totp: boolean;
  policies: Policies;
}

export type CloudPhase = "signedOut" | "migrate" | "syncing" | "ready";

export interface CloudStatus {
  /** Base URL in use; null = not chosen yet. */
  server: string | null;
  /** Build-time default (REMAIL_SERVER), if any. */
  defaultServer: string | null;
  signedIn: boolean;
  me: Me | null;
  phase: CloudPhase;
  /** The server can't be reached (the cached setup is in use). */
  offline: boolean;
  /** First sync after signing in. */
  progress: { done: number; total: number; label: string } | null;
}

export type LicenceState = "none" | "valid" | "warning" | "grace" | "expired" | "invalid";

/** `GET /v1/meta` (proto `api::Meta`). */
export interface Meta {
  mode: "hosted" | "onprem";
  version: string;
  /** Oldest client version the server still talks to. */
  minClient: string;
  orgName?: string | null;
  signup: "open" | "invite";
  /** E-mail must be confirmed before signing in. */
  verifyEmail: boolean;
  licence: LicenceState;
}

export type LoginResult = { status: "ok"; me: Me } | { status: "2fa"; challenge: string; setupRequired: boolean };

export interface Device {
  id: string;
  name: string;
  /** linux | windows | macos | ios | android */
  platform: string;
  appVersion: string;
  createdAt: number;
  lastSeenAt: number | null;
  current: boolean;
}

/** A mail server the organisation lists for "Other email" (proto `org::ServerPreset`). */
export interface ServerPreset {
  name: string;
  /** E-mail domain it applies to; empty = any. */
  domain: string;
  imapHost: string;
  imapPort: number;
  imapSecurity: Security;
  smtpHost: string;
  smtpPort: number;
  smtpSecurity: Security;
  /** CalDAV base URL; empty = no calendar. */
  caldavUrl: string;
}

export type ProviderKind = "google" | "microsoft" | "imap";

/** One entry of "Add mailbox" (proto `api::ProviderOption`). */
export interface ProviderOption {
  kind: ProviderKind;
  enabled: boolean;
  oauth?: { clientId: string; tenant: string };
  /** `imap`: may people type any server, or only pick a preset? */
  manualServers: boolean;
  presets: ServerPreset[];
  serverExchange: boolean;
}

export interface MigrationInfo {
  localAccounts: number;
  serverAccounts: number;
}

/** UI preferences that follow the user (they used to live in localStorage). */
export interface UiPrefs {
  shortcuts: Record<string, string[]>;
  sidebarHidden: string[] | null;
  lastComposeAccount: string | null;
}

export type CloudEvent =
  | { kind: "status"; status: CloudStatus }
  | { kind: "signedOut"; reason: "revoked" | "expired" | "user" };
