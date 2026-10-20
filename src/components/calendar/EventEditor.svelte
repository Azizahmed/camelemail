<script lang="ts">
  import { Sparkles, Trash2, X } from "@lucide/svelte";
  import { errorText } from "../../lib/api";
  import {
    addDays,
    cal,
    daysBetween,
    calApi,
    deleteEvent,
    localTz,
    parseQuickAdd,
    writable,
    type CalEvent,
    type EditScope,
    type EventInput,
  } from "../../lib/calendar.svelte";
  import { confirmDialog, toast } from "../../lib/ui.svelte";

  type Mode = { kind: "new"; start: Date; end: Date; allDay: boolean } | { kind: "edit"; event: CalEvent };
  let { mode, onclose, onsaved }: { mode: Mode; onclose: () => void; onsaved: () => void } = $props();

  // svelte-ignore state_referenced_locally
  const ev = mode.kind === "edit" ? mode.event : null;
  // svelte-ignore state_referenced_locally
  const init = mode.kind === "new" ? mode : { start: new Date(ev!.startMs), end: new Date(ev!.endMs), allDay: ev!.allDay };

  const pad = (n: number) => String(n).padStart(2, "0");
  const dateStr = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
  const timeStr = (d: Date) => `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  const parseDate = (s: string) => {
    const [y, m, d] = s.split("-").map(Number);
    return new Date(y, m - 1, d);
  };

  let title = $state(ev?.summary ?? "");
  let calendarId = $state(ev?.calendarId ?? (cal.prefs?.defaultCalendar && writable().some((c) => c.id === cal.prefs!.defaultCalendar) ? cal.prefs!.defaultCalendar : (writable()[0]?.id ?? "")));
  let allDay = $state(init.allDay);
  let startDate = $state(dateStr(init.start));
  let startTime = $state(timeStr(init.start));
  // All-day end is exclusive; show the last day.
  let endDate = $state(dateStr(init.allDay ? (addDays(init.end, -1) < init.start ? init.start : addDays(init.end, -1)) : init.end));
  let endTime = $state(timeStr(init.end));
  let location = $state(ev?.location ?? "");
  let notes = $state(ev?.description ?? "");
  let alarm = $state(String(ev?.alarmsMinutesBefore[0] ?? -1));
  // Unless changed here, keep the event's own repeat rule and every alert (other apps may set several).
  let repeatTouched = $state(false);
  let alarmTouched = $state(false);
  // svelte-ignore state_referenced_locally
  let datesTouched = $state(mode.kind === "edit");
  let saving = $state(false);
  let titleEl: HTMLInputElement | undefined = $state();

  const REPEATS: [string, string][] = [
    ["", "Never"],
    ["daily", "Every day"],
    ["weekdays", "Every weekday"],
    ["weekly", "Every week"],
    ["biweekly", "Every 2 weeks"],
    ["monthly", "Every month"],
    ["yearly", "Every year"],
  ];
  const BYDAY = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"];
  function rruleFor(kind: string, start: Date): string | null {
    const wd = BYDAY[start.getDay()];
    switch (kind) {
      case "daily":
        return "FREQ=DAILY";
      case "weekdays":
        return "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR";
      case "weekly":
        return `FREQ=WEEKLY;BYDAY=${wd}`;
      case "biweekly":
        return `FREQ=WEEKLY;INTERVAL=2;BYDAY=${wd}`;
      case "monthly":
        return `FREQ=MONTHLY;BYMONTHDAY=${start.getDate()}`;
      case "yearly":
        return "FREQ=YEARLY";
      default:
        return null;
    }
  }
  function repeatKind(rrule: string | null): string {
    if (!rrule) return "";
    const r = rrule.toUpperCase();
    if (r.includes("FREQ=DAILY") && !r.includes("INTERVAL")) return "daily";
    if (r.includes("FREQ=WEEKLY") && r.includes("BYDAY=MO,TU,WE,TH,FR")) return "weekdays";
    if (r.includes("FREQ=WEEKLY") && r.includes("INTERVAL=2")) return "biweekly";
    if (r.includes("FREQ=WEEKLY") && !r.includes("INTERVAL")) return "weekly";
    if (r.includes("FREQ=MONTHLY") && !r.includes("INTERVAL")) return "monthly";
    if (r.includes("FREQ=YEARLY") && !r.includes("INTERVAL")) return "yearly";
    return "custom";
  }
  // svelte-ignore state_referenced_locally
  const originalRule = ev?.rrule ?? null;
  let repeat = $state(repeatKind(originalRule));
  let rrule = $state<string | null>(originalRule);

  // svelte-ignore state_referenced_locally
  const quickBase = init.start;
  const parsed = $derived(mode.kind === "new" ? parseQuickAdd(title, new Date(), quickBase) : null);

  $effect(() => {
    titleEl?.focus();
  });

  // Quick add: "Lunch with Sara tomorrow at 1pm" fills in the dates.
  $effect(() => {
    const p = parsed;
    if (!p || datesTouched) return;
    if (p.start && p.end) {
      allDay = p.allDay;
      startDate = dateStr(p.start);
      startTime = timeStr(p.start);
      endDate = dateStr(p.allDay ? addDays(p.end, -1) : p.end);
      endTime = timeStr(p.end);
    }
    if (p.rrule) {
      rrule = p.rrule;
      repeat = repeatKind(p.rrule);
    }
  });

  function bounds(): [number, number] | null {
    if (allDay) {
      const s = parseDate(startDate);
      const e = addDays(parseDate(endDate), 1);
      return e > s ? [s.getTime(), e.getTime()] : null;
    }
    const s = new Date(`${startDate}T${startTime}`);
    const e = new Date(`${endDate}T${endTime}`);
    return e >= s ? [s.getTime(), e.getTime()] : null;
  }

  async function save() {
    // Ctrl+Enter, a double click or Enter during the confirm dialog must not create it twice.
    if (saving) return;
    const b = bounds();
    if (!b) return toast("The event ends before it starts", { kind: "error" });
    if (!calendarId) return toast("Connect a calendar first (Settings → Calendar)", { kind: "error" });
    saving = true;
    const finalTitle = (parsed && (parsed.start || parsed.rrule) && !datesTouched ? parsed.title : title).trim();
    const rule = ev && !repeatTouched ? originalRule : repeat === "custom" ? rrule : rruleFor(repeat, new Date(b[0]));
    const alarms = ev && !alarmTouched ? ev.alarmsMinutesBefore : Number(alarm) >= 0 ? [Number(alarm)] : [];
    const input: EventInput = {
      summary: finalTitle || "New event",
      location: location.trim(),
      description: notes.trim(),
      startMs: b[0],
      endMs: b[1],
      allDay,
      tz: localTz(),
      rrule: rule,
      alarmsMinutesBefore: alarms,
    };
    let scope: EditScope = { type: "all" };
    if (ev?.recurring && ev.recurrenceId !== null) {
      const r = await confirmDialog({
        title: "Change a repeating event",
        message: "Change only this occurrence, or every event in the series? Moving the time of every event discards changes made to single occurrences.",
        confirmLabel: "All events",
        altLabel: "This event only",
      });
      if (r === "cancel") {
        saving = false;
        return;
      }
      if (r === "alt") scope = { type: "this", recurrenceId: ev.recurrenceId };
    }
    try {
      if (ev) await calApi.update(ev.calendarId, ev.href, input, scope, calendarId !== ev.calendarId && scope.type === "all" ? calendarId : null, ev.startMs);
      else await calApi.create(calendarId, input);
      onsaved();
    } catch (e) {
      toast(errorText(e), { kind: "error", duration: 7000 });
    } finally {
      saving = false;
    }
  }

  async function remove() {
    if (ev && (await deleteEvent(ev))) onsaved();
  }

  function toggleAllDay(v: boolean) {
    datesTouched = true;
    allDay = v;
    if (!v && startTime === "00:00") {
      startTime = "09:00";
      endTime = "10:00";
    }
  }

  // Keep the end after the start when the start moves (whole days for all-day events).
  function shiftEnd(oldStart: number) {
    if (allDay) {
      const delta = daysBetween(new Date(oldStart), parseDate(startDate));
      endDate = dateStr(addDays(parseDate(endDate), delta));
      return;
    }
    const b0 = new Date(`${startDate}T${startTime}`).getTime();
    const e = new Date(`${endDate}T${endTime}`).getTime();
    const ne = new Date(e + (b0 - oldStart));
    if (!Number.isNaN(ne.getTime())) {
      endDate = dateStr(ne);
      endTime = timeStr(ne);
    }
  }
  const curStart = () => (allDay ? parseDate(startDate).getTime() : new Date(`${startDate}T${startTime}`).getTime());
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="scrim" onclick={onclose}>
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="editor" onclick={(e) => e.stopPropagation()} onkeydown={(e) => { if (e.key === "Escape") { e.stopPropagation(); onclose(); } if ((e.ctrlKey || e.metaKey) && e.key === "Enter") void save(); }}>
    <div class="head">
      <span>{ev ? "Edit event" : "New event"}</span>
      <button class="icon-btn" title="Close (Esc)" onclick={onclose}><X size={15} /></button>
    </div>
    <input bind:this={titleEl} class="title" placeholder={ev ? "Title" : "Lunch with Sara tomorrow at 1pm"} bind:value={title} dir="auto" onkeydown={(e) => e.key === "Enter" && !e.ctrlKey && !e.metaKey && save()} />
    {#if parsed && parsed.hint && !datesTouched}
      <div class="hint"><Sparkles size={12} /> <bdi>{parsed.title || "Event"}</bdi> · {parsed.hint}</div>
    {/if}

    <div class="grid">
      <label for="ev-cal">Calendar</label>
      <select id="ev-cal" class="input" bind:value={calendarId}>
        {#each writable() as c (c.id)}<option value={c.id}>{c.name} — {c.accountName}</option>{/each}
      </select>

      <span class="lbl">All-day</span>
      <label class="switch"><input type="checkbox" checked={allDay} onchange={(e) => toggleAllDay(e.currentTarget.checked)} /> All-day event</label>

      <label for="ev-start">Starts</label>
      <div class="dt">
        <input id="ev-start" class="input" type="date" value={startDate} onchange={(e) => { const o = curStart(); datesTouched = true; startDate = e.currentTarget.value; shiftEnd(o); }} />
        {#if !allDay}<input class="input" type="time" aria-label="Start time" value={startTime} onchange={(e) => { const o = curStart(); datesTouched = true; startTime = e.currentTarget.value; shiftEnd(o); }} />{/if}
      </div>

      <label for="ev-end">Ends</label>
      <div class="dt">
        <input id="ev-end" class="input" type="date" bind:value={endDate} onchange={() => (datesTouched = true)} />
        {#if !allDay}<input class="input" type="time" aria-label="End time" bind:value={endTime} onchange={() => (datesTouched = true)} />{/if}
      </div>

      <label for="ev-rep">Repeat</label>
      <select id="ev-rep" class="input" bind:value={repeat} onchange={() => { datesTouched = true; repeatTouched = true; }}>
        {#each REPEATS as [v, l] (v)}<option value={v}>{l}</option>{/each}
        {#if repeat === "custom"}<option value="custom">Custom ({rrule})</option>{/if}
      </select>

      <label for="ev-alarm">Alert</label>
      <select id="ev-alarm" class="input" bind:value={alarm} onchange={() => (alarmTouched = true)}>
        <option value="-1">None</option>
        <option value="0">At time of event</option>
        <option value="5">5 minutes before</option>
        <option value="10">10 minutes before</option>
        <option value="15">15 minutes before</option>
        <option value="30">30 minutes before</option>
        <option value="60">1 hour before</option>
        <option value="120">2 hours before</option>
        <option value="1440">1 day before</option>
        <option value="2880">2 days before</option>
      </select>

      <label for="ev-loc">Location</label>
      <input id="ev-loc" class="input" bind:value={location} dir="auto" placeholder="Add location" />

      <label for="ev-notes">Notes</label>
      <textarea id="ev-notes" class="input notes" bind:value={notes} dir="auto" placeholder="Add notes"></textarea>
    </div>

    <div class="foot">
      {#if ev}<button class="btn danger" onclick={remove}><Trash2 size={13} /> Delete</button>{/if}
      <span class="grow"></span>
      <button class="btn" onclick={onclose}>Cancel</button>
      <button class="btn primary" disabled={saving} onclick={save}>{saving ? "Saving…" : ev ? "Save" : "Add event"}</button>
    </div>
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: var(--overlay-50);
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding-top: 9vh;
    z-index: 500;
  }
  .editor {
    width: min(500px, 94vw);
    max-height: 84vh;
    overflow-y: auto;
    background: var(--surface-overlay);
    border-radius: 14px;
    box-shadow: var(--shadow-dropdown);
    border: 0.5px solid var(--separator);
    padding: 12px 16px 14px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 12px;
    font-weight: 600;
    color: var(--content-subtle);
    text-transform: uppercase;
    letter-spacing: 0.4px;
  }
  .title {
    border: none;
    outline: none;
    background: none;
    font-size: 19px;
    font-weight: 600;
    padding: 4px 0;
    user-select: text;
  }
  .hint {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--accent);
    background: var(--accent-soft);
    border-radius: 8px;
    padding: 4px 9px;
    align-self: flex-start;
  }
  .grid {
    display: grid;
    grid-template-columns: 78px 1fr;
    gap: 8px 10px;
    align-items: center;
    margin-top: 4px;
  }
  .grid label,
  .lbl {
    font-size: 12px;
    color: var(--content-subtle);
    text-align: right;
  }
  .grid .switch {
    text-align: left;
    display: flex;
    gap: 6px;
    align-items: center;
    color: var(--content);
    font-size: 13px;
  }
  .dt {
    display: flex;
    gap: 8px;
  }
  .dt .input {
    flex: 1;
    min-width: 0;
  }
  .notes {
    min-height: 70px;
    resize: vertical;
    padding: 8px 10px;
    user-select: text;
  }
  .foot {
    display: flex;
    gap: 8px;
    margin-top: 6px;
  }
  .grow {
    flex: 1;
  }
  :global(html[data-theme="dark"]) .dt .input {
    color-scheme: dark;
  }
</style>
