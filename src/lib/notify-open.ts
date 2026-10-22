// Clicks on OS notifications (taps on a phone): the app shell parks what the
// notification was about and says `notification-open`; the page takes it with
// `take_notification_open` (see src-tauri/src/notify.rs). It also looks once
// at start, for a tap that launched the app.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { cal, startOfDay } from "./calendar.svelte";
import { key, mail, navigate, openThread, refreshThreads, setSmartTab, showCalendar, smartActive } from "./mail.svelte";
import { L, type ThreadRef } from "./types";

export type NotificationTarget =
  | { kind: "thread"; accountId: string; threadId: string }
  | { kind: "inbox"; accountId: string }
  | { kind: "calendar"; startMs: number };

/** Run `open` for every notification clicked from now on, and for one that
 *  was clicked before (it started the app). Returns the unlisten function. */
export async function onNotificationOpen(open: (t: NotificationTarget) => void): Promise<() => void> {
  const take = async () => {
    const target = await invoke<NotificationTarget | null>("take_notification_open").catch(() => null);
    if (target) open(target);
  };
  const un = await listen("notification-open", () => void take());
  await take();
  return un;
}

/** Show a notification's mail: its conversation open in the reading view, or
 *  the inbox. The unified inbox stays as it is; another account's view
 *  switches to the notification's account. Phones push the thread screen
 *  themselves. */
export function openMailTarget(t: Exclude<NotificationTarget, { kind: "calendar" }>) {
  mail.modal = null;
  const v = mail.view.accountId;
  const accountId = v === null || v === t.accountId ? v : t.accountId;
  if (t.kind === "inbox") {
    navigate(accountId, L.INBOX);
    // New mail lands in Important (or is told of only there).
    if (smartActive()) setSmartTab("important", null);
    return;
  }
  const ref = { accountId: t.accountId, threadId: t.threadId };
  const listed = (mail.page === "list" || mail.page === "search") && mail.threads.some((x) => key(x) === key(ref));
  if (!listed) navigate(accountId, L.INBOX);
  openThread(ref);
  if (!listed) void showRow(ref);
}

/** Once the inbox is in, mark the conversation's row (J/K go on from it).
 *  With Smart Inbox it may be in the other half: switch to that. */
async function showRow(ref: ThreadRef) {
  const open = () => !!mail.open && key(mail.open) === key(ref);
  const row = () => mail.threads.findIndex((x) => key(x) === key(ref));
  await refreshThreads();
  if (!open()) return;
  if (row() < 0 && smartActive()) {
    const was = { tab: mail.smart.tab, other: mail.smart.other };
    const halves = (["important", "other"] as const).filter((h) => h !== was.tab || was.other !== null);
    for (const half of halves) {
      setSmartTab(half, null);
      // The full-width list closes the conversation on a tab switch.
      openThread(ref);
      await refreshThreads();
      if (!open() || row() >= 0) break;
    }
    if (open() && row() < 0) {
      setSmartTab(was.tab, was.other);
      openThread(ref);
      await refreshThreads();
    }
  }
  const i = row();
  if (i >= 0 && open()) mail.focusedIndex = i;
}

/** Desktop: open what a clicked notification is about in the main window. */
export function openNotificationTarget(t: NotificationTarget) {
  if (t.kind === "calendar") {
    mail.modal = null;
    cal.selected = startOfDay(new Date(t.startMs));
    cal.anchor = cal.selected;
    showCalendar();
    return;
  }
  openMailTarget(t);
}
