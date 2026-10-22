// Phone views: what the drawer, the bottom bar's pins and the header title
// point at, and the list's swipe actions. Pins and swipe choices follow the
// user to every phone (UI prefs `mobilePins` / `swipe`).

import { Archive, CalendarClock, CalendarDays, Clock3, FileText, Folder, Inbox, Layers, Mail, MessagesSquare, Send, ShieldAlert, Star, Tag, Trash2, Users } from "@lucide/svelte";
import { t, type MessageKey } from "../lib/i18n.svelte";
import { labelName, mail, navigate, smartActive } from "../lib/mail.svelte";
import { saveUiPrefs, uiPrefs } from "../lib/prefs.svelte";
import { push } from "./nav.svelte";
import { L, type Category } from "../lib/types";

export type Icon = typeof Inbox;
export type OtherBucket = Exclude<Category, "primary">;

/** A view id: a system label (`INBOX`), `smart:<bucket>`, `folder:<account>:<label>` or `calendar`. */
export type ViewId = string;

export const DEFAULT_PINS: ViewId[] = [L.INBOX, L.UNREAD, L.STARRED, L.SNOOZED];
export const MAX_PINS = 4;

const SYSTEM_ICONS: Record<string, Icon> = {
  [L.INBOX]: Inbox,
  [L.UNREAD]: Mail,
  [L.STARRED]: Star,
  [L.SNOOZED]: Clock3,
  [L.ARCHIVE]: Archive,
  [L.SENT]: Send,
  [L.SCHEDULED]: CalendarClock,
  [L.DRAFTS]: FileText,
  [L.SPAM]: ShieldAlert,
  [L.TRASH]: Trash2,
};

const SYSTEM_KEYS: Record<string, MessageKey> = {
  [L.INBOX]: "m.view.inbox",
  [L.UNREAD]: "m.view.unread",
  [L.STARRED]: "m.view.starred",
  [L.SNOOZED]: "m.view.snoozed",
  [L.ARCHIVE]: "m.view.archive",
  [L.SENT]: "m.view.sent",
  [L.SCHEDULED]: "m.view.scheduled",
  [L.DRAFTS]: "m.view.drafts",
  [L.SPAM]: "m.view.spam",
  [L.TRASH]: "m.view.trash",
  [L.ALL]: "m.view.all",
};

export const SMART_BUCKETS: { id: OtherBucket | "other"; icon: Icon }[] = [
  { id: "other", icon: Layers },
  { id: "updates", icon: Tag },
  { id: "promotions", icon: Tag },
  { id: "social", icon: Users },
  { id: "forums", icon: MessagesSquare },
];

/** A Smart Inbox bucket's name in the app language. */
export const bucketName = (id: Category | "other") => t(`m.bucket.${id}` as MessageKey);

export function viewIcon(id: ViewId): Icon {
  if (SYSTEM_ICONS[id]) return SYSTEM_ICONS[id];
  if (id.startsWith("smart:")) return SMART_BUCKETS.find((b) => `smart:${b.id}` === id)?.icon ?? Tag;
  if (id === "calendar") return CalendarDays;
  return Folder;
}

/** A view's name in the app language (folders keep their own names). */
export function viewName(id: ViewId): string {
  if (id === "calendar") return t("m.view.calendar");
  if (id.startsWith("smart:")) {
    const b = SMART_BUCKETS.find((x) => `smart:${x.id}` === id);
    return b ? bucketName(b.id) : id;
  }
  if (id.startsWith("folder:")) {
    const [, accountId, ...rest] = id.split(":");
    return labelName(rest.join(":"), accountId);
  }
  if (SYSTEM_KEYS[id]) return t(SYSTEM_KEYS[id]);
  return labelName(id, null);
}

/** The view shown now (for the header title and the active pin). */
export function currentView(): ViewId {
  if (mail.page === "scheduled") return L.SCHEDULED;
  const label = mail.view.label;
  if (label === L.INBOX && smartActive() && mail.smart.tab === "other") return `smart:${mail.smart.other ?? "other"}`;
  const isSystem = !!SYSTEM_ICONS[label];
  if (!isSystem && mail.view.accountId) return `folder:${mail.view.accountId}:${label}`;
  return label;
}

