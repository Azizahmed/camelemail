// Main-window state and actions.

import { SvelteSet } from "svelte/reactivity";
import { listen } from "@tauri-apps/api/event";
import { api, onEngineEvent } from "./api";
import { wakeDate } from "./format";
import { L } from "./types";
import type {
  Account,
  AiSettingsView,
  AppSettings,
  Category,
  Draft,
  EngineEvent,
  Label,
  OutboxItem,
  ReplyKind,
  SmartCounts,
  SmartFilter,
  ThreadAction,
  ThreadRef,
  ThreadSummary,
} from "./types";
import { applyTheme, toast, toastError } from "./ui.svelte";

export type Page = "list" | "search" | "scheduled" | "unsubscribe";

const PAGE_SIZE = 100;

/** Sidebar entries tucked under "More" (folder keys look like `folder:<account>:<id>`). */
function loadHidden(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem("sidebarHidden") ?? "null");
    if (Array.isArray(v)) return v.filter((x) => typeof x === "string");
  } catch {
    /* default below */
  }
  return [L.SPAM, L.TRASH];
}

export const SIDEBAR_MIN = 190;
export const SIDEBAR_MAX = 420;

class MailState {
  ready = $state(false);
  accounts = $state<Account[]>([]);
  labels = $state<Record<string, Label[]>>({});
  unread = $state<Record<string, number>>({});
  settings = $state<AppSettings | null>(null);
  view = $state<{ accountId: string | null; label: string }>({ accountId: null, label: L.INBOX });
  page = $state<Page>("list");
  /** Not deeply reactive: a refresh keeps the object of every unchanged row
   *  (so only changed rows re-render). Replace rows, never mutate them. */
  threads = $state.raw<ThreadSummary[]>([]);
  loading = $state(false);
  loadingMore = $state(false);
  hasMore = $state(true);
  focusedIndex = $state(-1);
  open = $state<ThreadRef | null>(null);
  /** Bumped when the open thread should reload. */
  threadVersion = $state(0);
  /** Current time, ticking each minute, for relative dates. */
  clock = $state(Date.now());
  searchQuery = $state("");
  searchedServer = $state(false);
  searchingServer = $state(false);
  syncing = $state<Record<string, boolean>>({});
  syncErrors = $state<Record<string, string | null>>({});
  outbox = $state<OutboxItem[]>([]);
  sidebarOpen = $state(false);
  /** Kept open beside the content; on until the user undocks it. */
  sidebarDocked = $state(localStorage.getItem("sidebarDocked") !== "0");
  sidebarWidth = $state(Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, Number(localStorage.getItem("sidebarWidth")) || 240)));
  sidebarAccount = $state<string | null>(null);
  modal = $state<null | "palette" | "help">(null);
  /** Reply being written inside the thread view. */
  inlineDraft = $state<Draft | null>(null);
  removing = new SvelteSet<string>();
  sidebarHidden = $state(loadHidden());
  online = $state(navigator.onLine);
  /** Smart Inbox: which half of the Inbox is shown, and an optional bucket of Other. */
  smart = $state({
    tab: "important" as "important" | "other",
    other: null as null | Exclude<Category, "primary">,
    counts: { important: 0, other: 0 } as SmartCounts,
  });
  ai = $state({ enabled: false, ready: false });
  assistantOpen = $state(false);
  /** Mail or the calendar (both live in the main window). */
  space = $state<"mail" | "calendar">("mail");
}

export const mail = new MailState();

export function showCalendar() {
  mail.space = "calendar";
  selection.clear();
  if (!mail.sidebarDocked) mail.sidebarOpen = false;
}

export function showMail() {
  mail.space = "mail";
}

/** Smart Inbox applies to the Inbox list when it's turned on. */
export function smartActive(): boolean {
  return !!mail.settings?.smartInbox && mail.page === "list" && mail.view.label === L.INBOX;
}

function smartFilter(): SmartFilter | null {
  if (!smartActive()) return null;
  return mail.smart.tab === "important" ? "important" : (mail.smart.other ?? "other");
}

