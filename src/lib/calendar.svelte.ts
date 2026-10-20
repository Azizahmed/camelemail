// Calendar state, date helpers and quick-add parsing.

import { invoke } from "@tauri-apps/api/core";
import { invokeInOrder } from "./api";
import type { AppSettings } from "./types";
import { confirmDialog, toast } from "./ui.svelte";
export { parseQuickAdd } from "./quickadd";

// ---------------------------------------------------------------------------
// Types (mirror crates/remail-core/src/calendar.rs and caldav/event.rs)
// ---------------------------------------------------------------------------

export type CalView = "day" | "week" | "month" | "quarter" | "year";

export interface CalendarInfo {
  id: string;
  accountId: string;
  accountName: string;
  name: string;
  color: string;
  readOnly: boolean;
  enabled: boolean;
}

export interface CalAccount {
  id: string;
  /** "ics" = a read-only iCal subscription */
  kind: "caldav" | "ics" | "graph";
  mailAccountId: string | null;
  name: string;
  url: string;
  username: string;
  status: "ok" | "error" | "signin";
  lastError: string | null;
  lastSync: number | null;
  calendars: CalendarInfo[];
}

export interface Attendee {
  email: string;
  name: string;
  partstat: string;
}

export interface CalEvent {
  key: string;
  calendarId: string;
  href: string;
  uid: string;
  recurrenceId: number | null;
  startMs: number;
  endMs: number;
  allDay: boolean;
  recurring: boolean;
  summary: string;
  location: string;
  description: string;
  url: string;
  status: string;
  alarmsMinutesBefore: number[];
  organizer: string | null;
  attendees: Attendee[];
  rrule: string | null;
  color: string;
  readOnly: boolean;
}

export interface EventInput {
  summary: string;
  location: string;
  description: string;
  startMs: number;
  endMs: number;
  allDay: boolean;
  tz: string;
  rrule: string | null;
  alarmsMinutesBefore: number[];
}

export type EditScope = { type: "all" } | { type: "this"; recurrenceId: number };

export interface DayWeather {
  date: string;
  code: number;
  max: number;
  min: number;
}

export interface Place {
  name: string;
  region: string;
  country: string;
  latitude: number;
  longitude: number;
}

export interface CalendarPrefs {
  weekStart: 0 | 1 | 6;
  weekNumbers: boolean;
  weather: Place | null;
  units: "c" | "f";
  defaultCalendar: string | null;
  view: CalView;
}

export const calApi = {
  accounts: () => invoke<CalAccount[]>("cal_accounts"),
  calendars: () => invoke<CalendarInfo[]>("cal_calendars"),
  connect: (req: { mailAccountId: string | null; url: string | null; username: string | null; password: string | null; allowInvalidCerts: boolean }) =>
    invoke<CalAccount>("cal_connect", { req }),
  subscribe: (req: { url: string; name: string | null; username: string | null; password: string | null }) => invoke<CalAccount>("cal_subscribe", { req }),
  removeAccount: (id: string) => invoke<void>("cal_remove_account", { id }),
  setCalendar: (id: string, enabled: boolean | null, color: string | null) => invokeInOrder<void>("cal_set_calendar", { id, enabled, color }),
  sync: (accountId: string | null = null) => invoke<void>("cal_sync", { accountId }),
  events: (fromMs: number, toMs: number) => invoke<CalEvent[]>("cal_events", { fromMs, toMs }),
  search: (query: string) => invoke<CalEvent[]>("cal_search", { query }),
  create: (calendarId: string, input: EventInput) => invoke<void>("cal_create", { calendarId, input }),
  update: (calendarId: string, href: string, input: EventInput, scope: EditScope, newCalendarId: string | null, occurrenceStart: number | null) =>
    invoke<void>("cal_update", { calendarId, href, input, scope, newCalendarId, occurrenceStart }),
  remove: (calendarId: string, href: string, scope: EditScope) => invoke<void>("cal_delete", { calendarId, href, scope }),
  weather: () => invoke<DayWeather[]>("weather_forecast"),
  places: (query: string) => invoke<Place[]>("weather_places", { query }),
};

