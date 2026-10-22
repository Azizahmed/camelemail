// UI preferences that follow the user to every device: shortcut overrides,
// the sidebar entries tucked under "More", and the account last composed
// from. They live in the synced settings (`ui_prefs` / `set_ui_prefs`); they
// used to be in localStorage, and `loadUiPrefs({ migrate: true })` moves them
// up once. Device-only keys (sidebarMore, sidebarDocked, sidebarWidth,
// listWidth) stay in localStorage.

import { api } from "./api";
import type { UiPrefs } from "./types";

/** localStorage keys that moved to `ui_prefs`. */
export const LEGACY_KEYS = { shortcuts: "shortcuts", sidebarHidden: "sidebarHidden", lastComposeAccount: "lastComposeAccount" } as const;

export const uiPrefs = $state<UiPrefs & { loaded: boolean }>({
  shortcuts: {},
  sidebarHidden: null,
  lastComposeAccount: null,
  mobilePins: null,
  swipe: null,
  loaded: false,
});

type Listener = (p: UiPrefs) => void;
const listeners = new Set<Listener>();

/** Run `fn` whenever prefs are (re)loaded from the synced settings. */
export function onUiPrefs(fn: Listener): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** Fill gaps in what `ui_prefs` returned (an older or empty answer). */
export function normalizePrefs(p: Partial<UiPrefs> | null | undefined): UiPrefs {
  const shortcuts: Record<string, string[]> = {};
  if (p?.shortcuts && typeof p.shortcuts === "object")
    for (const [id, list] of Object.entries(p.shortcuts)) if (Array.isArray(list)) shortcuts[id] = list.filter((b) => typeof b === "string");
  return {
    shortcuts,
    sidebarHidden: Array.isArray(p?.sidebarHidden) ? p.sidebarHidden.filter((x) => typeof x === "string") : null,
    lastComposeAccount: typeof p?.lastComposeAccount === "string" && p.lastComposeAccount ? p.lastComposeAccount : null,
    mobilePins: Array.isArray(p?.mobilePins) ? p.mobilePins.filter((x) => typeof x === "string") : null,
    swipe:
      p?.swipe && typeof p.swipe === "object" && !Array.isArray(p.swipe)
        ? Object.fromEntries(Object.entries(p.swipe).filter(([, v]) => typeof v === "string"))
        : null,
  };
}

type KeyValueStore = Pick<Storage, "getItem" | "removeItem">;

function readLegacy(storage: KeyValueStore): Partial<UiPrefs> {
  const out: Partial<UiPrefs> = {};
  const json = (key: string) => {
    try {
      return JSON.parse(storage.getItem(key) ?? "null") as unknown;
    } catch {
      return null;
    }
  };
  const shortcuts = json(LEGACY_KEYS.shortcuts);
  if (shortcuts && typeof shortcuts === "object" && !Array.isArray(shortcuts)) {
    const n = normalizePrefs({ shortcuts: shortcuts as Record<string, string[]> }).shortcuts;
    if (Object.keys(n).length > 0) out.shortcuts = n;
  }
  const hidden = json(LEGACY_KEYS.sidebarHidden);
  if (Array.isArray(hidden)) out.sidebarHidden = hidden.filter((x): x is string => typeof x === "string");
  const last = storage.getItem(LEGACY_KEYS.lastComposeAccount);
  if (last) out.lastComposeAccount = last;
  return out;
}

/**
 * One-time move from localStorage: values the synced prefs don't have yet
 * (empty shortcuts, null sidebarHidden / lastComposeAccount) are pushed up,
 * then the old keys are deleted. If the push fails the keys stay for next time.
 * Returns the prefs to use.
 */
export async function migrateLegacyPrefs(
  server: UiPrefs,
  storage: KeyValueStore,
  push: (patch: Partial<UiPrefs>) => Promise<void>,
): Promise<UiPrefs> {
  const legacy = readLegacy(storage);
  const hasLegacy = Object.values(LEGACY_KEYS).some((k) => storage.getItem(k) !== null);
  if (!hasLegacy) return server;
  const patch: Partial<UiPrefs> = {};
  if (legacy.shortcuts && Object.keys(server.shortcuts).length === 0) patch.shortcuts = legacy.shortcuts;
  if (legacy.sidebarHidden && server.sidebarHidden === null) patch.sidebarHidden = legacy.sidebarHidden;
  if (legacy.lastComposeAccount && server.lastComposeAccount === null) patch.lastComposeAccount = legacy.lastComposeAccount;
  if (Object.keys(patch).length > 0) await push(patch);
  for (const k of Object.values(LEGACY_KEYS)) storage.removeItem(k);
  return { ...server, ...patch };
}

function apply(p: UiPrefs) {
  uiPrefs.shortcuts = p.shortcuts;
  uiPrefs.sidebarHidden = p.sidebarHidden;
  uiPrefs.lastComposeAccount = p.lastComposeAccount;
  uiPrefs.mobilePins = p.mobilePins;
  uiPrefs.swipe = p.swipe;
  uiPrefs.loaded = true;
  for (const fn of listeners) fn(p);
}

/**
 * Load the synced prefs (again: after `settingsChanged`, or when a window gets
 * focus). `migrate` (the main window) moves old localStorage values up first.
 */
export async function loadUiPrefs(opts: { migrate?: boolean } = {}): Promise<UiPrefs> {
  let p = normalizePrefs(await api.uiPrefs());
  if (opts.migrate && typeof localStorage !== "undefined") {
    try {
      p = await migrateLegacyPrefs(p, localStorage, (patch) => api.setUiPrefs(patch));
    } catch (e) {
      console.warn("Couldn't move UI prefs from localStorage", e);
    }
  }
  apply(p);
  return p;
}

/** Change some prefs here and in the synced settings. */
export function saveUiPrefs(patch: Partial<UiPrefs>): Promise<void> {
  if (patch.shortcuts !== undefined) uiPrefs.shortcuts = patch.shortcuts;
  if (patch.sidebarHidden !== undefined) uiPrefs.sidebarHidden = patch.sidebarHidden;
  if (patch.lastComposeAccount !== undefined) uiPrefs.lastComposeAccount = patch.lastComposeAccount;
  if (patch.mobilePins !== undefined) uiPrefs.mobilePins = patch.mobilePins;
  if (patch.swipe !== undefined) uiPrefs.swipe = patch.swipe;
  return api.setUiPrefs(patch).catch((e) => {
    console.warn("Couldn't save UI prefs", e);
  });
}