export const splitLayout = () => mail.settings?.layout !== "list";

/** Bulk selection, keyed by `accountId|threadId`. */
export const selection = new SvelteSet<string>();

export const key = (t: ThreadRef) => `${t.accountId}|${t.threadId}`;
export const refOf = (t: ThreadSummary): ThreadRef => ({ accountId: t.accountId, threadId: t.threadId });

// ---------------------------------------------------------------------------
// Labels / names
// ---------------------------------------------------------------------------

const SYSTEM_NAMES: Record<string, string> = {
  [L.INBOX]: "Inbox",
  [L.UNREAD]: "Unread",
  [L.STARRED]: "Starred",
  [L.SNOOZED]: "Snoozed",
  [L.ARCHIVE]: "Archive",
  [L.SENT]: "Sent",
  [L.SCHEDULED]: "Scheduled",
  [L.DRAFTS]: "Drafts",
  [L.SPAM]: "Spam",
  [L.TRASH]: "Trash",
  [L.ALL]: "All Mail",
};

export function labelName(label: string, accountId: string | null = mail.view.accountId): string {
  if (SYSTEM_NAMES[label]) return SYSTEM_NAMES[label];
  const lists = accountId ? [mail.labels[accountId] ?? []] : Object.values(mail.labels);
  for (const list of lists) {
    const l = list.find((x) => x.id === label);
    if (l) return l.name.split(/[/.]/).pop() || l.name;
  }
  return label;
}

export function account(id: string | null | undefined): Account | undefined {
  return mail.accounts.find((a) => a.id === id);
}

/** The user's own addresses, lowercased. */
export const ownEmailSet = () => new Set(mail.accounts.map((a) => a.email.toLowerCase()));

/** Scheduled sends for one account (or all with `null`). */
export const scheduledCount = (accountId: string | null) =>
  mail.outbox.filter((o) => o.kind === "scheduled" && (!accountId || o.accountId === accountId)).length;

export function accountName(a: Account | undefined): string {
  if (!a) return "";
  return a.displayName || a.email.split("@")[0];
}

