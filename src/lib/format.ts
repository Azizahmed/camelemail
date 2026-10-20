// Date and size formatting for the message list and threads.

const DAY = 86_400_000;

function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

/** Calendar days from `b` to `a` (days aren't always 24 hours long where clocks change). */
function dayDiff(a: number, b: number): number {
  return Math.round((startOfDay(new Date(a)) - startOfDay(new Date(b))) / DAY);
}

// Formatters are costly to build (WebKit especially) and lists format a date
// per row, so each one is built once.
const fmt = (options: Intl.DateTimeFormatOptions) => new Intl.DateTimeFormat(undefined, options);
const TIME = fmt({ hour: "numeric", minute: "2-digit" });
const WEEKDAY_SHORT = fmt({ weekday: "short" });
const WEEKDAY_LONG = fmt({ weekday: "long" });
const MONTH_DAY = fmt({ month: "short", day: "numeric" });
const MONTH_DAY_YEAR = fmt({ month: "short", day: "numeric", year: "numeric" });
const WEEKDAY_MONTH_DAY = fmt({ weekday: "short", month: "short", day: "numeric" });
const FULL_DATE = fmt({ weekday: "long", year: "numeric", month: "long", day: "numeric" });

function time(d: Date): string {
  return TIME.format(d);
}

/** Compact list date: just now / 5m ago / 2h ago / Yesterday / Mon / Dec 8. */
export function listDate(ts: number, now = Date.now()): string {
  const d = new Date(ts);
  const n = new Date(now);
  const today = startOfDay(n);
  if (ts >= today) {
    const mins = Math.floor((now - ts) / 60_000);
    if (mins < 1) return "just now";
    if (mins < 60) return `${mins}m ago`;
    return `${Math.floor(mins / 60)}h ago`;
  }
  const days = dayDiff(now, ts);
  if (days === 1) return "Yesterday";
  if (days < 7) return WEEKDAY_SHORT.format(d);
  return MONTH_DAY.format(d);
}

/** Absolute: Today, 3:42 PM / Yesterday, 3:42 PM / Dec 8, 3:42 PM / Dec 8, 2023, 3:42 PM. */
export function absoluteDate(ts: number, now = Date.now()): string {
  const d = new Date(ts);
  const days = dayDiff(now, ts);
  if (days === 0) return `Today, ${time(d)}`;
  if (days === 1) return `Yesterday, ${time(d)}`;
  const sameYear = d.getFullYear() === new Date(now).getFullYear();
  const date = (sameYear ? MONTH_DAY : MONTH_DAY_YEAR).format(d);
  return `${date}, ${time(d)}`;
}

/** Message header: today → time, else "Dec 8, 3:42 PM". */
export function messageDate(ts: number, now = Date.now()): string {
  const d = new Date(ts);
  if (ts >= startOfDay(new Date(now))) return time(d);
  return absoluteDate(ts, now);
}

/** Snooze/schedule times: Today, 3:00 PM / Tomorrow, 10:00 AM / Saturday, 10:00 AM / Dec 8, 10:00 AM. */
export function wakeDate(ts: number, now = Date.now()): string {
  const d = new Date(ts);
  const days = dayDiff(ts, now);
  if (days === 0) return `Today, ${time(d)}`;
  if (days === 1) return `Tomorrow, ${time(d)}`;
  if (days > 1 && days < 7) return `${WEEKDAY_LONG.format(d)}, ${time(d)}`;
  return `${MONTH_DAY.format(d)}, ${time(d)}`;
}

/** Short preset detail: "Tue, Oct 20 · 8:00 AM". */
export function presetDetail(ts: number): string {
  const d = new Date(ts);
  return `${WEEKDAY_MONTH_DAY.format(d)} · ${time(d)}`;
}

/** Full: "Monday, December 8, 2025 at 3:42 PM". */
export function fullDate(ts: number): string {
  const d = new Date(ts);
  return `${FULL_DATE.format(d)} at ${time(d)}`;
}

export function fileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

export function base64Size(b64: string): number {
  return Math.floor((b64.length * 3) / 4);
}

export function initials(name: string): string {
  const parts = name
    .replace(/[<>"@].*$/, "")
    .split(/\s+/)
    .map((p) => p.replace(/[^\p{L}\p{N}]/gu, ""))
    .filter(Boolean);
  if (parts.length === 0) return "?";
  if (parts.length === 1) return parts[0].slice(0, 1).toUpperCase();
  return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase();
}

const AVATAR_COLORS = ["#0093ff", "#c6a801", "#00c669", "#715000", "#bf0162", "#a72594", "#009da7", "#512da8"];

export function avatarColor(seed: string): string {
  let h = 0;
  for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) | 0;
  return AVATAR_COLORS[Math.abs(h) % AVATAR_COLORS.length];
}

export function displayName(a: { name: string; email: string }): string {
  return a.name?.trim() || a.email.split("@")[0];
}

export function addressList(list: { name: string; email: string }[]): string {
  return list.map((a) => (a.name ? `${a.name} <${a.email}>` : a.email)).join(", ");
}

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}
