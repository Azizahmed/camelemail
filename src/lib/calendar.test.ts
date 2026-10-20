import { describe, expect, it, vi } from "vitest";

// ui.svelte touches `window` when loaded; these helpers don't need it.
vi.mock("./ui.svelte", () => ({ confirmDialog: vi.fn(), toast: vi.fn() }));

const { dayKey, eventsByDay, temp } = await import("./calendar.svelte");
type CalEvent = import("./calendar.svelte").CalEvent;

const at = (y: number, m: number, d: number, h = 0, min = 0) => new Date(y, m - 1, d, h, min).getTime();

function ev(key: string, startMs: number, endMs: number, allDay = false): CalEvent {
  return {
    key,
    calendarId: "c",
    href: key,
    uid: key,
    recurrenceId: null,
    startMs,
    endMs,
    allDay,
    recurring: false,
    summary: key,
    location: "",
    description: "",
    url: "",
    status: "",
    alarmsMinutesBefore: [],
    organizer: null,
    attendees: [],
    rrule: null,
    color: "",
    readOnly: false,
  };
}

const keys = (m: Map<string, CalEvent[]>) => Object.fromEntries([...m].map(([k, v]) => [k, v.map((e) => e.key)]));

describe("dayKey", () => {
  it("uses the local date, zero-padded", () => {
    expect(dayKey(new Date(2026, 0, 5, 23, 59))).toBe("2026-01-05");
    expect(dayKey(new Date(2026, 11, 31))).toBe("2026-12-31");
  });
});

describe("eventsByDay", () => {
  const from = new Date(2026, 9, 19);
  const to = new Date(2026, 9, 26);

  it("puts a timed event on its day", () => {
    const m = eventsByDay([ev("a", at(2026, 10, 20, 9), at(2026, 10, 20, 10))], from, to);
    expect(keys(m)).toEqual({ "2026-10-20": ["a"] });
  });

  it("lists an event on every day it covers", () => {
    const m = eventsByDay([ev("a", at(2026, 10, 20, 22), at(2026, 10, 22, 1))], from, to);
    expect(keys(m)).toEqual({ "2026-10-20": ["a"], "2026-10-21": ["a"], "2026-10-22": ["a"] });
  });

  it("treats the all-day end as exclusive", () => {
    const m = eventsByDay([ev("a", at(2026, 10, 20), at(2026, 10, 22), true)], from, to);
    expect(keys(m)).toEqual({ "2026-10-20": ["a"], "2026-10-21": ["a"] });
  });

  it("doesn't put an event ending at midnight on the next day", () => {
    const m = eventsByDay([ev("a", at(2026, 10, 20, 23), at(2026, 10, 21))], from, to);
    expect(keys(m)).toEqual({ "2026-10-20": ["a"] });
  });

  it("clamps to [from, to)", () => {
    const m = eventsByDay(
      [
        ev("long", at(2026, 10, 1), at(2026, 11, 30), true),
        ev("before", at(2026, 10, 18, 9), at(2026, 10, 18, 10)),
        ev("after", at(2026, 10, 26, 9), at(2026, 10, 26, 10)),
      ],
      from,
      to,
    );
    expect([...m.keys()]).toEqual(["2026-10-19", "2026-10-20", "2026-10-21", "2026-10-22", "2026-10-23", "2026-10-24", "2026-10-25"]);
    for (const list of m.values()) expect(list.map((e) => e.key)).toEqual(["long"]);
  });

  it("keeps a zero-length event on its day, including at the range start", () => {
    const m = eventsByDay([ev("a", at(2026, 10, 19), at(2026, 10, 19)), ev("b", at(2026, 10, 21, 8), at(2026, 10, 21, 8))], from, to);
    expect(keys(m)).toEqual({ "2026-10-19": ["a"], "2026-10-21": ["b"] });
  });

  it("keeps input order within a day", () => {
    const m = eventsByDay([ev("b", at(2026, 10, 20, 12), at(2026, 10, 20, 13)), ev("a", at(2026, 10, 20, 9), at(2026, 10, 20, 10))], from, to);
    expect(keys(m)).toEqual({ "2026-10-20": ["b", "a"] });
  });

  it("returns nothing for an empty range", () => {
    expect(eventsByDay([ev("a", at(2026, 10, 20, 9), at(2026, 10, 20, 10))], to, from).size).toBe(0);
  });
});

describe("temp", () => {
  it("rounds and converts", () => {
    expect(temp(21.4, "c")).toBe("21°");
    expect(temp(20, "f")).toBe("68°");
    expect(temp(-0.4, "c")).toBe("0°");
  });
});
