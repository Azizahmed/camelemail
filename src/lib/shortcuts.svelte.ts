// Keyboard shortcuts: the actions you can rebind, their default keys, and the
// keys that stay fixed. Changes are saved in localStorage (shared by every
// window), so the Settings window edits them and the main window follows live.
//
// A binding is a string: a single key as `KeyboardEvent.key` ("e", "#", "J",
// "?"), a combination like "Ctrl+K" or "Ctrl+\\", or two keys pressed one after
// the other, separated by a space: "g i".

export type ShortcutGroup = "Navigation" | "Actions" | "Selection" | "Calendar" | "Windows & Search";

export const GROUPS: readonly ShortcutGroup[] = ["Navigation", "Actions", "Selection", "Calendar", "Windows & Search"];

export interface ShortcutAction {
  id: string;
  label: string;
  group: ShortcutGroup;
  defaults: readonly string[];
  /** Fires from a text field too (only through a Ctrl combination). */
  global?: boolean;
  /** Also works while the calendar is shown. */
  calendar?: boolean;
}

/** A key that can't be changed; listed so the shortcut list is complete. */
export interface FixedShortcut {
  label: string;
  group: ShortcutGroup;
  /** Alternatives, each in binding notation (ranges like "Ctrl+1–9" are for display only). */
  keys: readonly string[];
}

const GO: [string, string, string][] = [
  ["i", "inbox", "Inbox"],
  ["s", "starred", "Starred"],
  ["t", "sent", "Sent"],
  ["d", "drafts", "Drafts"],
  ["w", "snoozed", "Snoozed"],
  ["a", "archive", "Archive"],
  ["u", "unread", "Unread"],
  ["r", "trash", "Trash"],
  ["p", "spam", "Spam"],
];

export const ACTIONS: readonly ShortcutAction[] = [
  ...GO.map(([k, id, name]): ShortcutAction => ({ id: `go.${id}`, label: `Go to ${name}`, group: "Navigation", defaults: [`g ${k}`], calendar: true })),
  { id: "go.calendar", label: "Go to Calendar", group: "Navigation", defaults: ["g c"], calendar: true },
  { id: "next", label: "Next thread", group: "Navigation", defaults: ["j"] },
  { id: "prev", label: "Previous thread", group: "Navigation", defaults: ["k"] },
  { id: "openFirst", label: "Open first thread", group: "Navigation", defaults: ["o"] },
  { id: "sidebar", label: "Toggle sidebar", group: "Navigation", defaults: ["["], calendar: true },
  { id: "layout", label: "Split view / full-width list", group: "Navigation", defaults: ["Ctrl+\\"], global: true },

  { id: "compose", label: "Compose new email", group: "Actions", defaults: ["c", "Ctrl+N"], global: true, calendar: true },
  { id: "reply", label: "Reply", group: "Actions", defaults: ["r"] },
  { id: "replyAll", label: "Reply all", group: "Actions", defaults: ["a"] },
  { id: "forward", label: "Forward", group: "Actions", defaults: ["f"] },
  { id: "archive", label: "Archive", group: "Actions", defaults: ["e"] },
  { id: "snooze", label: "Snooze", group: "Actions", defaults: ["w"] },
  { id: "move", label: "Move to folder", group: "Actions", defaults: ["m"] },
  { id: "trash", label: "Move to Trash", group: "Actions", defaults: ["#"] },
  { id: "spam", label: "Mark as spam", group: "Actions", defaults: ["!"] },
  { id: "star", label: "Star / Unstar", group: "Actions", defaults: ["s"] },
  { id: "unread", label: "Mark as unread", group: "Actions", defaults: ["u"] },
  { id: "undo", label: "Undo last action", group: "Actions", defaults: ["z"] },
  { id: "assistant", label: "AI assistant", group: "Actions", defaults: ["Ctrl+J"], global: true },

  { id: "select", label: "Select / deselect focused thread", group: "Selection", defaults: ["x"] },
  { id: "extendDown", label: "Extend selection down", group: "Selection", defaults: ["J"] },
  { id: "extendUp", label: "Extend selection up", group: "Selection", defaults: ["K"] },
  { id: "selectAll", label: "Toggle select all", group: "Selection", defaults: ["*"] },

  { id: "search", label: "Search", group: "Windows & Search", defaults: ["Ctrl+F"], global: true },
  { id: "palette", label: "Command palette", group: "Windows & Search", defaults: ["/", "Ctrl+K"], global: true, calendar: true },
  { id: "sync", label: "Sync now", group: "Windows & Search", defaults: ["Ctrl+R"], global: true },
  { id: "settings", label: "Settings", group: "Windows & Search", defaults: ["Ctrl+,"], global: true, calendar: true },
  { id: "help", label: "Show keyboard shortcuts", group: "Windows & Search", defaults: ["?"], calendar: true },
];