// ---------------------------------------------------------------------------
// Dates (local time)
// ---------------------------------------------------------------------------

export const DAY = 86_400_000;
export const localTz = () => Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";

export function startOfDay(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate());
}

export function addDays(d: Date, n: number): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() + n, d.getHours(), d.getMinutes());
}

export function addMonths(d: Date, n: number): Date {
  const r = new Date(d.getFullYear(), d.getMonth() + n, 1);
  const last = new Date(r.getFullYear(), r.getMonth() + 1, 0).getDate();
  r.setDate(Math.min(d.getDate(), last));
  return r;
}

/** Whole calendar days from `a` to `b` (DST-safe). */
export function daysBetween(a: Date, b: Date): number {
  return Math.round((startOfDay(b).getTime() - startOfDay(a).getTime()) / DAY);
}

/** Minutes since local midnight (wall clock, so DST days line up with the hour grid). */
export function minutesOfDay(ms: number): number {
  const d = new Date(ms);
  return d.getHours() * 60 + d.getMinutes();
}

export function sameDay(a: Date, b: Date): boolean {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

export function startOfWeek(d: Date, weekStart: number): Date {
  const s = startOfDay(d);
  return addDays(s, -((s.getDay() - weekStart + 7) % 7));
}

/** ISO 8601 week number ("CW") of the week a row starting at `rowStart` mostly covers. */
export function weekNumber(rowStart: Date): number {
  const t = addDays(rowStart, 3);
  const thursday = new Date(t.getFullYear(), t.getMonth(), t.getDate() + 3 - ((t.getDay() + 6) % 7));
  const jan4 = new Date(thursday.getFullYear(), 0, 4);
  return 1 + Math.round(((thursday.getTime() - jan4.getTime()) / DAY - 3 + ((jan4.getDay() + 6) % 7)) / 7);
}

export const dayKey = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;

// Built once: views format a time or date per event and per day.
const HOUR = new Intl.DateTimeFormat(undefined, { hour: "numeric" });
const HOUR_MINUTE = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });
const EVENT_DAY = new Intl.DateTimeFormat(undefined, { weekday: "short", month: "short", day: "numeric" });
const WEEKDAY = { short: new Intl.DateTimeFormat(undefined, { weekday: "short" }), narrow: new Intl.DateTimeFormat(undefined, { weekday: "narrow" }) };

export function weekdayNames(weekStart: number, style: "short" | "narrow" = "short"): string[] {
  const base = new Date(2023, 0, 1); // a Sunday
  return Array.from({ length: 7 }, (_, i) => WEEKDAY[style].format(addDays(base, (weekStart + i) % 7)).toUpperCase());
}

export function fmtTime(ms: number, compact = false): string {
  const d = new Date(ms);
  if (compact && d.getMinutes() === 0) return HOUR.format(d);
  return HOUR_MINUTE.format(d);
}

export function fmtRange(e: CalEvent): string {
  const s = new Date(e.startMs);
  const end = new Date(e.endMs);
  if (e.allDay) {
    const last = addDays(end, -1);
    return sameDay(s, last) || last < s
      ? `${EVENT_DAY.format(s)} · all day`
      : `${EVENT_DAY.format(s)} – ${EVENT_DAY.format(last)} · all day`;
  }
  if (sameDay(s, end) || (end.getTime() - s.getTime() < DAY && end.getHours() === 0 && end.getMinutes() === 0)) {
    return `${EVENT_DAY.format(s)} · ${fmtTime(e.startMs)} – ${fmtTime(e.endMs)}`;
  }
  return `${EVENT_DAY.format(s)} ${fmtTime(e.startMs)} – ${EVENT_DAY.format(end)} ${fmtTime(e.endMs)}`;
}

/** Does the event cover any part of the local day `d`? */
export function onDay(e: CalEvent, d: Date): boolean {
  const s = startOfDay(d).getTime();
  const end = addDays(startOfDay(d), 1).getTime();
  return e.startMs < end && (e.endMs > s || (e.endMs === e.startMs && e.startMs >= s));
}

