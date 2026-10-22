import { beforeEach, describe, expect, it, vi } from "vitest";
import type { UiPrefs } from "./types";

let server: UiPrefs | null = null;
const pushed: Partial<UiPrefs>[] = [];
vi.mock("./api", () => ({
  api: {
    uiPrefs: () => Promise.resolve(server),
    setUiPrefs: (p: Partial<UiPrefs>) => {
      pushed.push(p);
      return Promise.resolve();
    },
  },
}));
const { LEGACY_KEYS, migrateLegacyPrefs, normalizePrefs, loadUiPrefs, uiPrefs } = await import("./prefs.svelte");

class MemoryStorage {
  data = new Map<string, string>();
  getItem = (k: string) => this.data.get(k) ?? null;
  setItem = (k: string, v: string) => void this.data.set(k, v);
  removeItem = (k: string) => void this.data.delete(k);
}

const EMPTY: UiPrefs = { shortcuts: {}, sidebarHidden: null, lastComposeAccount: null, mobilePins: null, swipe: null };

describe("UI prefs moving out of localStorage", () => {
  let storage: MemoryStorage;
  beforeEach(() => {
    storage = new MemoryStorage();
    pushed.length = 0;
  });

  it("pushes old values the synced prefs don't have, then deletes them", async () => {
    storage.setItem("shortcuts", JSON.stringify({ archive: ["y"] }));
    storage.setItem("sidebarHidden", JSON.stringify(["SPAM", "folder:acc_1:Receipts"]));
    storage.setItem("lastComposeAccount", "acc_1");
    storage.setItem("sidebarWidth", "260");
    const push = vi.fn((_: Partial<UiPrefs>) => Promise.resolve());
    const out = await migrateLegacyPrefs(EMPTY, storage, push);
    expect(push).toHaveBeenCalledWith({
      shortcuts: { archive: ["y"] },
      sidebarHidden: ["SPAM", "folder:acc_1:Receipts"],
      lastComposeAccount: "acc_1",
    });
    expect(out.lastComposeAccount).toBe("acc_1");
    for (const k of Object.values(LEGACY_KEYS)) expect(storage.getItem(k)).toBeNull();
    // Device-only keys stay.
    expect(storage.getItem("sidebarWidth")).toBe("260");
  });

  it("keeps what another device already saved", async () => {
    storage.setItem("shortcuts", JSON.stringify({ archive: ["y"] }));
    storage.setItem("lastComposeAccount", "acc_old");
    const synced: UiPrefs = { shortcuts: { compose: ["Ctrl+M"] }, sidebarHidden: ["TRASH"], lastComposeAccount: null, mobilePins: null, swipe: null };
    const push = vi.fn((_: Partial<UiPrefs>) => Promise.resolve());
    const out = await migrateLegacyPrefs(synced, storage, push);
    expect(push).toHaveBeenCalledWith({ lastComposeAccount: "acc_old" });
    expect(out).toEqual({ ...synced, lastComposeAccount: "acc_old" });
    expect(storage.getItem("shortcuts")).toBeNull();
  });

  it("does nothing without old keys", async () => {
    const push = vi.fn((_: Partial<UiPrefs>) => Promise.resolve());
    expect(await migrateLegacyPrefs(EMPTY, storage, push)).toBe(EMPTY);
    expect(push).not.toHaveBeenCalled();
  });

  it("keeps the old keys when the push fails", async () => {
    storage.setItem("lastComposeAccount", "acc_1");
    await expect(migrateLegacyPrefs(EMPTY, storage, () => Promise.reject(new Error("offline")))).rejects.toThrow("offline");
    expect(storage.getItem("lastComposeAccount")).toBe("acc_1");
  });

  it("ignores broken old values", async () => {
    storage.setItem("shortcuts", "{not json");
    storage.setItem("sidebarHidden", JSON.stringify({ no: "array" }));
    const push = vi.fn((_: Partial<UiPrefs>) => Promise.resolve());
    await migrateLegacyPrefs(EMPTY, storage, push);
    expect(push).not.toHaveBeenCalled();
    expect(storage.getItem("shortcuts")).toBeNull();
  });

  it("fills gaps in what the backend returned", () => {
    expect(normalizePrefs(null)).toEqual(EMPTY);
    expect(normalizePrefs({ shortcuts: { a: ["x", 3 as unknown as string], b: "y" as unknown as string[] } })).toEqual({ ...EMPTY, shortcuts: { a: ["x"] } });
  });

  it("loads into the store and migrates from the real localStorage", async () => {
    const mem = new MemoryStorage();
    vi.stubGlobal("localStorage", mem);
    mem.setItem("sidebarHidden", JSON.stringify(["SPAM"]));
    server = null;
    await loadUiPrefs({ migrate: true });
    expect(pushed).toEqual([{ sidebarHidden: ["SPAM"] }]);
    expect(uiPrefs.sidebarHidden).toEqual(["SPAM"]);
    expect(uiPrefs.loaded).toBe(true);
    expect(mem.getItem("sidebarHidden")).toBeNull();
    vi.unstubAllGlobals();
  });
});