export const FIXED: readonly FixedShortcut[] = [
  { label: "Next / previous thread", group: "Navigation", keys: ["ArrowDown", "ArrowUp"] },
  { label: "Open focused thread", group: "Navigation", keys: ["Enter"] },
  { label: "Go back / Close", group: "Navigation", keys: ["Escape"] },
  { label: "All accounts inbox", group: "Navigation", keys: ["Ctrl+0"] },
  { label: "Account 1–9 inbox", group: "Navigation", keys: ["Ctrl+1–9"] },
  { label: "Move to Trash", group: "Actions", keys: ["Delete", "Backspace"] },
  { label: "Send (in compose)", group: "Actions", keys: ["Ctrl+Enter"] },
  { label: "Select / deselect focused thread", group: "Selection", keys: ["Space"] },
  { label: "Extend selection", group: "Selection", keys: ["Shift+ArrowDown", "Shift+ArrowUp"] },
  { label: "Select all threads", group: "Selection", keys: ["Ctrl+A"] },
  { label: "Cancel selection", group: "Selection", keys: ["Escape"] },
  { label: "Today", group: "Calendar", keys: ["t"] },
  { label: "Previous / next", group: "Calendar", keys: ["ArrowLeft", "ArrowRight", "k", "j"] },
  { label: "Day, week, month, quarter, year", group: "Calendar", keys: ["d", "w", "m", "q", "y"] },
  { label: "Switch view", group: "Calendar", keys: ["Ctrl+1–5"] },
  { label: "New event", group: "Calendar", keys: ["n"] },
  { label: "Search events", group: "Calendar", keys: ["Ctrl+F"] },
  { label: "Sync calendars", group: "Calendar", keys: ["Ctrl+R"] },
];

/** Keys the main window keeps for itself, with what they do. */
const RESERVED = new Map<string, string>([
  ["Escape", "Go back / Close"],
  ["Enter", "Open focused thread"],
  ["Space", "Select / deselect focused thread"],
  ["ArrowDown", "Next thread"],
  ["ArrowUp", "Previous thread"],
  ["Shift+ArrowDown", "Extend selection"],
  ["Shift+ArrowUp", "Extend selection"],
  ["Delete", "Move to Trash"],
  ["Backspace", "Move to Trash"],
  ["Ctrl+A", "Select all threads"],
  ["Ctrl+Enter", "Send (in compose)"],
  ["Tab", "Move focus"],
  ["Shift+Tab", "Move focus"],
  ["Ctrl+0", "All accounts inbox"],
  ...Array.from({ length: 9 }, (_, i): [string, string] => [`Ctrl+${i + 1}`, `Account ${i + 1} inbox`]),
]);

/** Keys the calendar uses (app shortcuts win over them there). */
const CALENDAR_KEYS = new Map<string, string>([
  ["t", "Today"],
  ["n", "New event"],
  ["d", "Day view"],
  ["w", "Week view"],
  ["m", "Month view"],
  ["q", "Quarter view"],
  ["y", "Year view"],
  ["j", "Next"],
  ["k", "Previous"],
  ["ArrowLeft", "Previous"],
  ["ArrowRight", "Next"],
  ["Ctrl+F", "Search events"],
  ["Ctrl+R", "Sync calendars"],
  ...Array.from({ length: 5 }, (_, i): [string, string] => [`Ctrl+${i + 1}`, "Switch view"]),
]);

/** How long the first key of a two-key shortcut waits for the second. */
export const PREFIX_TIMEOUT = 1500;
/** What `matchKey` returns for the first key of a two-key shortcut. */
export const PREFIX = "prefix";