/** Show a view in the list (the calendar is a screen of its own: the caller opens it). */
export function showView(id: ViewId, accountId: string | null = mail.view.accountId) {
  if (id.startsWith("folder:")) {
    const [, acc, ...rest] = id.split(":");
    navigate(acc, rest.join(":"));
    return;
  }
  if (id.startsWith("smart:")) {
    const bucket = id.slice(6);
    mail.smart.tab = "other";
    mail.smart.other = bucket === "other" ? null : (bucket as OtherBucket);
    navigate(accountId, L.INBOX);
    return;
  }
  if (id === L.INBOX) {
    mail.smart.tab = "important";
    mail.smart.other = null;
  }
  navigate(accountId, id);
}

/** Open a view: the list for mail views, a screen for Scheduled and the calendar. */
export function openView(id: ViewId, accountId: string | null = mail.view.accountId) {
  if (id === "calendar") push({ name: "calendar" });
  else if (id === L.SCHEDULED) {
    mail.view = { accountId, label: mail.view.label };
    push({ name: "scheduled" });
  } else showView(id, accountId);
}

export function pins(): ViewId[] {
  return uiPrefs.mobilePins?.length ? uiPrefs.mobilePins.slice(0, MAX_PINS) : DEFAULT_PINS;
}

export function setPins(list: ViewId[]) {
  void saveUiPrefs({ mobilePins: list.slice(0, MAX_PINS) });
}

// ---------------------------------------------------------------------------
// Swipe actions
// ---------------------------------------------------------------------------

export type SwipeGesture = "shortRight" | "longRight" | "shortLeft" | "longLeft";
export type SwipeAction = "star" | "trash" | "archive" | "read" | "snooze" | "none";

export const DEFAULT_SWIPE: Record<SwipeGesture, SwipeAction> = {
  shortRight: "star",
  longRight: "trash",
  shortLeft: "archive",
  longLeft: "snooze",
};

export const SWIPE_GESTURES: { id: SwipeGesture; name: string }[] = [
  { id: "shortRight", name: "Short swipe right" },
  { id: "longRight", name: "Long swipe right" },
  { id: "shortLeft", name: "Short swipe left" },
  { id: "longLeft", name: "Long swipe left" },
];

export const SWIPE_ACTIONS: { id: SwipeAction; name: string; color: string }[] = [
  { id: "star", name: "Star", color: "var(--m-swipe-star)" },
  { id: "trash", name: "Delete", color: "var(--m-swipe-delete)" },
  { id: "archive", name: "Archive", color: "var(--m-swipe-archive)" },
  { id: "read", name: "Read / unread", color: "var(--m-swipe-read)" },
  { id: "snooze", name: "Snooze", color: "var(--m-swipe-snooze)" },
  { id: "none", name: "Nothing", color: "var(--m-surface-2)" },
];

export function swipeAction(g: SwipeGesture): SwipeAction {
  const v = uiPrefs.swipe?.[g];
  return SWIPE_ACTIONS.some((a) => a.id === v) ? (v as SwipeAction) : DEFAULT_SWIPE[g];
}

export function setSwipe(g: SwipeGesture, a: SwipeAction) {
  const next: Record<string, string> = {};
  for (const x of SWIPE_GESTURES) next[x.id] = swipeAction(x.id);
  next[g] = a;
  void saveUiPrefs({ swipe: next });
}

const PRESET_KEYS: Record<string, MessageKey> = {
  later_today: "m.preset.laterToday",
  "later-today": "m.preset.laterToday",
  afternoon: "m.preset.afternoon",
  evening: "m.preset.evening",
  morning: "m.preset.tomorrowMorning",
  "tomorrow-morning": "m.preset.tomorrowMorning",
  tomorrow: "m.preset.tomorrow",
  day_after: "m.preset.dayAfter",
  this_weekend: "m.preset.weekend",
  next_week: "m.preset.nextWeek",
  "monday-morning": "m.preset.mondayMorning",
};

/** A snooze or send-later preset's name in the app language. */
export const presetName = (p: { id: string; label: string }) => (PRESET_KEYS[p.id] ? t(PRESET_KEYS[p.id]) : p.label);
