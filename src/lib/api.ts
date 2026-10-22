// Typed wrappers around the Tauri commands in src-tauri/src/commands.rs.

import { Channel, invoke, type InvokeArgs } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isMobile, pickFilesInPage } from "./platform";
import type {
  Account,
  AiOutput,
  AiSettings,
  AiSettingsView,
  AiTask,
  AppSettings,
  ChatEvent,
  CloudEvent,
  CloudStatus,
  Device,
  LoginResult,
  Me,
  Meta,
  MigrationInfo,
  ProviderOption,
  UiPrefs,
  ImportedServer,
  McpSettings,
  McpStatus,
  McpToolInfo,
  McpTransport,
  SenderRule,
  SmartCounts,
  Contact,
  Draft,
  EngineEvent,
  Label,
  MessageFull,
  NewAccount,
  OAuthAccountAdded,
  OAuthAccountRequest,
  NewsletterSender,
  OutboxItem,
  OutgoingAttachment,
  ProviderPreset,
  ReplyKind,
  ServerConfig,
  Signature,
  Template,
  ThreadAction,
  ThreadDetail,
  ThreadQuery,
  ThreadRef,
  ThreadSummary,
  TimePreset,
  UnsubscribeResult,
} from "./types";

const chains = new Map<string, Promise<unknown>>();

/** Invoke so that calls in one `queue` (by default, calls to `cmd`) finish in
 *  the order they were made. Commands run concurrently, so two quick saves, or
 *  a star and an unstar, could otherwise land the wrong way round. */
export function invokeInOrder<T>(cmd: string, args?: InvokeArgs, queue = cmd): Promise<T> {
  const run = (chains.get(queue) ?? Promise.resolve()).then(() => invoke<T>(cmd, args));
  const tail = run.catch(() => {});
  chains.set(queue, tail);
  // Forget a queue once it's idle (every draft has its own).
  void tail.then(() => {
    if (chains.get(queue) === tail) chains.delete(queue);
  });
  return run;
}

/** Changes to mail (flags, moves, snoozes, the outbox) apply in the order the user made them. */
const inMailOrder = <T>(cmd: string, args?: InvokeArgs) => invokeInOrder<T>(cmd, args, "mail");

