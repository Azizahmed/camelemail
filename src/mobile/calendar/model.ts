// Pure helpers of the phone calendar (CalendarScreen): which days a view
// shows, what range to load, the agenda's day list and swipe reading.

import { DAY, addDays, dayKey, isBar, startOfDay, startOfWeek, type CalEvent } from "../../lib/calendar.svelte";
import { t, type MessageKey } from "../../lib/i18n.svelte";

export type PhoneView = "agenda" | "day" | "3day";

/** The views and their names' keys. */
export const PHONE_VIEWS: [PhoneView, MessageKey][] = [
  ["agenda", "m.cal.agenda"],
  ["day", "m.cal.day"],
  ["3day", "m.cal.3days"],
];

/** Days a time view shows, from the selected day. */
export function periodDays(view: PhoneView, selected: Date): Date[] {
  const s = startOfDay(selected);
  return view === "3day" ? [s, addDays(s, 1), addDays(s, 2)] : [s];
}

/** How many days one swipe moves the time view. */
export const periodStep = (view: PhoneView) => (view === "3day" ? 3 : 1);

/** The span of events to load: the week strip around the selected day, the
 *  agenda's days and the month picker's month. */
export function loadRange(selected: Date, weekStart: number, agendaDays: number, pickerMonth: Date | null): [number, number] {
  const week = startOfWeek(selected, weekStart);
  let from = addDays(week, -7).getTime();
  let to = Math.max(addDays(week, 21).getTime(), addDays(startOfDay(selected), agendaDays + 1).getTime());
  if (pickerMonth) {
    const ps = startOfWeek(new Date(pickerMonth.getFullYear(), pickerMonth.getMonth(), 1), weekStart);
    from = Math.min(from, ps.getTime());
    to = Math.max(to, addDays(ps, 42).getTime());
  }
  // Stay within what the engine serves.
  const maxSpan = 760 * DAY;
  if (to - from > maxSpan) to = from + maxSpan;
  return [from, to];
}

export interface AgendaDay {
  d: Date;
  key: string;
  /** All-day and multi-day events, then timed ones by start. */
  events: CalEvent[];
  /** The first listed day of a month other than the first day's. */
  monthStart: boolean;
}

/** The agenda's days from `from`: days with events, plus `from` and today (shown empty). */
export function agendaList(from: Date, count: number, byDay: Map<string, CalEvent[]>, today: Date): AgendaDay[] {
  const out: AgendaDay[] = [];
  const first = startOfDay(from);
  let month = first.getFullYear() * 12 + first.getMonth();
  for (let i = 0; i < count; i++) {
    const d = addDays(first, i);
    const key = dayKey(d);
    const list = byDay.get(key) ?? [];
    if (i > 0 && list.length === 0 && key !== dayKey(today)) continue;
    const bars = list.filter(isBar);
    const timed = list.filter((e) => !isBar(e)).sort((a, b) => a.startMs - b.startMs);
    const m = d.getFullYear() * 12 + d.getMonth();
    out.push({ d, key, events: [...bars, ...timed], monthStart: m !== month });
    month = m;
  }
  return out;
}

/** A finished horizontal swipe: -1 = to the previous period, 1 = the next, 0 = none.
 *  Swiping left (finger towards the start in LTR) moves forward; RTL mirrors it. */
export function swipeDir(dx: number, dy: number, rtl: boolean, threshold = 60): -1 | 0 | 1 {
  if (Math.abs(dx) < threshold || Math.abs(dx) < Math.abs(dy) * 1.5) return 0;
  const forward = rtl ? dx > 0 : dx < 0;
  return forward ? 1 : -1;
}

/** A maps search for a location (opens the maps app on phones). */
export const mapsUrl = (location: string) => `https://www.google.com/maps/search/?api=1&query=${encodeURIComponent(location.trim())}`;

/** An online meeting's link label (Teams, Zoom, Meet…), or the host; null if it isn't a web link. */
export function meetingLabel(url: string): string | null {
  if (!/^https:\/\//i.test(url)) return null;
  try {
    const host = new URL(url).hostname.replace(/^www\./, "");
    const name =
      host.endsWith("teams.microsoft.com") || host.endsWith("teams.live.com") ? "Teams" : host.endsWith("zoom.us") ? "Zoom" : host === "meet.google.com" ? "Google Meet" : null;
    return name ? t("m.cal.join", { name }) : host;
  } catch {
    return null;
  }
}

/** "15 min before", "1 h before", "At start"… */
export const alarmLabel = (m: number) =>
  m === 0
    ? t("m.cal.atStart")
    : m === 1440
      ? t("m.cal.daysBefore.one")
      : m % 1440 === 0
        ? t("m.cal.daysBefore", { n: m / 1440 })
        : m % 60 === 0
          ? t("m.cal.hoursBefore", { n: m / 60 })
          : t("m.cal.minBefore", { n: m });

/** An attendee's answer, as shown next to their name. */
export function partstatLabel(p: string): string {
  switch (p.toUpperCase()) {
    case "ACCEPTED":
      return t("m.cal.going");
    case "DECLINED":
      return t("m.cal.notGoing");
    case "TENTATIVE":
      return t("m.cal.maybe");
    default:
      return "";
  }
}
