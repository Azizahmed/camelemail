// Quick add: "Lunch with Sara tomorrow at 1pm", "Standup every weekday 9:30".
// Pure (no Svelte, no Tauri) so it can be tested on its own.

const DAY = 86_400_000;
const startOfDay = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate());
const addDays = (d: Date, n: number) => new Date(d.getFullYear(), d.getMonth(), d.getDate() + n, d.getHours(), d.getMinutes());
const fmtTime = (ms: number) => new Date(ms).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });

export interface Parsed {
  title: string;
  start: Date | null;
  end: Date | null;
  allDay: boolean;
  rrule: string | null;
  /** Human summary of what was understood, e.g. "Tomorrow · 1:00 PM". */
  hint: string;
}

const WD_SHORT = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
const RR_DAYS = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"];
const MONTHS = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

function parseClock(h: string, m: string | undefined, ap: string | undefined): [number, number] | null {
  let hour = Number(h);
  const min = m ? Number(m) : 0;
  if (min > 59 || hour > 23) return null;
  const a = ap?.toLowerCase().replace(/\./g, "");
  if (a === "pm" && hour < 12) hour += 12;
  if (a === "am" && hour === 12) hour = 0;
  return [hour, min];
}

/** `baseDay`: the day to use when the text names a time but no date (e.g. the day that was clicked). */
export function parseQuickAdd(input: string, now = new Date(), baseDay: Date | null = null): Parsed {
  let text = ` ${input} `;
  let day: Date | null = null;
  let start: [number, number] | null = null;
  let end: [number, number] | null = null;
  let durationMin: number | null = null;
  let rrule: string | null = null;
  const take = (re: RegExp, fn: (m: RegExpExecArray) => boolean | void) => {
    const m = re.exec(text);
    if (m && fn(m) !== false) text = text.slice(0, m.index) + " " + text.slice(m.index + m[0].length);
  };
  const today = startOfDay(now);

  // Repeats
  take(/\s(every\s+weekday|every\s+work\s*day|weekdays)\s/i, () => void (rrule = "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"));
  take(/\severy\s+(sun|mon|tue|wed|thu|fri|sat)[a-z]*\s/i, (m) => {
    const i = WD_SHORT.indexOf(m[1].toLowerCase());
    rrule = `FREQ=WEEKLY;BYDAY=${RR_DAYS[i]}`;
    day = addDays(today, (i - today.getDay() + 7) % 7);
  });
  // "Weekly review" / "Daily standup": a leading word belongs to the title.
  const repeat = (re: RegExp, rule: string) => take(re, () => (rrule ? false : void (rrule = rule)));
  repeat(/(?<=\S)\s(daily|every\s+day)\s/i, "FREQ=DAILY");
  repeat(/(?<=\S)\s(weekly|every\s+week)\s/i, "FREQ=WEEKLY");
  repeat(/(?<=\S)\s(monthly|every\s+month)\s/i, "FREQ=MONTHLY");
  repeat(/(?<=\S)\s(yearly|annually|every\s+year)\s/i, "FREQ=YEARLY");

  // Dates
  take(/\s(day after tomorrow)\s/i, () => void (day = addDays(today, 2)));
  take(/\s(tomorrow|tmrw|tmr)\s/i, () => void (day = addDays(today, 1)));
  let tonight = false;
  take(/\s(today|tonight)\s/i, (m) => {
    day = today;
    tonight = m[1].toLowerCase() === "tonight";
  });
  take(/\sin\s+(\d+)\s+(day|days|week|weeks)\s/i, (m) => void (day = addDays(today, Number(m[1]) * (m[2].startsWith("week") ? 7 : 1))));
  take(/\s(?:on\s+)?(?:next\s+)?(sun|mon|tue|wed|thu|fri|sat)(?:day|sday|nesday|rsday|urday)?\s/i, (m) => {
    if (day) return false;
    const i = WD_SHORT.indexOf(m[1].toLowerCase());
    // The coming one; on that weekday itself, a week from today.
    day = addDays(today, (i - today.getDay() + 7) % 7 || 7);
  });
  take(/\s(?:on\s+)?(\d{1,2})(?:st|nd|rd|th)?\s+(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\.?(?:\s+(\d{4}))?\s/i, (m) => {
    const mon = MONTHS.indexOf(m[2].toLowerCase());
    let d = new Date(m[3] ? Number(m[3]) : now.getFullYear(), mon, Number(m[1]));
    if (!m[3] && d < today) d = new Date(now.getFullYear() + 1, mon, Number(m[1]));
    day = d;
  });
  take(/\s(?:on\s+)?(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\.?\s+(\d{1,2})(?:st|nd|rd|th)?(?:,?\s+(\d{4}))?\s/i, (m) => {
    if (day) return false;
    const mon = MONTHS.indexOf(m[1].toLowerCase());
    let d = new Date(m[3] ? Number(m[3]) : now.getFullYear(), mon, Number(m[2]));
    if (!m[3] && d < today) d = new Date(now.getFullYear() + 1, mon, Number(m[2]));
    day = d;
  });

  // Times
  take(/\s(?:from\s+)?(\d{1,2})(?::(\d{2}))?\s*(am|pm|a\.m\.|p\.m\.)?\s*(?:-|–|to|until)\s*(\d{1,2})(?::(\d{2}))?\s*(am|pm|a\.m\.|p\.m\.)?\s/i, (m) => {
    const ap2 = m[6];
    let s = parseClock(m[1], m[2], m[3] ?? ap2);
    const e = parseClock(m[4], m[5], ap2);
    if (!s || !e) return false;
    // "11-1pm": the end's pm would put the start after the end, so it's 11am.
    if (!m[3] && ap2 && s[0] * 60 + s[1] >= e[0] * 60 + e[1]) s = parseClock(m[1], m[2], /^p/i.test(ap2) ? "am" : "pm") ?? s;
    start = s;
    end = e;
  });
  take(/\s(?:at\s+|@\s*)?(noon|midnight)\s/i, (m) => void (start = m[1].toLowerCase() === "noon" ? [12, 0] : [0, 0]));
  take(/\s(?:at\s+|@\s*)(\d{1,2})(?::(\d{2}))?\s*(am|pm|a\.m\.|p\.m\.)?\s/i, (m) => {
    if (start) return false;
    const s = parseClock(m[1], m[2], m[3]);
    if (!s) return false;
    // "at 3" without am/pm: afternoon for 1–7, evening "tonight".
    if (!m[3] && s[0] >= 1 && (s[0] <= 7 || (tonight && s[0] < 12))) s[0] += 12;
    start = s;
  });
  take(/\s(\d{1,2})(?::(\d{2}))?\s*(am|pm|a\.m\.|p\.m\.)\s/i, (m) => {
    if (start) return false;
    start = parseClock(m[1], m[2], m[3]);
  });
  take(/\s(\d{1,2}):(\d{2})\s/i, (m) => {
    if (start) return false;
    start = parseClock(m[1], m[2], undefined);
  });
  take(/\sfor\s+(\d+(?:\.\d+)?)\s*(h|hr|hrs|hour|hours|m|min|mins|minute|minutes)\s/i, (m) => {
    durationMin = Number(m[1]) * (m[2].toLowerCase().startsWith("h") ? 60 : 1);
  });

  if (tonight && !start) start = [19, 0];
  const title = text.replace(/\s+/g, " ").replace(/\s+(on|at|from)$/i, "").trim();
  const base = day ?? (start ? (baseDay ? startOfDay(baseDay) : today) : null);
  if (!base) return { title, start: null, end: null, allDay: false, rrule, hint: rrule ? "Repeats" : "" };
  let d0 = base as Date;
  // A series on certain weekdays starts on the first of them ("every weekday" typed on a Saturday).
  const byDay = !day && (rrule as string | null)?.match(/BYDAY=([A-Z,]+)/)?.[1].split(",");
  if (byDay) while (!byDay.includes(RR_DAYS[d0.getDay()])) d0 = addDays(d0, 1);
  if (!start) {
    const s = startOfDay(d0);
    return { title, start: s, end: addDays(s, 1), allDay: true, rrule, hint: `${s.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" })} · all day` };
  }
  const st = start as [number, number];
  const s = new Date(d0.getFullYear(), d0.getMonth(), d0.getDate(), st[0], st[1]);
  let e: Date;
  if (end) {
    const en = end as [number, number];
    e = new Date(d0.getFullYear(), d0.getMonth(), d0.getDate(), en[0], en[1]);
    if (e <= s) e = new Date(e.getTime() + 12 * 3600_000 <= s.getTime() ? e.getTime() + DAY : e.getTime() + 12 * 3600_000);
  } else {
    e = new Date(s.getTime() + (durationMin ?? 60) * 60_000);
  }
  return {
    title,
    start: s,
    end: e,
    allDay: false,
    rrule,
    hint: `${s.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" })} · ${fmtTime(s.getTime())} – ${fmtTime(e.getTime())}${rrule ? " · repeats" : ""}`,
  };
}