const STORAGE_KEY = "shortcuts";
const byId = new Map(ACTIONS.map((a) => [a.id, a]));

// ---------------------------------------------------------------------------
// Binding notation
// ---------------------------------------------------------------------------

const MODIFIER_KEYS = new Set(["Control", "Shift", "Alt", "Meta", "AltGraph", "OS", "Super", "Hyper", "CapsLock", "Fn"]);

/** A modifier pressed on its own (not a shortcut yet). */
export const isModifierKey = (key: string) => MODIFIER_KEYS.has(key);

interface Step {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  key: string;
}

function parseStep(s: string): Step {
  const step: Step = { ctrl: false, alt: false, shift: false, key: s };
  for (;;) {
    const m = /^(ctrl|control|cmd|meta|alt|shift)\+(.+)$/i.exec(step.key);
    if (!m) break;
    const mod = m[1].toLowerCase();
    if (mod === "alt") step.alt = true;
    else if (mod === "shift") step.shift = true;
    else step.ctrl = true;
    step.key = m[2];
  }
  if (step.key === " ") step.key = "Space";
  return step;
}

function stepString({ ctrl, alt, shift, key }: Step): string {
  // With Ctrl or Alt held the letter's case says nothing; Shift does.
  const k = (ctrl || alt) && key.length === 1 ? key.toUpperCase() : key;
  return (ctrl ? "Ctrl+" : "") + (alt ? "Alt+" : "") + (shift ? "Shift+" : "") + k;
}

/** The binding's steps (one, or two for a sequence). */
const stepsOf = (binding: string) => binding.trim().split(/\s+/).filter(Boolean);

/** Canonical spelling: "ctrl+k" becomes "Ctrl+K", "g  i" becomes "g i". */
export function normalize(binding: string): string {
  if (binding === " ") return "Space";
  return stepsOf(binding).map((s) => stepString(parseStep(s))).join(" ");
}

/** The binding step a key press makes, or null for a lone modifier. */
export function eventKey(e: Pick<KeyboardEvent, "key" | "ctrlKey" | "metaKey" | "altKey" | "shiftKey"> & { code?: string }): string | null {
  if (!e.key || isModifierKey(e.key) || e.key === "Dead" || e.key === "Unidentified") return null;
  let key = e.key === " " ? "Space" : e.key;
  // Another keyboard layout (Arabic) prints its own letter: use the key's place on a US keyboard.
  if (key.length === 1 && !/[\x20-\x7e]/.test(key) && e.code) {
    const m = /^(?:Key([A-Z])|Digit([0-9]))$/.exec(e.code);
    if (m) key = m[1] ? (e.shiftKey && !(e.ctrlKey || e.metaKey) ? m[1] : m[1].toLowerCase()) : m[2];
  }
  const ctrl = e.ctrlKey || e.metaKey;
  // A printed character already shows Shift ("J", "#"); named keys and Ctrl combinations don't.
  const shift = e.shiftKey && (key.length > 1 || ctrl || e.altKey);
  return stepString({ ctrl, alt: e.altKey, shift, key });
}

/** Steps of a sequence match whatever the letter case ("G I" works as "g i"). */
const sameStep = (a: string, b: string) => a === b || (a.length === 1 && b.length === 1 && a.toLowerCase() === b.toLowerCase());

const hasCtrl = (step: string) => step.startsWith("Ctrl+");

// ---------------------------------------------------------------------------
// The keymap
// ---------------------------------------------------------------------------

function load(): Record<string, string[]> {
  try {
    const raw = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}");
    const out: Record<string, string[]> = {};
    if (raw && typeof raw === "object")
      for (const [id, list] of Object.entries(raw))
        if (byId.has(id) && Array.isArray(list)) out[id] = list.filter((b): b is string => typeof b === "string" && b.trim() !== "").map(normalize);
    return out;
  } catch {
    return {};
  }
}

/** Only the actions you changed; everything else uses its defaults. */
const keys = $state<{ overrides: Record<string, string[]> }>({ overrides: typeof localStorage === "undefined" ? {} : load() });

function save() {
  const o = $state.snapshot(keys.overrides);
  if (Object.keys(o).length === 0) localStorage.removeItem(STORAGE_KEY);
  else localStorage.setItem(STORAGE_KEY, JSON.stringify(o));
}

