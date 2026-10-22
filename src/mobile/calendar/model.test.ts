import { describe, expect, it, vi } from "vitest";

// ui.svelte touches `window` when loaded; these helpers don't need it.
vi.mock("../../lib/ui.svelte", () => ({ confirmDialog: vi.fn(), toast: vi.fn() }));

const { agendaList, loadRange, mapsUrl, meetingLabel, periodDays, swipeDir } = await import("./model");
const { dayKey, eventsByDay } = await import("../../lib/calendar.svelte");
type CalEvent = import("../../lib/calendar.svelte").CalEvent;

const at = (y: number, m: number, d: number, h = 0) => new Date(y, m - 1, d, h);

function ev(key: string, start: Date, end: Date, allDay = false): CalEvent {
  return {
    key,
    calendarId: "c",
    href: key,
    uid: key,
    recurrenceId: null,
    startMs: start.getTime(),
    endMs: end.getTime(),
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
    color: "#f00",
    readOnly: false,
  };
}

describe("periodDays", () => {
  it("shows one or three days from the selected day", () => {
    expect(periodDays("day", at(2026, 10, 21, 15)).map(dayKey)).toEqual(["2026-10-21"]);
    expect(periodDays("3day", at(2026, 10, 31)).map(dayKey)).toEqual(["2026-10-31", "2026-11-01", "2026-11-02"]);
  });
});

describe("loadRange", () => {
  it("covers the week strip around the day and the agenda", () => {
    const [from, to] = loadRange(at(2026, 10, 21), 1, 60, null);
    expect(dayKey(new Date(from))).toBe("2026-10-12"); // the Monday before the week of the 21st
    expect(to).toBeGreaterThanOrEqual(at(2026, 12, 20).getTime());
  });
  it("adds the month picker's month", () => {
    const [from] = loadRange(at(2026, 10, 21), 0, 7, new Date(2026, 6, 1));
    expect(from).toBeLessThanOrEqual(at(2026, 7, 1).getTime());
  });
});

describe("agendaList", () => {
  it("lists the first day, today and days with events, marking new months", () => {
    const events = [ev("timed", at(2026, 10, 30, 14), at(2026, 10, 30, 15)), ev("early", at(2026, 10, 30, 9), at(2026, 10, 30, 10)), ev("allday", at(2026, 11, 2), at(2026, 11, 3), true)];
    const byDay = eventsByDay(events, at(2026, 10, 1), at(2026, 12, 1));
    const list = agendaList(at(2026, 10, 21), 20, byDay, at(2026, 10, 25));
    expect(list.map((d) => d.key)).toEqual(["2026-10-21", "2026-10-25", "2026-10-30", "2026-11-02"]);
    expect(list.map((d) => d.monthStart)).toEqual([false, false, false, true]);
    expect(list[2].events.map((e) => e.key)).toEqual(["early", "timed"]);
  });
});

describe("swipeDir", () => {
  it("reads left as next in LTR and mirrors it in RTL", () => {
    expect(swipeDir(-100, 10, false)).toBe(1);
    expect(swipeDir(100, 10, false)).toBe(-1);
    expect(swipeDir(-100, 10, true)).toBe(-1);
  });
  it("ignores short or mostly vertical moves", () => {
    expect(swipeDir(-30, 0, false)).toBe(0);
    expect(swipeDir(-100, 90, false)).toBe(0);
  });
});

describe("links", () => {
  it("names meeting services and builds maps searches", () => {
    expect(meetingLabel("https://teams.microsoft.com/l/meetup-join/x")).toBe("Join Teams meeting");
    expect(meetingLabel("https://www.example.com/x")).toBe("example.com");
    expect(meetingLabel("mailto:a@b.c")).toBeNull();
    expect(mapsUrl(" 1 Main St & Co ")).toBe("https://www.google.com/maps/search/?api=1&query=1%20Main%20St%20%26%20Co");
  });
});