/** Shown as a bar (all-day or spanning midnight) rather than a timed row. */
export const isBar = (e: CalEvent) => e.allDay || e.endMs - e.startMs >= DAY;

/** Events by local day (`dayKey`) for the days in [from, to). An event is
 *  listed on every day it covers (as `onDay` decides), in input order. */
export function eventsByDay(events: CalEvent[], from: Date, to: Date): Map<string, CalEvent[]> {
  const out = new Map<string, CalEvent[]>();
  const first = startOfDay(from);
  const lo = first.getTime();
  const hi = to.getTime();
  for (const e of events) {
    if (e.startMs >= hi || (e.endMs <= lo && !(e.endMs === e.startMs && e.startMs >= lo))) continue;
    // Walk only the days the event touches, within the range.
    let d = e.startMs > lo ? startOfDay(new Date(e.startMs)) : first;
    while (d.getTime() < hi && onDay(e, d)) {
      const k = dayKey(d);
      const list = out.get(k);
      if (list) list.push(e);
      else out.set(k, [e]);
      d = addDays(d, 1);
    }
  }
  return out;
}

/** A temperature in the chosen units, e.g. "21°". */
export const temp = (v: number, units: "c" | "f") => `${Math.round(units === "f" ? v * 1.8 + 32 : v)}°`;

// ---------------------------------------------------------------------------
// Weather (WMO codes from Open-Meteo)
// ---------------------------------------------------------------------------

export function weatherKind(code: number): "sun" | "partly" | "cloud" | "fog" | "drizzle" | "rain" | "snow" | "storm" {
  if (code === 0) return "sun";
  if (code <= 2) return "partly";
  if (code === 3) return "cloud";
  if (code === 45 || code === 48) return "fog";
  if (code >= 51 && code <= 57) return "drizzle";
  if ((code >= 61 && code <= 67) || (code >= 80 && code <= 82)) return "rain";
  if ((code >= 71 && code <= 77) || code === 85 || code === 86) return "snow";
  if (code >= 95) return "storm";
  return "cloud";
}

// ---------------------------------------------------------------------------
// Window state
// ---------------------------------------------------------------------------

export const cal = $state({
  view: "month" as CalView,
  /** Date the current view is anchored on. */
  anchor: startOfDay(new Date()),
  selected: startOfDay(new Date()),
  events: [] as CalEvent[],
  range: [0, 0] as [number, number],
  calendars: [] as CalendarInfo[],
  accounts: [] as CalAccount[],
  weather: {} as Record<string, DayWeather>,
  prefs: null as CalendarPrefs | null,
  settings: null as AppSettings | null,
  loading: false,
  syncing: false,
  query: "",
  results: null as CalEvent[] | null,
});

export const writable = () => cal.calendars.filter((c) => c.enabled && !c.readOnly);

/** Delete an event, asking "this occurrence or the series?" for repeating ones. Returns whether it was deleted. */
export async function deleteEvent(ev: CalEvent): Promise<boolean> {
  let scope: EditScope = { type: "all" };
  if (ev.recurring && ev.recurrenceId !== null) {
    const r = await confirmDialog({
      title: "Delete a repeating event",
      message: "Delete only this occurrence, or the whole series?",
      confirmLabel: "All events",
      altLabel: "This event only",
      danger: true,
    });
    if (r === "cancel") return false;
    if (r === "alt") scope = { type: "this", recurrenceId: ev.recurrenceId };
  } else {
    const r = await confirmDialog({ title: "Delete event?", message: `"${ev.summary || "(No title)"}" will be deleted from the calendar.`, confirmLabel: "Delete", danger: true });
    if (r !== "confirm") return false;
  }
  try {
    await calApi.remove(ev.calendarId, ev.href, scope);
    return true;
  } catch (e) {
    toast(e instanceof Error ? e.message : String(e), { kind: "error", duration: 7000 });
    return false;
  }
}
export const calendarOf = (id: string) => cal.calendars.find((c) => c.id === id);