export const api = {
  appInfo: () => invoke<{ version: string; dataDir: string }>("app_info"),
  getSettings: () => invoke<AppSettings>("get_settings"),
  setSettings: (settings: AppSettings) => invokeInOrder<AppSettings>("set_settings", { settings }),

  providerPresets: () => invoke<ProviderPreset[]>("provider_presets"),
  guessServers: (email: string) => invoke<{ imap: ServerConfig; smtp: ServerConfig }>("guess_servers", { email }),
  addAccount: (account: NewAccount) => invoke<Account>("add_account", { account }),
  updateAccount: (id: string, account: NewAccount) => invoke<void>("update_account", { id, account }),
  addMicrosoftAccount: (req: OAuthAccountRequest) => invoke<OAuthAccountAdded>("add_microsoft_account", { req }),
  /** Google sign-in in the browser (OAuth, the org's client ID). */
  addGoogleAccount: (displayName: string | null) => invoke<OAuthAccountAdded | Account>("add_google_account", { displayName }),
  reauthorizeAccount: (id: string) => invoke<void>("reauthorize_account", { id }),
  cancelSignIn: () => invoke<void>("cancel_sign_in"),
  microsoftSignInReady: () => invoke<boolean>("microsoft_sign_in_ready"),
  removeAccount: (id: string) => invoke<void>("remove_account", { id }),
  accounts: () => invoke<Account[]>("accounts"),
  updateAccountPrefs: (
    id: string,
    prefs: { displayName?: string; color?: string; hiddenInUnified?: boolean; autoBcc?: string[] },
  ) => invokeInOrder<void>("update_account_prefs", { id, ...prefs }),

  allLabels: () => invoke<Record<string, Label[]>>("all_labels"),
  unreadCounts: () => invoke<Record<string, number>>("unread_counts"),
  listThreads: (query: ThreadQuery) =>
    invoke<ThreadSummary[]>("list_threads", {
      query: { unreadOnly: false, search: null, limit: 100, before: null, category: null, ...query },
    }),
  loadMore: (accountId: string | null, label: string) => invoke<boolean>("load_more", { accountId, label }),
  serverSearch: (accountId: string | null, query: string) => invoke<number>("server_search", { accountId, query }),
  getThread: (accountId: string, threadId: string) => invoke<ThreadDetail>("get_thread", { accountId, threadId }),
  messageWithImages: (messageId: string) => invoke<MessageFull>("message_with_images", { messageId }),
  saveAttachment: (messageId: string, index: number) => invoke<string | null>("save_attachment", { messageId, index }),
  openAttachment: (messageId: string, index: number) => invoke<void>("open_attachment", { messageId, index }),

  modifyThreads: (threads: ThreadRef[], action: ThreadAction) => inMailOrder<void>("modify_threads", { threads, action }),
  setMessageFlags: (messageId: string, flags: { unread?: boolean; starred?: boolean }) =>
    inMailOrder<void>("set_message_flags", { messageId, unread: flags.unread ?? null, starred: flags.starred ?? null }),
  emptyFolder: (accountId: string, label: string) => inMailOrder<void>("empty_folder", { accountId, label }),
  createFolder: (accountId: string, name: string) => invoke<void>("create_folder", { accountId, name }),
  snoozePresets: () => invoke<TimePreset[]>("snooze_presets"),
  sendLaterPresets: () => invoke<TimePreset[]>("send_later_presets"),
  snooze: (threads: ThreadRef[], until: number) => inMailOrder<void>("snooze", { threads, until }),
  unsnooze: (threads: ThreadRef[]) => inMailOrder<void>("unsnooze", { threads }),

  newDraft: (accountId: string | null, mailto?: string) => invoke<Draft>("new_draft", { accountId, mailto: mailto ?? null }),
  replyDraft: (messageId: string, kind: ReplyKind) => invoke<Draft>("reply_draft", { messageId, kind }),
  editDraftMessage: (messageId: string) => invoke<Draft>("edit_draft_message", { messageId }),
  getDraft: (id: string) => invoke<Draft | null>("get_draft", { id }),
  // A draft's saves, discard and send land in order (a late autosave must not bring back a discarded or sent draft).
  saveDraft: (draft: Draft, syncServer = false) => invokeInOrder<Draft>("save_draft", { draft, syncServer }, `draft:${draft.id}`),
  deleteDraft: (id: string) => invokeInOrder<void>("delete_draft", { id }, `draft:${id}`),
  send: (draft: Draft, sendAt: number | null = null) => invokeInOrder<OutboxItem>("send", { draft, sendAt }, `draft:${draft.id}`),
  undoSend: (outboxId: string) => inMailOrder<Draft | null>("undo_send", { outboxId }),
  outbox: () => invoke<OutboxItem[]>("outbox"),
  reschedule: (outboxId: string, sendAt: number) => inMailOrder<void>("reschedule", { outboxId, sendAt }),
  sendNow: (outboxId: string) => inMailOrder<void>("send_now", { outboxId }),

  signatures: () => invoke<Signature[]>("signatures"),
  saveSignature: (signature: Signature) => invokeInOrder<Signature>("save_signature", { signature }, "signatures"),
  deleteSignature: (id: string) => invokeInOrder<void>("delete_signature", { id }, "signatures"),
  templates: () => invoke<Template[]>("templates"),
  saveTemplate: (template: Template) => invokeInOrder<Template>("save_template", { template }, "templates"),
  deleteTemplate: (id: string) => invokeInOrder<void>("delete_template", { id }, "templates"),

  newsletterSenders: (accountId: string | null) => invoke<NewsletterSender[]>("newsletter_senders", { accountId }),
  unsubscribe: (messageId: string) => invoke<UnsubscribeResult>("unsubscribe", { messageId }),
  suggestContacts: (prefix: string) => invoke<Contact[]>("suggest_contacts", { prefix }),
  syncNow: (accountId: string | null = null) => invoke<void>("sync_now", { accountId }),

  openCompose: (draftId: string) => invoke<void>("open_compose", { draftId }),
  openSettings: (section: string | null = null) => invoke<void>("open_settings", { section }),
  openUrl: (url: string) => invoke<void>("open_url", { url }),
  /** The phone's picker hands over content URIs Rust can't read: read the files in the page there. */
  pickAttachments: (images = false) =>
    isMobile ? pickFilesInPage(images) : invoke<OutgoingAttachment[]>("pick_attachments", { images }),
  readAttachments: (paths: string[]) => invoke<OutgoingAttachment[]>("read_attachments", { paths }),

  smartCounts: (accountId: string | null) => invoke<SmartCounts>("smart_counts", { accountId }),
  setSenderCategory: (email: string, category: string | null) => invokeInOrder<void>("set_sender_category", { email, category }),
  setViewing: (thread: ThreadRef | null) => invoke<void>("set_viewing", { thread }),
  autocleanPreview: () => invoke<number>("autoclean_preview"),
  autocleanUndo: (threads: ThreadRef[], messages: string[]) => inMailOrder<void>("autoclean_undo", { threads, messages }),
  senderRules: () => invoke<SenderRule[]>("sender_rules"),

  aiSettings: () => invoke<AiSettingsView>("ai_settings"),
  setAiSettings: (settings: AiSettings, apiKey: string | null) => invokeInOrder<AiSettingsView>("set_ai_settings", { settings, apiKey }),
  aiModels: (settings: AiSettings, apiKey: string | null) => invoke<string[]>("ai_models", { settings, apiKey }),
  aiTest: (settings: AiSettings, apiKey: string | null) => invoke<string>("ai_test", { settings, apiKey }),
  /** Run a writing task; `onText` receives the streamed text. */
  aiRun: (requestId: string, task: AiTask, onText: (delta: string) => void) => {
    const ch = new Channel<string>();
    ch.onmessage = onText;
    return invoke<AiOutput>("ai_run", { requestId, task, onText: ch });
  },
  aiCancel: (requestId: string) => invoke<void>("ai_cancel", { requestId }),
  aiChat: (conversationId: string, requestId: string, text: string, context: ThreadRef | null, onEvent: (e: ChatEvent) => void) => {
    const ch = new Channel<ChatEvent>();
    ch.onmessage = onEvent;
    return invoke<void>("ai_chat", { conversationId, requestId, text, context, onEvent: ch });
  },
  aiChatReset: (conversationId: string) => invoke<void>("ai_chat_reset", { conversationId }),
  aiApprove: (callId: string, allow: boolean) => invoke<void>("ai_approve", { callId, allow }),

  // Camel Mail account (docs/platform/client-contract.md). Server errors reject
  // with "cloud:<code>:<message>"; show them with errMessage() from i18n.
  cloudStatus: () => invoke<CloudStatus>("cloud_status"),
  /** A URL, a bare host or an organisation code. Saves the server. */
  cloudSetServer: (address: string) => invoke<Meta>("cloud_set_server", { address }),
  cloudSignup: (req: { name: string; email: string; password: string; invite: string | null; locale: string }) =>
    invoke<{ verify: "email" | "none" }>("cloud_signup", req),
  cloudVerifyEmail: (token: string) => invoke<void>("cloud_verify_email", { token }),
  cloudLogin: (email: string, password: string) => invoke<LoginResult>("cloud_login", { email, password }),
  cloudLogin2fa: (challenge: string, code: string) => invoke<{ me: Me }>("cloud_login_2fa", { challenge, code }),
  /** Wipes this device's synced setup and credentials, and stops mail. */
  cloudLogout: () => invoke<void>("cloud_logout"),
  cloudRequestReset: (email: string) => invoke<void>("cloud_request_reset", { email }),
  cloudResetPassword: (token: string, password: string) => invoke<void>("cloud_reset_password", { token, password }),
  cloudMe: () => invoke<Me>("cloud_me"),
  cloudUpdateMe: (name: string) => invoke<Me>("cloud_update_me", { name }),
  cloudChangePassword: (current: string, next: string) => invoke<void>("cloud_change_password", { current, new: next }),
  /** With a challenge: the setup a required-2FA sign-in asks for. */
  cloudTotpSetup: (challenge: string | null = null) => invoke<{ secret: string; otpauthUrl: string }>("cloud_totp_setup", { challenge }),
  /** With a challenge it also completes the sign-in. */
  cloudTotpEnable: (code: string, challenge: string | null = null) => invoke<{ backupCodes: string[] }>("cloud_totp_enable", { code, challenge }),
  cloudTotpDisable: (password: string, code: string) => invoke<void>("cloud_totp_disable", { password, code }),
  cloudDevices: () => invoke<Device[]>("cloud_devices"),
  cloudRevokeDevice: (id: string) => invoke<void>("cloud_revoke_device", { id }),
  /** What "Add account" may offer (the cached org config when offline). */
  cloudProviders: () => invoke<ProviderOption[]>("cloud_providers"),
  cloudMigration: () => invoke<MigrationInfo>("cloud_migration"),
  /** `upload: false` discards this computer's setup and takes the server's. */
  cloudMigrate: (upload: boolean) => invoke<void>("cloud_migrate", { upload }),
  cloudSyncNow: () => invoke<void>("cloud_sync_now"),
  // One queue: two quick changes don't land the wrong way round, and a reload waits for saves.
  uiPrefs: () => invokeInOrder<UiPrefs | null>("ui_prefs", undefined, "ui_prefs"),
  setUiPrefs: (prefs: Partial<UiPrefs>) => invokeInOrder<void>("set_ui_prefs", { prefs }, "ui_prefs"),

  mcpSettings: () => invoke<McpSettings>("mcp_settings"),
  setMcpSettings: (settings: McpSettings) => invoke<McpStatus>("set_mcp_settings", { settings }),
  mcpStatus: () => invoke<McpStatus>("mcp_status"),
  mcpRegenerateToken: () => invoke<McpStatus>("mcp_regenerate_token"),
  mcpTest: (transport: McpTransport) => invoke<{ server: { name: string; version: string; instructions: string | null }; tools: McpToolInfo[] }>("mcp_test", { transport }),
  mcpDiscover: () => invoke<ImportedServer[]>("mcp_discover"),
  mcpParse: (text: string) => invoke<ImportedServer[]>("mcp_parse", { text }),
};

let reqSeq = 0;
/** Unique id for a cancellable AI request. */
export const newRequestId = () => `req-${Date.now().toString(36)}-${++reqSeq}`;

export function onEngineEvent(cb: (e: EngineEvent) => void): Promise<UnlistenFn> {
  return listen<EngineEvent>("engine-event", (e) => cb(e.payload));
}

export function onCloudEvent(cb: (e: CloudEvent) => void): Promise<UnlistenFn> {
  return listen<CloudEvent>("cloud-event", (e) => cb(e.payload));
}

/** Error text from a rejected invoke. */
export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e);
}
