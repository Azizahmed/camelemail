// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from "vitest";
import {
  ACTIONS, PREFIX, bindingCaps, bindings, collides, conflicts, eventKey, formatBinding, hint, isDefault, keyHint, matchKey, normalize,
  reloadShortcuts, removeBinding, reservedBy, resetAll, resetBinding, setBinding,
} from "./shortcuts.svelte";

type Mods = { ctrl?: boolean; shift?: boolean; alt?: boolean; meta?: boolean };
const ev = (key: string, m: Mods = {}) => ({ key, ctrlKey: !!m.ctrl, shiftKey: !!m.shift, altKey: !!m.alt, metaKey: !!m.meta });

beforeEach(() => {
  localStorage.clear();
  resetAll();
});

describe("notation", () => {
  it("normalizes bindings", () => {
    expect(normalize("ctrl+k")).toBe("Ctrl+K");
    expect(normalize("Cmd+Shift+p")).toBe("Ctrl+Shift+P");
    expect(normalize("g   i")).toBe("g i");
    expect(normalize("Ctrl+\\")).toBe("Ctrl+\\");
    expect(normalize("Ctrl++")).toBe("Ctrl++");
    expect(normalize(" ")).toBe("Space");
  });

  it("reads key presses", () => {
    expect(eventKey(ev("e"))).toBe("e");
    expect(eventKey(ev("J", { shift: true }))).toBe("J");
    expect(eventKey(ev("#", { shift: true }))).toBe("#");
    expect(eventKey(ev("k", { ctrl: true }))).toBe("Ctrl+K");
    expect(eventKey(ev("k", { meta: true }))).toBe("Ctrl+K");
    expect(eventKey(ev("K", { ctrl: true, shift: true }))).toBe("Ctrl+Shift+K");
    expect(eventKey(ev(" "))).toBe("Space");
    expect(eventKey(ev("ArrowDown", { shift: true }))).toBe("Shift+ArrowDown");
    expect(eventKey(ev("Shift", { shift: true }))).toBeNull();
    expect(eventKey(ev("Control", { ctrl: true }))).toBeNull();
  });

  it("formats for people", () => {
    expect(formatBinding("Ctrl+K")).toBe("Ctrl+K");
    expect(formatBinding("g i")).toBe("G then I");
    expect(formatBinding("J")).toBe("Shift+J");
    expect(formatBinding("e")).toBe("E");
    expect(bindingCaps("Ctrl+\\")).toEqual([["Ctrl", "\\"]]);
    expect(bindingCaps("Shift+ArrowUp")).toEqual([["Shift", "↑"]]);
    expect(hint("archive")).toBe("E");
    expect(hint("palette")).toBe("/");
    expect(hint("go.inbox")).toBe("G then I");
    expect(keyHint("search")).toBe("Ctrl F");
    expect(keyHint("go.inbox")).toBe("G I");
  });
});

describe("matchKey with the default keys", () => {
  it("finds single keys and Ctrl combinations", () => {
    expect(matchKey(ev("e"), null)).toBe("archive");
    expect(matchKey(ev("#", { shift: true }), null)).toBe("trash");
    expect(matchKey(ev("j"), null)).toBe("next");
    expect(matchKey(ev("J", { shift: true }), null)).toBe("extendDown");
    expect(matchKey(ev("k", { ctrl: true }), null)).toBe("palette");
    expect(matchKey(ev("/"), null)).toBe("palette");
    expect(matchKey(ev("\\", { ctrl: true }), null)).toBe("layout");
    expect(matchKey(ev(",", { ctrl: true }), null)).toBe("settings");
    expect(matchKey(ev("n", { ctrl: true }), null)).toBe("compose");
    expect(matchKey(ev("E"), null)).toBeNull();
    expect(matchKey(ev("Enter"), null)).toBeNull();
  });

  it("lets Ctrl+Shift+K mean Ctrl+K", () => {
    expect(matchKey(ev("K", { ctrl: true, shift: true }), null)).toBe("palette");
  });

  it("runs two-key sequences in either case", () => {
    expect(matchKey(ev("g"), null)).toBe(PREFIX);
    expect(matchKey(ev("G", { shift: true }), null)).toBe(PREFIX);
    expect(matchKey(ev("i"), "g")).toBe("go.inbox");
    expect(matchKey(ev("I", { shift: true }), "g")).toBe("go.inbox");
    expect(matchKey(ev("c"), "G")).toBe("go.calendar");
    expect(matchKey(ev("z"), "g")).toBeNull();
  });

  it("only offers app-wide Ctrl shortcuts while typing", () => {
    expect(matchKey(ev("e"), null, { typing: true })).toBeNull();
    expect(matchKey(ev("c"), null, { typing: true })).toBeNull();
    expect(matchKey(ev("f", { ctrl: true }), null, { typing: true })).toBe("search");
    expect(matchKey(ev("n", { ctrl: true }), null, { typing: true })).toBe("compose");
  });

  it("limits the calendar to its subset", () => {
    const only = (a: (typeof ACTIONS)[number]) => !!a.calendar;
    expect(matchKey(ev("c"), null, { only })).toBe("compose");
    expect(matchKey(ev("?"), null, { only })).toBe("help");
    expect(matchKey(ev("e"), null, { only })).toBeNull();
    expect(matchKey(ev("j", { ctrl: true }), null, { only })).toBeNull();
    expect(matchKey(ev("i"), "g", { only })).toBe("go.inbox");
  });
});

