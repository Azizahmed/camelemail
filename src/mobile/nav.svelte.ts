// Phone navigation: a stack of screens over the mail list. Each push adds a
// browser history entry, so Android's back button/gesture (which goes back in
// the web view while it can) closes the top screen, sheet or drawer.

import { closeThread } from "../lib/mail.svelte";
import { dismissStaleTextMenu } from "../lib/platform";

export type Screen =
  | { name: "thread" }
  | { name: "compose"; draftId: string }
  | { name: "search" }
  | { name: "settings" }
  | { name: "settingsPage"; section: string }
  | { name: "calendar" }
  | { name: "assistant" }
  | { name: "scheduled" }
  | { name: "unsubscribe" }
  /** Overlays: a drawer or a sheet (closed by back too). */
  | { name: "drawer" }
  | { name: "sheet"; id: string };

class Nav {
  // Raw: screens are compared by identity (guards, leave hooks).
  stack = $state.raw<Screen[]>([]);
}

export const nav = new Nav();

/** Asked before a screen is popped by back; false keeps it (e.g. compose's discard question). */
const guards = new Map<Screen, () => Promise<boolean>>();
/** Run when a screen leaves the stack. */
const leaves = new Map<Screen, () => void>();

export const top = (): Screen | undefined => nav.stack[nav.stack.length - 1];
export const isOpen = (name: Screen["name"]) => nav.stack.some((s) => s.name === name);

export function push(s: Screen) {
  nav.stack = [...nav.stack, s];
  history.pushState({ depth: nav.stack.length }, "");
}

/** Replace the top screen (no new history entry). */
export function replaceTop(s: Screen) {
  if (nav.stack.length === 0) return push(s);
  nav.stack = [...nav.stack.slice(0, -1), s];
}

let settled: (() => void)[] = [];

/** Close the top screen (as the back button would). Resolves once it's gone,
 *  so the next push lands after it in the history. */
export function back(): Promise<void> {
  if (nav.stack.length - wanted - inFlight <= 0) return Promise.resolve();
  wanted++;
  return new Promise((resolve) => {
    settled.push(resolve);
    queueMicrotask(kick);
  });
}

/** Steps back still to take, and steps of the traversal under way. Back
 *  steps asked for together go as one traversal (browsers may merge or drop
 *  traversals started while one is running). */
let wanted = 0;
let inFlight = 0;

function kick() {
  if (inFlight || !wanted) return;
  inFlight = wanted;
  wanted = 0;
  history.go(-inFlight);
}

/** Close screens until `name` is gone (or everything with no name). */
export function popTo(depth: number) {
  const n = nav.stack.length - depth - wanted - inFlight;
  for (let i = 0; i < n; i++) void back();
}

export function guard(s: Screen, fn: () => Promise<boolean>) {
  guards.set(s, fn);
  return () => guards.delete(s);
}

export function onLeave(s: Screen, fn: () => void) {
  leaves.set(s, fn);
  return () => leaves.delete(s);
}

function drop(count: number) {
  const gone = nav.stack.slice(nav.stack.length - count);
  nav.stack = nav.stack.slice(0, nav.stack.length - count);
  // A selection left on a closed screen would keep Android's text toolbar up.
  window.getSelection()?.removeAllRanges();
  dismissStaleTextMenu();
  for (const s of gone) {
    leaves.get(s)?.();
    leaves.delete(s);
    guards.delete(s);
    if (s.name === "thread") closeThread();
  }
}

export function initNav() {
  history.replaceState({ depth: 0 }, "");
  window.addEventListener("popstate", async (e) => {
    const depth = typeof e.state?.depth === "number" ? e.state.depth : 0;
    const count = nav.stack.length - depth;
    if (count <= 0) return finish();
    const topScreen = top();
    const g = topScreen && count === 1 ? guards.get(topScreen) : undefined;
    if (g && !(await g())) {
      // Stay: put the history entry back (pushState fires no popstate).
      history.pushState({ depth: nav.stack.length }, "");
      return finish();
    }
    drop(count);
    finish();
  });
}

function finish() {
  inFlight = 0;
  if (wanted) return kick();
  const waiting = settled;
  settled = [];
  for (const r of waiting) r();
}