/** Pick up changes saved by another window (the Settings window edits, the main window follows). */
export function reloadShortcuts() {
  keys.overrides = load();
}

if (typeof window !== "undefined") {
  window.addEventListener("storage", (e) => {
    if (e.key === STORAGE_KEY || e.key === null) reloadShortcuts();
  });
  // A safety net in case a webview doesn't deliver the storage event.
  window.addEventListener("focus", reloadShortcuts);
}

export const action = (id: string) => byId.get(id);

export function defaults(id: string): readonly string[] {
  return byId.get(id)?.defaults ?? [];
}

/** The keys that run an action right now. */
export function bindings(id: string): readonly string[] {
  return keys.overrides[id] ?? defaults(id);
}

export function isDefault(id: string): boolean {
  const cur = bindings(id);
  const def = defaults(id);
  return cur.length === def.length && cur.every((b, i) => b === def[i]);
}

/** Replace an action's keys as they are (no conflict handling). */
export function setBindings(id: string, list: readonly string[]) {
  if (!byId.has(id)) return;
  const next = [...new Set(list.map(normalize).filter(Boolean))];
  const def = defaults(id);
  if (next.length === def.length && next.every((b, i) => b === def[i])) delete keys.overrides[id];
  else keys.overrides[id] = next;
  save();
}

/** Do two bindings get in each other's way (the same keys, or one starts the other)? */
export function collides(a: string, b: string): boolean {
  const sa = stepsOf(a);
  const sb = stepsOf(b);
  if (sa.length === 1 && sb.length === 1) return sa[0] === sb[0];
  // A single key that is the first step of a sequence would hide it.
  if (sa.length === 1) return sa[0] === sb[0];
  if (sb.length === 1) return sb[0] === sa[0];
  return sa.length === sb.length && sa.every((s, i) => sameStep(s, sb[i]));
}

/** Other actions whose keys collide with `binding`. */
export function conflicts(binding: string, exceptId?: string): { id: string; label: string; binding: string }[] {
  const b = normalize(binding);
  const out: { id: string; label: string; binding: string }[] = [];
  for (const a of ACTIONS) {
    if (a.id === exceptId) continue;
    for (const other of bindings(a.id)) if (collides(b, other)) out.push({ id: a.id, label: a.label, binding: other });
  }
  return out;
}

/** What a fixed key does, when `binding` would take it over (its first key is what counts). */
export function reservedBy(binding: string): string | null {
  const first = stepsOf(normalize(binding))[0];
  return (first && RESERVED.get(first)) ?? null;
}

/** What the calendar does with this key (app shortcuts win there), for actions that work in the calendar. */
export function calendarClash(id: string, binding: string): string | null {
  if (!byId.get(id)?.calendar) return null;
  const first = stepsOf(normalize(binding))[0];
  return (first && CALENDAR_KEYS.get(first)) ?? null;
}

/**
 * Give `id` the keys `binding` (in place of `replace`, if given), taking them
 * away from any other action that had them. Returns what was taken away.
 */
export function setBinding(id: string, binding: string, replace?: string | null) {
  const b = normalize(binding);
  if (!b || !byId.has(id)) return [];
  const old = replace ? normalize(replace) : null;
  const taken = conflicts(b, id);
  for (const c of taken) setBindings(c.id, bindings(c.id).filter((x) => x !== c.binding));
  const cur = bindings(id).filter((x) => x === old || !collides(b, x));
  const at = old ? cur.indexOf(old) : -1;
  setBindings(id, at >= 0 ? cur.map((x, i) => (i === at ? b : x)) : [...cur, b]);
  return taken;
}

export function removeBinding(id: string, binding: string) {
  setBindings(id, bindings(id).filter((b) => b !== normalize(binding)));
}

export function resetBinding(id: string) {
  const taken = defaults(id).flatMap((b) => conflicts(b, id));
  for (const c of taken) setBindings(c.id, bindings(c.id).filter((x) => x !== c.binding));
  delete keys.overrides[id];
  save();
  return taken;
}

export function resetAll() {
  keys.overrides = {};
  save();
}

// ---------------------------------------------------------------------------
// Matching key presses
// ---------------------------------------------------------------------------