describe("changing keys", () => {
  it("rebinds, persists and resets", () => {
    setBinding("archive", "y", "e");
    expect(bindings("archive")).toEqual(["y"]);
    expect(isDefault("archive")).toBe(false);
    expect(JSON.parse(localStorage.getItem("shortcuts")!)).toEqual({ archive: ["y"] });
    expect(matchKey(ev("y"), null)).toBe("archive");
    expect(matchKey(ev("e"), null)).toBeNull();
    resetBinding("archive");
    expect(bindings("archive")).toEqual(["e"]);
    expect(localStorage.getItem("shortcuts")).toBeNull();
  });

  it("adds a second key and removes one", () => {
    setBinding("archive", "Ctrl+E");
    expect(bindings("archive")).toEqual(["e", "Ctrl+E"]);
    removeBinding("archive", "e");
    expect(bindings("archive")).toEqual(["Ctrl+E"]);
    removeBinding("archive", "Ctrl+E");
    expect(bindings("archive")).toEqual([]);
    expect(matchKey(ev("e"), null)).toBeNull();
  });

  it("takes a key away from the action that had it", () => {
    const taken = setBinding("archive", "s");
    expect(taken).toEqual([{ id: "star", label: "Star / Unstar", binding: "s" }]);
    expect(bindings("star")).toEqual([]);
    expect(bindings("archive")).toEqual(["e", "s"]);
  });

  it("treats a key that starts a sequence as a conflict", () => {
    expect(collides("g", "g i")).toBe(true);
    expect(collides("G", "g i")).toBe(false);
    expect(collides("g i", "G I")).toBe(true);
    expect(collides("g i", "g s")).toBe(false);
    expect(conflicts("g").map((c) => c.id)).toContain("go.inbox");
    setBinding("undo", "g");
    expect(bindings("go.inbox")).toEqual([]);
    expect(matchKey(ev("g"), null)).toBe("undo");
  });

  it("gives defaults back on reset, taking them from whoever has them", () => {
    setBinding("star", "e");
    expect(bindings("archive")).toEqual([]);
    const taken = resetBinding("archive");
    expect(taken.map((t) => t.id)).toEqual(["star"]);
    expect(bindings("archive")).toEqual(["e"]);
  });

  it("knows the keys that can't be used", () => {
    expect(reservedBy("Enter")).toBe("Open focused thread");
    expect(reservedBy("Ctrl+a")).toBe("Select all threads");
    expect(reservedBy("Ctrl+3")).toBe("Account 3 inbox");
    expect(reservedBy("Escape x")).toBe("Go back / Close");
    expect(reservedBy("e")).toBeNull();
  });

  it("follows changes saved by another window", () => {
    localStorage.setItem("shortcuts", JSON.stringify({ compose: ["Ctrl+M"], bogus: ["x"], archive: "nope" }));
    reloadShortcuts();
    expect(bindings("compose")).toEqual(["Ctrl+M"]);
    expect(bindings("archive")).toEqual(["e"]);
    expect(matchKey(ev("c"), null)).toBeNull();
    expect(matchKey(ev("m", { ctrl: true }), null)).toBe("compose");
  });

  it("has unique ids and conflict-free defaults", () => {
    const ids = ACTIONS.map((a) => a.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const a of ACTIONS) for (const b of a.defaults) expect(conflicts(b, a.id)).toEqual([]);
  });
});

describe("other keyboard layouts", () => {
  it("reads Arabic-layout letters by their key position", async () => {
    const { eventKey } = await import("./shortcuts.svelte");
    const ev = (key: string, code: string, extra: Partial<KeyboardEvent> = {}) => ({ key, code, ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...extra });
    expect(eventKey(ev("ث", "KeyE"))).toBe("e");
    expect(eventKey(ev("ؤ", "KeyC", { ctrlKey: true }))).toBe("Ctrl+C");
    expect(eventKey(ev("e", "KeyE"))).toBe("e");
  });
});
