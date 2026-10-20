// Day boundaries on the days clocks change (23- and 25-hour days).
import { describe, expect, it, vi } from "vitest";

vi.stubEnv("TZ", "America/New_York");
const { absoluteDate, listDate, wakeDate } = await import("./format");

describe("dates across DST changes (America/New_York)", () => {
  it("runs in New York time", () => {
    expect(new Date(2026, 2, 7, 12).getTimezoneOffset()).toBe(300);
    expect(new Date(2026, 2, 9, 12).getTimezoneOffset()).toBe(240);
  });

  it("doesn't call the day before yesterday 'Yesterday' after a 23-hour day", () => {
    // Clocks went forward on Sunday 8 March 2026.
    const sat = new Date(2026, 2, 7, 23, 30).getTime();
    expect(listDate(sat, new Date(2026, 2, 9, 10).getTime())).not.toBe("Yesterday");
    expect(listDate(new Date(2026, 2, 8, 0, 30).getTime(), new Date(2026, 2, 9, 10).getTime())).toBe("Yesterday");
  });

  it("says Tomorrow for the day after a 23-hour day", () => {
    expect(wakeDate(new Date(2026, 2, 9, 8).getTime(), new Date(2026, 2, 8, 10).getTime())).toMatch(/^Tomorrow, /);
  });

  it("keeps the last hour of a 25-hour day in Today", () => {
    // Clocks went back on Sunday 1 November 2026.
    const late = new Date(2026, 10, 1, 23, 30).getTime();
    expect(absoluteDate(late, new Date(2026, 10, 1, 23, 45).getTime())).toMatch(/^Today, /);
    expect(absoluteDate(late, new Date(2026, 10, 2, 9).getTime())).toMatch(/^Yesterday, /);
  });
});