export interface MatchOptions {
  /** Focus is in a text field: only Ctrl combinations of app-wide actions count. */
  typing?: boolean;
  /** Only these actions are in play (e.g. the calendar's subset). */
  only?: (a: ShortcutAction) => boolean;
}

/**
 * Which action a key press runs. `pending` is the first key of a sequence
 * pressed just before (from an earlier `PREFIX` result), or null. Returns an
 * action id, `PREFIX` when this key starts a sequence, or null.
 */
export function matchKey(
  e: Pick<KeyboardEvent, "key" | "ctrlKey" | "metaKey" | "altKey" | "shiftKey">,
  pending: string | null,
  opts: MatchOptions = {},
): string | null {
  const step = eventKey(e);
  if (!step) return null;
  // Ctrl+Shift+K still means Ctrl+K unless Ctrl+Shift+K is bound itself.
  const loose = step.startsWith("Ctrl+") && step.includes("Shift+") ? step.replace("Shift+", "") : null;
  const candidates: [ShortcutAction, string[]][] = [];
  for (const a of ACTIONS) {
    if (opts.only && !opts.only(a)) continue;
    if (opts.typing && !a.global) continue;
    for (const b of bindings(a.id)) {
      const steps = stepsOf(b);
      if (opts.typing && !steps.every(hasCtrl)) continue;
      candidates.push([a, steps]);
    }
  }
  if (pending) {
    for (const [a, s] of candidates) if (s.length === 2 && sameStep(s[0], pending) && sameStep(s[1], step)) return a.id;
    if (loose) for (const [a, s] of candidates) if (s.length === 2 && sameStep(s[0], pending) && s[1] === loose) return a.id;
    return null;
  }
  for (const [a, s] of candidates) if (s.length === 1 && s[0] === step) return a.id;
  for (const [, s] of candidates) if (s.length === 2 && sameStep(s[0], step)) return PREFIX;
  if (loose) {
    for (const [a, s] of candidates) if (s.length === 1 && s[0] === loose) return a.id;
    for (const [, s] of candidates) if (s.length === 2 && s[0] === loose) return PREFIX;
  }
  return null;
}

// ---------------------------------------------------------------------------
// Showing keys
// ---------------------------------------------------------------------------

const KEY_NAMES: Record<string, string> = {
  Escape: "Esc",
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
  Space: "Space",
  Delete: "Delete",
  Backspace: "Backspace",
  Enter: "Enter",
};

/** One step as key caps: "Ctrl+K" → ["Ctrl", "K"], "J" → ["Shift", "J"]. */
function stepCaps(s: string): string[] {
  const { ctrl, alt, shift, key } = parseStep(s);
  const caps: string[] = [];
  if (ctrl) caps.push("Ctrl");
  if (alt) caps.push("Alt");
  // An upper-case letter on its own is typed with Shift.
  if (shift || (!ctrl && !alt && key.length === 1 && key !== key.toLowerCase())) caps.push("Shift");
  caps.push(KEY_NAMES[key] ?? (key.length === 1 ? key.toUpperCase() : key));
  return caps;
}

/** A binding as key caps per step: "g i" → [["G"], ["I"]], "Ctrl+K" → [["Ctrl", "K"]]. */
export function bindingCaps(binding: string): string[][] {
  return stepsOf(binding === " " ? "Space" : binding).map(stepCaps);
}

/** A binding for reading: "Ctrl+K", "G then I", "Shift+J". */
export function formatBinding(binding: string): string {
  return bindingCaps(binding)
    .map((caps) => caps.join("+"))
    .join(" then ");
}

/** The first key of an action for a button title, e.g. "E", "Ctrl+K", "G then I" ("" when unbound). */
export function hint(id: string): string {
  const b = bindings(id)[0];
  return b ? formatBinding(b) : "";
}

/** `label` with the action's first key in brackets: "Archive (E)". */
export function titled(label: string, id: string): string {
  const h = hint(id);
  return h ? `${label} (${h})` : label;
}

/** A compact hint for a single key cap, like the command palette's: "Ctrl K", "G I". */
export function keyHint(id: string): string {
  const b = bindings(id)[0];
  return b
    ? bindingCaps(b)
        .map((caps) => caps.join(" "))
        .join(" ")
    : "";
}