/** Unread count shown next to a folder (unified sums across accounts). */
export function unreadFor(label: string, accountId: string | null): number {
  if (label === L.INBOX && accountId === null) {
    return mail.accounts.filter((a) => !a.hiddenInUnified || mail.accounts.length === 1).reduce((n, a) => n + (mail.unread[a.id] ?? 0), 0);
  }
  if (label === L.INBOX && accountId) return mail.unread[accountId] ?? 0;
  if (label === L.UNREAD || label === L.STARRED || label === L.SNOOZED) return 0;
  const ids = accountId ? [accountId] : mail.accounts.map((a) => a.id);
  let n = 0;
  for (const id of ids) {
    const l = (mail.labels[id] ?? []).find((x) => x.id === label);
    if (l && label !== L.SENT && label !== L.DRAFTS && label !== L.TRASH && label !== L.ARCHIVE) n += l.unreadCount;
  }
  return n;
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

let metaSeq = 0;

export async function refreshMeta() {
  const seq = ++metaSeq;
  try {
    const [accounts, labels, unread] = await Promise.all([api.accounts(), api.allLabels(), api.unreadCounts()]);
    if (seq !== metaSeq) return;
    mail.accounts = accounts;
    mail.labels = labels;
    mail.unread = unread;
    if (mail.view.accountId && !accounts.some((a) => a.id === mail.view.accountId)) {
      mail.view = { accountId: null, label: L.INBOX };
    }
  } catch (e) {
    toastError(e);
  }
}

let listSeq = 0;
/** Bumped when the list starts over (another folder, tab or page), so an older page can't land in it. */
let viewGen = 0;

/** Same row as before? (`next` comes from a fresh query, `prev` from the list.) */
function sameRow(prev: ThreadSummary, next: ThreadSummary): boolean {
  for (const k of Object.keys(next) as (keyof ThreadSummary)[]) {
    const a = prev[k];
    const b = next[k];
    if (a === b) continue;
    if (typeof a !== "object" || typeof b !== "object" || a === null || b === null || JSON.stringify(a) !== JSON.stringify(b)) return false;
  }
  return true;
}

/** `next`, keeping the previous object for each unchanged row. */
function reuseRows(prev: ThreadSummary[], next: ThreadSummary[]): ThreadSummary[] {
  if (prev.length === 0) return next;
  const old = new Map(prev.map((t) => [key(t), t]));
  return next.map((t) => {
    const o = old.get(key(t));
    return o && sameRow(o, t) ? o : t;
  });
}

export async function refreshThreads(opts: { keepCount?: boolean } = {}) {
  const seq = ++listSeq;
  if (mail.page === "scheduled" || mail.page === "unsubscribe") return;
  const searching = mail.page === "search";
  if (searching && !mail.searchQuery.trim()) {
    mail.threads = [];
    return;
  }
  mail.loading = mail.threads.length === 0;
  try {
    const limit = opts.keepCount ? Math.max(PAGE_SIZE, mail.threads.length) : PAGE_SIZE;
    const fresh = await api.listThreads({
      accountId: mail.view.accountId,
      label: searching ? "" : mail.view.label,
      search: searching ? mail.searchQuery : null,
      limit,
      category: searching ? null : smartFilter(),
    });
    if (seq !== listSeq) return;
    // A page of older threads arrived meanwhile: this result would drop it.
    if (opts.keepCount && mail.threads.length > limit) {
      void refreshThreads(opts);
      return;
    }
    const focusedKey = mail.threads[mail.focusedIndex] ? key(mail.threads[mail.focusedIndex]) : null;
    const threads = reuseRows(mail.threads, fresh);
    mail.threads = threads;
    if (focusedKey) {
      const i = threads.findIndex((t) => key(t) === focusedKey);
      mail.focusedIndex = i >= 0 ? i : Math.min(mail.focusedIndex, threads.length - 1);
    }
    for (const k of [...selection]) if (!threads.some((t) => key(t) === k)) selection.delete(k);
    if (!opts.keepCount) mail.hasMore = true;
  } catch (e) {
    toastError(e);
  } finally {
    if (seq === listSeq) mail.loading = false;
  }
}

/** Older threads: from the local cache first, then from the server. */
export async function loadMore() {
  if (mail.loadingMore || !mail.hasMore || mail.page !== "list") return;
  mail.loadingMore = true;
  // A tab or folder switch while this runs makes the results stale.
  const gen = viewGen;
  try {
    const last = mail.threads[mail.threads.length - 1];
    const older = last
      ? await api.listThreads({
          accountId: mail.view.accountId,
          label: mail.view.label,
          limit: PAGE_SIZE,
          before: last.date,
          beforeAccountId: last.accountId,
          beforeThreadId: last.threadId,
          category: smartFilter(),
        })
      : [];
    if (gen !== viewGen) return;
    if (older.length > 0) {
      const known = new Set(mail.threads.map(key));
      mail.threads = [...mail.threads, ...older.filter((t) => !known.has(key(t)))];
      return;
    }
    if ([L.STARRED, L.SNOOZED, L.ALL].includes(mail.view.label as never)) {
      mail.hasMore = false;
      return;
    }
    const more = await api.loadMore(mail.view.accountId, mail.view.label);
    if (gen !== viewGen) return;
    await refreshThreads({ keepCount: true });
    if (!more) mail.hasMore = false;
  } catch (e) {
    toastError(e, "Couldn't load older emails");
  } finally {
    mail.loadingMore = false;
  }
}

let smartSeq = 0;

async function refreshSmartCounts() {
  if (!mail.settings?.smartInbox) return;
  const seq = ++smartSeq;
  const accountId = mail.view.accountId;
  try {
    const counts = await api.smartCounts(accountId);
    if (seq === smartSeq && accountId === mail.view.accountId) mail.smart.counts = counts;
  } catch {
    /* counts are cosmetic */
  }
}

export function setSmartTab(tab: "important" | "other", other: Exclude<Category, "primary"> | null = null) {
  if (mail.smart.tab === tab && mail.smart.other === other) return;
  mail.smart.tab = tab;
  mail.smart.other = other;
  viewGen++;
  mail.threads = [];
  mail.focusedIndex = -1;
  if (!splitLayout()) mail.open = null;
  selection.clear();
  void refreshThreads();
}

let outboxSeq = 0;

export async function refreshOutbox() {
  const seq = ++outboxSeq;
  try {
    const outbox = await api.outbox();
    if (seq === outboxSeq) mail.outbox = outbox;
  } catch {
    /* ignore */
  }
}

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

export function navigate(accountId: string | null, label: string) {
  mail.space = "mail";
  if (label === L.SCHEDULED) {
    mail.page = "scheduled";
    mail.view = { accountId, label };
    mail.open = null;
    selection.clear();
    void refreshOutbox();
    return;
  }
  mail.view = { accountId, label };
  mail.page = "list";
  mail.open = null;
  mail.inlineDraft = null;
  viewGen++;
  mail.threads = [];
  mail.focusedIndex = -1;
  selection.clear();
  void refreshThreads();
  void refreshSmartCounts();
}

export function openPage(page: Page) {
  mail.space = "mail";
  mail.page = page;
  mail.open = null;
  viewGen++;
  selection.clear();
  if (page === "search") {
    mail.threads = [];
    mail.searchedServer = false;
  }
}

export function openThread(t: ThreadSummary | ThreadRef) {
  mail.space = "mail";
  const i = mail.threads.findIndex((x) => x.accountId === t.accountId && x.threadId === t.threadId);
  if (i >= 0) mail.focusedIndex = i;
  // Already open: keep it (and the reply being written) as it is.
  if (mail.open && key(mail.open) === key(t)) return;
  mail.open = { accountId: t.accountId, threadId: t.threadId };
  mail.inlineDraft = null;
  openRow = rowSig(mail.threads[i]);
}

export function closeThread() {
  mail.open = null;
  mail.inlineDraft = null;
}

/** Move to the adjacent thread (J/K inside a thread). */
export function stepThread(dir: 1 | -1) {
  if (mail.threads.length === 0) return;
  const cur = mail.open ? mail.threads.findIndex((t) => t.accountId === mail.open!.accountId && t.threadId === mail.open!.threadId) : mail.focusedIndex;
  const next = Math.max(0, Math.min(mail.threads.length - 1, (cur < 0 ? -1 : cur) + dir));
  mail.focusedIndex = next;
  if (mail.open || (splitLayout() && (mail.page === "list" || mail.page === "search"))) openThread(mail.threads[next]);
}

// ---------------------------------------------------------------------------
// Actions with undo
// ---------------------------------------------------------------------------

interface Undo {
  label: string;
  run: () => Promise<void>;
  at: number;
}

let lastUndo: Undo | null = null;

export async function undoLast() {
  const u = lastUndo;
  lastUndo = null;
  if (!u || Date.now() - u.at > 60_000) {
    toast("Nothing to undo");
    return;
  }
  try {
    await u.run();
    toast(u.label);
  } catch (e) {
    toastError(e, "Could not undo");
  }
}

/** Threads currently targeted: the selection, else the open thread, else the focused row. */
export function targets(): ThreadRef[] {
  if (selection.size > 0) {
    return [...selection].map((k) => {
      const [accountId, ...rest] = k.split("|");
      return { accountId, threadId: rest.join("|") };
    });
  }
  if (mail.open) return [mail.open];
  const t = mail.threads[mail.focusedIndex];
  return t ? [refOf(t)] : [];
}

function threadsFor(refs: ThreadRef[]): ThreadSummary[] {
  return refs.map((r) => mail.threads.find((t) => t.accountId === r.accountId && t.threadId === r.threadId)).filter(Boolean) as ThreadSummary[];
}

/** Thread to open after the open one leaves the view (Settings → "After archiving or deleting"). */
function nextAfterRemoval(removed: string[]): ThreadSummary | null {
  const mode = mail.settings?.afterRemove ?? "nextUnread";
  const list = mail.threads;
  const cur = mail.open ? list.findIndex((t) => key(t) === key(mail.open!)) : -1;
  if (mode === "list" || cur < 0) return null;
  const keep = (i: number) => !removed.includes(key(list[i]));
  const after = () => list.find((_, i) => i > cur && keep(i)) ?? null;
  const before = () => list.findLast((_, i) => i < cur && keep(i)) ?? null;
  if (mode === "nextUnread") {
    const unread = (i: number) => keep(i) && list[i].unreadCount > 0;
    const below = list.find((_, i) => i > cur && unread(i));
    const above = list.find((_, i) => i < cur && unread(i));
    if (below || above) return (below ?? above)!;
    // Nothing unread left: keep reading in the split view, or go back to the list.
    return splitLayout() ? (after() ?? before()) : null;
  }
  if (mode === "previous") return before() ?? after();
  return after() ?? before();
}

/** After an action that removes threads from the view: animate, advance. */
async function leaveView(refs: ThreadRef[]) {
  const keys = refs.map(key);
  const next = mail.open && keys.includes(key(mail.open)) ? nextAfterRemoval(keys) : null;
  for (const k of keys) mail.removing.add(k);
  await new Promise((r) => setTimeout(r, 150));
  for (const k of keys) mail.removing.delete(k);
  if (mail.open && keys.includes(key(mail.open))) {
    if (next) {
      openThread(next);
    } else {
      mail.open = null;
      mail.inlineDraft = null;
    }
  }
  for (const k of keys) selection.delete(k);
}

export async function act(action: ThreadAction, refs: ThreadRef[] = targets()) {
  if (refs.length === 0) return;
  const view = mail.view.label;
  const count = refs.length;
  const fromView = mail.page === "search" ? null : view;
  let message = "";
  let undo: ThreadAction | null = null;
  let removes = true;
  switch (action.type) {
    case "archive":
      if (view !== L.INBOX && view !== L.UNREAD && mail.page !== "search") {
        toast("Nothing to archive — already archived");
        return;
      }
      message = count === 1 ? "Archived" : `Archived ${count}`;
      undo = { type: "moveToInbox", from: L.ARCHIVE };
      break;
    case "trash":
      message = count === 1 ? "Moved to trash" : `Moved ${count} to trash`;
      undo = { type: "moveToInbox", from: L.TRASH };
      break;
    case "spam":
      message = count === 1 ? "Marked as spam" : `Marked ${count} as spam`;
      undo = { type: "notSpam" };
      break;
    case "notSpam":
      message = "Moved to inbox";
      break;
    case "moveToInbox":
      message = count === 1 ? "Restored to inbox" : `Restored ${count} to inbox`;
      break;
    case "move":
      message = `Moved to ${labelName(action.to, refs[0].accountId)}`;
      undo = fromView && fromView !== L.STARRED && fromView !== L.UNREAD ? { type: "move", from: action.to, to: fromView } : null;
      break;
    case "markRead":
      message = count === 1 ? "Marked as read" : `Marked ${count} as read`;
      undo = { type: "markUnread" };
      removes = view === L.UNREAD;
      break;
    case "markUnread":
      message = count === 1 ? "Marked as unread" : `Marked ${count} as unread`;
      undo = { type: "markRead" };
      removes = false;
      break;
    case "star":
      message = "Starred";
      undo = { type: "unstar" };
      removes = false;
      break;
    case "unstar":
      message = "Unstarred";
      undo = { type: "star" };
      removes = view === L.STARRED;
      break;
    case "deleteForever":
      message = count === 1 ? "Deleted forever" : `Deleted ${count} forever`;
      break;
  }
  try {
    if (removes) await leaveView(refs);
    await api.modifyThreads(refs, action);
    if (action.type === "markUnread" && mail.open && !splitLayout()) closeThread();
    await refreshThreads({ keepCount: true });
    if (undo) {
      const u = undo;
      lastUndo = { label: undoLabel(action), run: () => api.modifyThreads(refs, u).then(() => refreshThreads({ keepCount: true })), at: Date.now() };
      toast(`${message} (Z to undo)`, { action: { label: "Undo", run: undoLast } });
    } else {
      toast(message);
    }
  } catch (e) {
    toastError(e);
    void refreshThreads({ keepCount: true });
  }
}

function undoLabel(a: ThreadAction): string {
  switch (a.type) {
    case "archive":
      return "Unarchived";
    case "trash":
      return "Restored from trash";
    case "spam":
      return "Removed from spam";
    case "markRead":
      return "Marked as unread";
    case "markUnread":
      return "Marked as read";
    case "star":
      return "Unstarred";
    case "unstar":
      return "Starred";
    default:
      return "Undone";
  }
}

export async function toggleStar(refs: ThreadRef[] = targets()) {
  const ts = threadsFor(refs);
  const allStarred = ts.length > 0 && ts.every((t) => t.isStarred);
  await act({ type: allStarred ? "unstar" : "star" }, refs);
}

export async function toggleRead(refs: ThreadRef[] = targets()) {
  const ts = threadsFor(refs);
  const anyUnread = ts.some((t) => t.unreadCount > 0);
  await act({ type: anyUnread ? "markRead" : "markUnread" }, refs);
}

export async function trashOrRestore(refs: ThreadRef[] = targets()) {
  if (mail.view.label === L.TRASH && mail.page === "list") await act({ type: "moveToInbox", from: L.TRASH }, refs);
  else await act({ type: "trash" }, refs);
}

export async function snooze(until: number, refs: ThreadRef[] = targets()) {
  if (refs.length === 0) return;
  try {
    if (mail.view.label !== L.SNOOZED) await leaveView(refs);
    await api.snooze(refs, until);
    lastUndo = { label: "Email unsnoozed", run: () => api.unsnooze(refs).then(() => refreshThreads({ keepCount: true })), at: Date.now() };
    toast(`Snoozed until ${wakeDate(until)}`, { action: { label: "Undo", run: undoLast } });
    await refreshThreads({ keepCount: true });
  } catch (e) {
    toastError(e, "Failed to snooze");
  }
}

export async function unsnooze(refs: ThreadRef[] = targets()) {
  try {
    if (mail.view.label === L.SNOOZED) await leaveView(refs);
    await api.unsnooze(refs);
    toast("Email unsnoozed");
    await refreshThreads({ keepCount: true });
  } catch (e) {
    toastError(e, "Failed to unsnooze");
  }
}

// ---------------------------------------------------------------------------
// Compose
// ---------------------------------------------------------------------------

function composeAccount(): string | null {
  return mail.view.accountId ?? localStorage.getItem("lastComposeAccount") ?? null;
}

export async function compose(mailto?: string) {
  if (mail.accounts.length === 0) {
    toast("Add an account first");
    return;
  }
  try {
    const d = await api.newDraft(composeAccount(), mailto);
    await api.openCompose(d.id);
  } catch (e) {
    toastError(e);
  }
}

export async function reply(messageId: string, kind: ReplyKind, popOut = false) {
  try {
    const d = await api.replyDraft(messageId, kind);
    if (popOut) await api.openCompose(d.id);
    else mail.inlineDraft = d;
  } catch (e) {
    toastError(e);
  }
}

// ---------------------------------------------------------------------------
// Send progress (undo send window)
// ---------------------------------------------------------------------------

export const sending = $state<{ items: OutboxItem[] }>({ items: [] });

export async function undoSend(item: OutboxItem) {
  try {
    const draft = await api.undoSend(item.id);
    if (!draft) {
      toast("Couldn't undo — the email is already sending", { kind: "error" });
      return;
    }
    toast("Sending undone");
    await api.openCompose(draft.id);
  } catch (e) {
    toastError(e);
  }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

let refreshTimer: ReturnType<typeof setTimeout> | undefined;
/** When the oldest change still waiting for a refresh came in (0 = none). */
let refreshDue = 0;
let refreshing = false;
let refreshAgain = false;
const changedAccounts = new Set<string>();
/** The open thread's list row as of its last (re)load; see `rowSig`. */
let openRow: string | null = null;

/** The parts of a list row that mean the open thread needs reloading. */
function rowSig(t: ThreadSummary | undefined): string | null {
  return t ? JSON.stringify([t.messageCount, t.unreadCount, t.latestMessageId, t.labels, t.isStarred, t.hasDraft]) : null;
}

/** Refresh after mailbox changes: debounced (but at most ~1 s behind a
 *  steady stream of events) and never more than one refresh at a time. */
function scheduleRefresh(accountId?: string) {
  if (accountId) changedAccounts.add(accountId);
  if (refreshing) {
    refreshAgain = true;
    return;
  }
  const now = Date.now();
  if (!refreshDue) refreshDue = now;
  clearTimeout(refreshTimer);
  refreshTimer = setTimeout(() => void runRefresh(), Math.max(0, Math.min(200, refreshDue + 1000 - now)));
}

async function runRefresh() {
  refreshDue = 0;
  refreshing = true;
  const accounts = new Set(changedAccounts);
  changedAccounts.clear();
  try {
    await Promise.all([refreshMeta(), refreshThreads({ keepCount: true }), refreshSmartCounts()]);
  } finally {
    refreshing = false;
  }
  // Reload the open thread only when its own row changed (or it isn't in the list to tell).
  const open = mail.open;
  if (open && accounts.has(open.accountId)) {
    const row = rowSig(mail.threads.find((t) => key(t) === key(open)));
    if (row === null || row !== openRow) {
      openRow = row;
      mail.threadVersion++;
    }
  }
  if (refreshAgain) {
    refreshAgain = false;
    scheduleRefresh();
  }
}

async function handleEvent(e: EngineEvent) {
  switch (e.type) {
    case "mailboxChanged":
      scheduleRefresh(e.accountId);
      break;
    case "syncStatus":
      mail.syncing[e.accountId] = e.syncing;
      if (!e.syncing) {
        const had = mail.syncErrors[e.accountId];
        mail.syncErrors[e.accountId] = e.error;
        // The account's status (shown as "Can't reach server") changed, possibly before we listened.
        const shownBad = mail.accounts.find((a) => a.id === e.accountId)?.status !== "active";
        if (e.error ? !had : had || shownBad) void refreshMeta();
      }
      break;
    case "outboxChanged":
      await refreshOutbox();
      sending.items = mail.outbox.filter((i) => i.kind === "undo");
      break;
    case "messageSent":
      toast("Sent!", { kind: "success" });
      break;
    case "sendFailed":
      toast(`Couldn't send "${e.subject || "(No subject)"}": ${e.error}`, {
        kind: "error",
        duration: 10000,
        action: {
          label: "Open draft",
          run: async () => {
            const item = mail.outbox.find((i) => i.draft.subject === e.subject && i.status === "failed");
            if (item) {
              const d = await api.undoSend(item.id);
              if (d) await api.openCompose(d.id);
            }
          },
        },
      });
      break;
    case "snoozeWoke":
      toast(`Back from snooze: ${e.subject || "(No subject)"}`, {
        action: { label: "Open", run: () => openThread({ accountId: e.accountId, threadId: e.threadId }) },
      });
      break;
    case "autoCleaned": {
      const n = e.threads.length;
      const what = `${n} read email${n === 1 ? "" : "s"}`;
      toast(e.action === "archive" ? `Auto-clean archived ${what}` : `Auto-clean moved ${what} to Trash`, {
        duration: 8000,
        action: {
          label: "Undo",
          run: async () => {
            try {
              await api.autocleanUndo(e.threads, e.messages);
              toast(n === 1 ? "Restored. Auto-clean will leave it alone." : "Restored. Auto-clean will leave them alone.");
            } catch (err) {
              toastError(err);
            }
          },
        },
      });
      break;
    }
    case "newMail":
    case "openCompose":
      break;
  }
}

function applyAi(v: AiSettingsView) {
  const s = v.settings;
  mail.ai.enabled = s.enabled;
  mail.ai.ready = s.enabled && !!s.model.trim() && !!s.baseUrl.trim() && (v.hasKey || /:\/\/(localhost|127\.|\[::1\]|0\.0\.0\.0)/.test(s.baseUrl));
  if (!mail.ai.ready) mail.assistantOpen = false;
}

/** Guard for AI actions: explain how to set it up when it isn't. */
export function aiAvailable(): boolean {
  if (mail.ai.ready) return true;
  toast(mail.ai.enabled ? "Finish setting up the AI assistant in Settings" : "Turn on the AI assistant in Settings", {
    action: { label: "Open Settings", run: () => void api.openSettings("ai") },
  });
  return false;
}

export async function initMail() {
  const settings = await api.getSettings();
  mail.settings = settings;
  applyTheme(settings.theme);
  await refreshMeta();
  await refreshOutbox();
  sending.items = mail.outbox.filter((i) => i.kind === "undo");
  if (mail.accounts.length === 1) mail.view = { accountId: null, label: L.INBOX };
  await onEngineEvent((e) => void handleEvent(e));
  await listen<AppSettings>("settings-changed", (e) => {
    const smartChanged = mail.settings?.smartInbox !== e.payload.smartInbox;
    mail.settings = e.payload;
    applyTheme(e.payload.theme);
    mail.threadVersion++;
    if (smartChanged) {
      viewGen++;
      mail.threads = [];
      void refreshThreads();
      void refreshSmartCounts();
    }
  });
  try {
    applyAi(await api.aiSettings());
  } catch {
    /* AI stays off */
  }
  await listen<AiSettingsView>("ai-settings-changed", (e) => applyAi(e.payload));
  // The tray's "Open Calendar".
  await listen("show-calendar", () => showCalendar());
  window.addEventListener("online", () => {
    mail.online = true;
    toast("Back online");
    void api.syncNow();
  });
  window.addEventListener("offline", () => {
    mail.online = false;
    toast("You are offline");
  });
  // Re-render relative dates every minute.
  setInterval(() => (mail.clock = Date.now()), 60_000);
  mail.ready = true;
  await refreshThreads();
  void refreshSmartCounts();
}

/** Switch between the split view and the full-width list (saved in settings). */
export async function toggleLayout() {
  if (!mail.settings) return;
  const layout = splitLayout() ? "list" : "split";
  mail.settings = { ...mail.settings, layout };
  try {
    await api.setSettings($state.snapshot(mail.settings) as AppSettings);
  } catch (e) {
    toastError(e);
  }
}

export function toggleAssistant() {
  if (!mail.assistantOpen && !aiAvailable()) return;
  mail.assistantOpen = !mail.assistantOpen;
}

export function setDocked(v: boolean) {
  mail.sidebarDocked = v;
  localStorage.setItem("sidebarDocked", v ? "1" : "0");
}

export function setSidebarWidth(px: number, save = false) {
  mail.sidebarWidth = Math.round(Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, px)));
  if (save) localStorage.setItem("sidebarWidth", String(mail.sidebarWidth));
}

export function isHidden(key: string) {
  return mail.sidebarHidden.includes(key);
}

/** Move a sidebar entry under "More", or back out of it. */
export function toggleHidden(key: string) {
  mail.sidebarHidden = isHidden(key) ? mail.sidebarHidden.filter((k) => k !== key) : [...mail.sidebarHidden, key];
  localStorage.setItem("sidebarHidden", JSON.stringify(mail.sidebarHidden));
}

export async function runServerSearch() {
  if (!mail.searchQuery.trim() || mail.searchingServer) return;
  mail.searchingServer = true;
  try {
    const n = await api.serverSearch(mail.view.accountId, mail.searchQuery);
    mail.searchedServer = true;
    await refreshThreads();
    toast(n > 0 ? `Found ${n} more on the server` : "No more results on the server");
  } catch (e) {
    toastError(e, "Server search failed");
  } finally {
    mail.searchingServer = false;
  }
}
