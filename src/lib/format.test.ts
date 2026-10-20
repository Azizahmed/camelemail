import { describe, expect, it } from "vitest";
import { addressList, avatarColor, base64Size, displayName, escapeHtml, fileSize, initials, listDate, wakeDate } from "./format";

const NOW = new Date(2026, 9, 19, 15, 0).getTime();
const MIN = 60_000;

describe("listDate", () => {
  it("is relative today", () => {
    expect(listDate(NOW - 20_000, NOW)).toBe("just now");
    expect(listDate(NOW - 5 * MIN, NOW)).toBe("5m ago");
    expect(listDate(NOW - 150 * MIN, NOW)).toBe("2h ago");
  });

  it("says Yesterday for any time yesterday", () => {
    expect(listDate(new Date(2026, 9, 18, 23, 59).getTime(), NOW)).toBe("Yesterday");
    expect(listDate(new Date(2026, 9, 18, 0, 1).getTime(), NOW)).toBe("Yesterday");
  });

  it("uses the weekday within a week, then the date", () => {
    const wed = new Date(2026, 9, 14, 9, 0);
    expect(listDate(wed.getTime(), NOW)).toBe(wed.toLocaleDateString(undefined, { weekday: "short" }));
    const old = new Date(2026, 8, 1, 9, 0);
    expect(listDate(old.getTime(), NOW)).toBe(old.toLocaleDateString(undefined, { month: "short", day: "numeric" }));
  });
});

describe("wakeDate", () => {
  it("names today and tomorrow", () => {
    expect(wakeDate(new Date(2026, 9, 19, 18, 0).getTime(), NOW)).toMatch(/^Today, /);
    expect(wakeDate(new Date(2026, 9, 20, 8, 0).getTime(), NOW)).toMatch(/^Tomorrow, /);
  });
});

describe("sizes", () => {
  it("formats bytes", () => {
    expect(fileSize(512)).toBe("512 B");
    expect(fileSize(2048)).toBe("2 KB");
    expect(fileSize(5 * 1024 * 1024 + 100_000)).toBe("5.1 MB");
  });

  it("estimates decoded base64", () => {
    expect(base64Size(btoa("hello world!"))).toBe(12);
  });
});

describe("names", () => {
  it("makes initials from names in any script", () => {
    expect(initials("Sara Ahmed")).toBe("SA");
    expect(initials("Mary Jane Watson")).toBe("MW");
    expect(initials("عزيز أحمد")).toBe("عأ");
    expect(initials("john@example.com")).toBe("J");
    expect(initials('"Bob" <bob@x.com>')).toBe("?");
    expect(initials("")).toBe("?");
  });

  it("picks a stable avatar colour", () => {
    expect(avatarColor("a@b.c")).toBe(avatarColor("a@b.c"));
    expect(avatarColor("")).toMatch(/^#[0-9a-f]{6}$/);
  });

  it("falls back to the mailbox name", () => {
    expect(displayName({ name: "  ", email: "sara@x.com" })).toBe("sara");
    expect(displayName({ name: "Sara", email: "sara@x.com" })).toBe("Sara");
    expect(addressList([{ name: "Sara", email: "s@x.com" }, { name: "", email: "b@x.com" }])).toBe("Sara <s@x.com>, b@x.com");
  });

  it("escapes HTML", () => {
    expect(escapeHtml(`<a href="x">'&'</a>`)).toBe("&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;");
  });
});
