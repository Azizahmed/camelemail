<script lang="ts">
  import { ChevronDown, ChevronLeft, ChevronRight, PanelLeft, Plus, RefreshCw, Search, Settings2, SquareCheck, X } from "@lucide/svelte";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import { api, errorText } from "../../lib/api";
  import {
    DAY,
    addDays,
    addMonths,
    cal,
    calApi,
    daysBetween,
    deleteEvent,
    fmtRange,
    sameDay,
    startOfDay,
    startOfWeek,
    eventsByDay,
    type CalEvent,
    type CalView,
    type CalendarPrefs,
  } from "../../lib/calendar.svelte";
  import type { AppSettings, EngineEvent } from "../../lib/types";
  import { mail } from "../../lib/mail.svelte";
  import { isTyping, toast, toastError } from "../../lib/ui.svelte";
  import Popover from "../menus/Popover.svelte";
  import SpaceSwitch from "../SpaceSwitch.svelte";
  import Agenda from "./Agenda.svelte";
  import EventEditor from "./EventEditor.svelte";
  import EventPopover from "./EventPopover.svelte";
  import GridView from "./GridView.svelte";
  import MiniMonth from "./MiniMonth.svelte";
  import TimeView from "./TimeView.svelte";
  import YearView from "./YearView.svelte";

  const VIEWS: [CalView, string][] = [
    ["day", "Day"],
    ["week", "Week"],
    ["month", "Month"],
    ["quarter", "Quarter"],
    ["year", "Year"],
  ];

  let popover = $state<{ event: CalEvent; anchor: DOMRect } | null>(null);
  let editor = $state<null | { kind: "new"; start: Date; end: Date; allDay: boolean } | { kind: "edit"; event: CalEvent }>(null);
  let setMenu = $state<DOMRect | null>(null);
  let searchEl: HTMLInputElement | undefined = $state();
  let sideMonth = $state(new Date(cal.anchor.getFullYear(), cal.anchor.getMonth(), 1));
  let error = $state("");
  /** Settings and calendars are in, so the first fetch uses the right week start. */
  let ready = $state(false);
  let now = $state(Date.now());
  const todayMs = $derived(startOfDay(new Date(now)).getTime());
  /** Start of the current day; moves at midnight. */
  const todayStart = $derived(new Date(todayMs));

  const prefs = $derived<CalendarPrefs>(
    cal.prefs ?? { weekStart: 0, weekNumbers: true, weather: null, units: "c", defaultCalendar: null, view: "month" },
  );

  // ------------------------------------------------------------------ ranges

  /** First day and row starts of the current view. */
  const viewRange = $derived.by((): { from: Date; to: Date; rows: Date[] } => {
    const a = cal.anchor;
    const ws = prefs.weekStart;
    switch (cal.view) {
      case "day":
        return { from: startOfDay(a), to: addDays(startOfDay(a), 1), rows: [] };
      case "week": {
        const s = startOfWeek(a, ws);
        return { from: s, to: addDays(s, 7), rows: [] };
      }
      case "month": {
        const first = new Date(a.getFullYear(), a.getMonth(), 1);
        const s = startOfWeek(first, ws);
        const last = new Date(a.getFullYear(), a.getMonth() + 1, 0);
        const n = Math.round(daysBetween(s, startOfWeek(last, ws)) / 7) + 1;
        const rows = Array.from({ length: n }, (_, i) => addDays(s, i * 7));
        return { from: s, to: addDays(s, n * 7), rows };
      }
      case "quarter": {
        const q = new Date(a.getFullYear(), Math.floor(a.getMonth() / 3) * 3, 1);
        const s = startOfWeek(q, ws);
        const end = new Date(q.getFullYear(), q.getMonth() + 3, 0);
        const n = Math.round(daysBetween(s, startOfWeek(end, ws)) / 7) + 1;
        const rows = Array.from({ length: n }, (_, i) => addDays(s, i * 7));
        return { from: s, to: addDays(s, n * 7), rows };
      }
      case "year": {
        const s = startOfWeek(new Date(a.getFullYear(), 0, 1), ws);
        return { from: s, to: addDays(new Date(a.getFullYear(), 11, 31), 14), rows: [] };
      }
    }
  });

  const title = $derived.by(() => {
    const a = cal.anchor;
    switch (cal.view) {
      case "day":
        return [a.toLocaleDateString(undefined, { weekday: "long", month: "long", day: "numeric" }), String(a.getFullYear())];
      case "week": {
        const s = viewRange.from;
        const e = addDays(s, 6);
        return [
          s.getMonth() === e.getMonth()
            ? s.toLocaleDateString(undefined, { month: "long" })
            : `${s.toLocaleDateString(undefined, { month: "short" })} – ${e.toLocaleDateString(undefined, { month: "short" })}`,
          String(e.getFullYear()),
        ];
      }
      case "quarter": {
        const q = Math.floor(a.getMonth() / 3);
        return [`Q${q + 1}`, String(a.getFullYear())];
      }
      case "year":
        return ["", String(a.getFullYear())];
      default:
        return [a.toLocaleDateString(undefined, { month: "long" }), String(a.getFullYear())];
    }
  });

  /** Which calendars are shown; the engine only returns their events. */
  const enabledKey = $derived(cal.calendars.map((c) => `${c.id}:${c.enabled ? 1 : 0}`).join());

  let loadSeq = 0;
  /** The range and calendars last asked for, so moving within them doesn't refetch. */
  let wanted: { from: number; to: number; key: string } | null = null;
  /** Fetch the events of the view and the sidebar; `force` refetches after the data changed. */
  async function load(force = false) {
    // The sidebar shows its month and three weeks of agenda too.
    const sideFrom = startOfWeek(sideMonth, prefs.weekStart);
    let from = Math.min(viewRange.from.getTime(), sideFrom.getTime(), startOfDay(cal.selected).getTime());
    let to = Math.max(viewRange.to.getTime(), addDays(sideFrom, 42).getTime(), addDays(cal.selected, 22).getTime());
    // Keep the request within what the engine serves (the year view plus some margin).
    const maxSpan = 760 * DAY;
    if (to - from > maxSpan) {
      from = viewRange.from.getTime();
      to = Math.min(to, from + maxSpan);
    }
    const key = enabledKey;
    if (!force && wanted && wanted.key === key && from >= wanted.from && to <= wanted.to) return;
    const seq = ++loadSeq;
    wanted = { from, to, key };
    try {
      const events = await calApi.events(from, to);
      if (seq !== loadSeq) return;
      cal.events = events;
      cal.range = [from, to];
      error = "";
    } catch (e) {
      if (seq === loadSeq) wanted = null;
      error = errorText(e);
    }
  }

  $effect(() => {
    void viewRange.from.getTime();
    void sideMonth.getTime();
    void cal.selected.getTime();
    void enabledKey;
    if (ready) void load();
  });

  /** The loaded events per local day, built once per load for every view. */
  const byDay = $derived(eventsByDay(cal.events, new Date(cal.range[0]), new Date(cal.range[1])));

  async function loadMeta() {
    try {
      [cal.calendars, cal.accounts] = await Promise.all([calApi.calendars(), calApi.accounts()]);
    } catch (e) {
      error = errorText(e);
    }
  }

  async function loadWeather() {
    if (!prefs.weather) {
      cal.weather = {};
      return;
    }
    try {
      const days = await calApi.weather();
      cal.weather = Object.fromEntries(days.map((d) => [d.date, d]));
    } catch {
      /* weather is decoration */
    }
  }

  function applySettings(s: AppSettings) {
    cal.settings = s;
    const firstLoad = cal.prefs === null;
    const hadWeather = JSON.stringify(cal.prefs?.weather ?? null);
    cal.prefs = (s as AppSettings & { calendar: CalendarPrefs }).calendar;
    if (firstLoad && cal.prefs) cal.view = cal.prefs.view;
    if (JSON.stringify(cal.prefs?.weather ?? null) !== hadWeather || cal.prefs?.units) void loadWeather();
  }

  onMount(() => {
    const un: (() => void)[] = [];
    let disposed = false;
    // Switching back to mail can unmount this before a listener is in place.
    const keep = (u: () => void) => (disposed ? u() : un.push(u));
    (async () => {
      applySettings(await api.getSettings());
      await loadMeta();
      ready = true;
      keep(await listen<AppSettings>("settings-changed", (e) => applySettings(e.payload)));
      keep(
        await listen<EngineEvent>("engine-event", (e) => {
          // Every change (sync, edits, shown calendars) lands here, so the handlers don't reload themselves.
          if ((e.payload as { type: string }).type === "calendarChanged") void loadMeta().then(() => load(true));
        }),
      );
    })()
      .catch((e) => (error = errorText(e)))
      .finally(() => (ready = true));
    const tick = setInterval(() => void loadWeather(), 3600_000);
    const clock = setInterval(() => (now = Date.now()), 60_000);
    return () => {
      disposed = true;
      clearInterval(tick);
      clearInterval(clock);
      un.forEach((u) => u());
    };
  });

  // ------------------------------------------------------------- navigation

  function go(dir: -1 | 1) {
    const a = cal.anchor;
    cal.anchor =
      cal.view === "day"
        ? addDays(a, dir)
        : cal.view === "week"
          ? addDays(a, 7 * dir)
          : cal.view === "month"
            ? addMonths(a, dir)
            : cal.view === "quarter"
              ? addMonths(a, 3 * dir)
              : new Date(a.getFullYear() + dir, a.getMonth(), 1);
    cal.selected = cal.view === "year" ? new Date(cal.anchor.getFullYear(), cal.selected.getMonth(), Math.min(cal.selected.getDate(), 28)) : startOfDay(cal.anchor);
    sideMonth = new Date(cal.selected.getFullYear(), cal.selected.getMonth(), 1);
  }

  function today() {
    const t = startOfDay(new Date());
    cal.anchor = t;
    cal.selected = t;
    sideMonth = new Date(t.getFullYear(), t.getMonth(), 1);
  }

  function setView(v: CalView) {
    cal.view = v;
    cal.anchor = cal.selected;
    cal.results = null;
    if (cal.settings && cal.prefs && cal.prefs.view !== v) {
      void api.setSettings({ ...cal.settings, calendar: { ...cal.prefs, view: v } } as AppSettings).catch(() => {});
    }
  }

  function pick(d: Date) {
    cal.selected = startOfDay(d);
    if (cal.view === "day" || cal.view === "week" || !(d >= viewRange.from && d < viewRange.to)) cal.anchor = startOfDay(d);
    if (cal.view === "year") setView("day");
  }

  function sidePick(d: Date) {
    cal.selected = startOfDay(d);
    cal.anchor = startOfDay(d);
    if (d.getMonth() !== sideMonth.getMonth()) sideMonth = new Date(d.getFullYear(), d.getMonth(), 1);
  }

  function newEvent(start?: Date, allDay = false) {
    if (!cal.calendars.some((c) => c.enabled && !c.readOnly)) {
      const msg = cal.calendars.some((c) => !c.readOnly)
        ? "Show one of your calendars first (All calendars, at the bottom left)"
        : cal.calendars.length
          ? "Subscribed calendars are read-only. Connect a CalDAV calendar to add events."
          : "Connect a calendar first";
      toast(msg, { action: { label: "Open Settings", run: () => void api.openSettings("calendar") } });
      return;
    }
    let s = start ?? new Date(cal.selected.getFullYear(), cal.selected.getMonth(), cal.selected.getDate(), new Date().getHours() + 1);
    if (!start && sameDay(cal.selected, new Date()) === false) s = new Date(s.getFullYear(), s.getMonth(), s.getDate(), 9);
    editor = allDay ? { kind: "new", start: startOfDay(s), end: addDays(startOfDay(s), 1), allDay: true } : { kind: "new", start: s, end: new Date(s.getTime() + 3600_000), allDay: false };
  }

  async function del(e: CalEvent) {
    popover = null;
    await deleteEvent(e);
  }

  async function syncNow() {
    cal.syncing = true;
    try {
      await calApi.sync();
    } catch (e) {
      toastError(e, "Sync failed");
    } finally {
      cal.syncing = false;
    }
  }

  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  function onSearch() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(async () => {
      if (!cal.query.trim()) {
        cal.results = null;
        return;
      }
      try {
        cal.results = await calApi.search(cal.query.trim());
      } catch (e) {
        toastError(e);
      }
    }, 200);
  }

  /** An event is being edited (a dialog that owns the keyboard). */
  export function busy() {
    return editor !== null;
  }

  /** Calendar shortcuts; the main window routes keys here while the calendar is shown. */
  export function handleKey(e: KeyboardEvent) {
    if (editor || document.querySelector(".overlay")) return;
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
      e.preventDefault();
      searchEl?.focus();
      return;
    }
    if ((e.ctrlKey || e.metaKey) && /^[1-5]$/.test(e.key)) {
      e.preventDefault();
      setView(VIEWS[Number(e.key) - 1][0]);
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "r") {
      e.preventDefault();
      void syncNow();
      return;
    }
    if (e.key === "Escape") {
      if (popover) popover = null;
      else if (cal.results) {
        cal.query = "";
        cal.results = null;
      } else if (isTyping(e)) (e.target as HTMLElement).blur();
      return;
    }
    if (isTyping(e) || e.ctrlKey || e.metaKey || e.altKey) return;
    const k = e.key.toLowerCase();
    if (k === "t") today();
    else if (k === "n") newEvent();
    else if (k === "arrowleft" || k === "k") go(-1);
    else if (k === "arrowright" || k === "j") go(1);
    else if (k === "d") setView("day");
    else if (k === "w") setView("week");
    else if (k === "m") setView("month");
    else if (k === "q") setView("quarter");
    else if (k === "y") setView("year");
    else return;
    e.preventDefault();
  }

  const weekDays = $derived(cal.view === "day" ? [startOfDay(cal.anchor)] : Array.from({ length: 7 }, (_, i) => addDays(viewRange.from, i)));
  const enabledCount = $derived(cal.calendars.filter((c) => c.enabled).length);
  const groups = $derived.by(() => {
    const g: Record<string, typeof cal.calendars> = {};
    for (const c of cal.calendars) (g[c.accountName] ??= []).push(c);
    return Object.entries(g);
  });
</script>

<div class="calwin">

  <main class="main">
    <div class="toolbar">
      <div class="lead">
        {#if !mail.sidebarDocked}
          <button class="icon-btn" title="Show sidebar" onclick={() => (mail.sidebarOpen = !mail.sidebarOpen)}><PanelLeft size={16} /></button>
        {/if}
        <SpaceSwitch />
      </div>
      <div class="nav">
        <button class="nb" title="Previous (←)" onclick={() => go(-1)}><ChevronLeft size={16} /></button>
        <button class="nb today" title="Today (T)" onclick={today}>Today</button>
        <button class="nb" title="Next (→)" onclick={() => go(1)}><ChevronRight size={16} /></button>
      </div>
      <div class="vtitle">{title[0]} <span class="yr">{title[1]}</span></div>
      <div class="views" role="tablist">
        {#each VIEWS as [v, l], i (v)}
          <button role="tab" aria-selected={cal.view === v} class:on={cal.view === v} title="{l} (Ctrl+{i + 1})" onclick={() => setView(v)}>{l}</button>
        {/each}
      </div>
      <div class="search">
        <Search size={13} />
        <input bind:this={searchEl} placeholder="Search" bind:value={cal.query} oninput={onSearch} />
        {#if cal.query}<button class="x" title="Clear" onclick={() => { cal.query = ""; cal.results = null; }}><X size={12} /></button>{/if}
      </div>
    </div>

    {#if error}
      <div class="banner">{error}</div>
    {/if}
    {#if cal.calendars.length === 0 && cal.accounts.length === 0}
      <div class="empty">
        <img src="/brand/camel-mail-icon.png" alt="" />
        <h2>Connect your calendar</h2>
        <p>Camel Mail syncs with iCloud, Fastmail, Nextcloud, Zoho, Yahoo and other CalDAV calendars, using the same app password as your mail. You can also subscribe to any calendar with its iCal link, including Google Calendar and Outlook.</p>
        <button class="btn primary" onclick={() => api.openSettings("calendar")}>Connect a calendar</button>
      </div>
    {:else if cal.results}
      <div class="results">
        {#each cal.results as e (e.key)}
          <button class="res" onclick={(ev) => { cal.selected = startOfDay(new Date(e.startMs)); cal.anchor = cal.selected; sideMonth = new Date(cal.selected.getFullYear(), cal.selected.getMonth(), 1); popover = { event: e, anchor: ev.currentTarget.getBoundingClientRect() }; }}>
            <i style:background={e.color}></i>
            <span class="rt" dir="auto">{e.summary || "(No title)"}</span>
            <span class="rd">{fmtRange(e)}</span>
          </button>
        {:else}
          <div class="none">No events match “{cal.query}”.</div>
        {/each}
      </div>
    {:else if cal.view === "month" || cal.view === "quarter"}
      <GridView
        rowStarts={viewRange.rows}
        focusMonth={cal.view === "month" ? new Date(cal.anchor.getFullYear(), cal.anchor.getMonth(), 1) : null}
        {byDay}
        today={todayStart}
        weather={cal.weather}
        weekStart={prefs.weekStart}
        weekNumbers={prefs.weekNumbers}
        fixedRowHeight={cal.view === "quarter" ? 92 : null}
        selected={cal.selected}
        units={prefs.units}
        onpick={pick}
        onopen={(e, r) => (popover = { event: e, anchor: r })}
        oncreate={(d) => newEvent(new Date(d.getFullYear(), d.getMonth(), d.getDate(), 9))}
        onmore={(d) => { cal.selected = startOfDay(d); cal.anchor = cal.selected; setView("day"); }}
      />
    {:else if cal.view === "year"}
      <YearView year={cal.anchor.getFullYear()} selected={cal.selected} weekStart={prefs.weekStart} weekNumbers={false} {byDay} today={todayStart} onpick={pick} />
    {:else}
      {#key cal.view}
        <TimeView
          days={weekDays}
          {byDay}
          weather={cal.weather}
          units={prefs.units}
          onopen={(e, r) => (popover = { event: e, anchor: r })}
          oncreate={(s, allDay) => newEvent(s, allDay)}
          onpick={(d) => { cal.selected = startOfDay(d); cal.anchor = cal.selected; setView("day"); }}
        />
      {/key}
    {/if}
  </main>

  <!-- The mini month, agenda and calendars sit on the right of the grid. -->
  <aside class="side">
    <div class="side-top">
      <span class="grow"></span>
      <button class="side-btn" title="Sync calendars (Ctrl+R)" onclick={syncNow}><span class:spin={cal.syncing}><RefreshCw size={14} /></span></button>
      <button class="side-btn add" title="New event (N)" onclick={() => newEvent()}><Plus size={17} /></button>
    </div>
    <div class="side-title">
      <span class="mt">{sideMonth.toLocaleDateString(undefined, { month: "long" })} <span class="yr">{sideMonth.getFullYear()}</span></span>
      <span class="grow"></span>
      <button class="side-btn" title="Previous month" onclick={() => (sideMonth = addMonths(sideMonth, -1))}><ChevronLeft size={16} /></button>
      <button class="side-btn" title="Next month" onclick={() => (sideMonth = addMonths(sideMonth, 1))}><ChevronRight size={16} /></button>
    </div>
    <div class="side-mini">
      <MiniMonth month={sideMonth} selected={cal.selected} weekStart={prefs.weekStart} weekNumbers={prefs.weekNumbers} {byDay} today={todayStart} onpick={sidePick} />
    </div>
    <div class="side-agenda">
      <Agenda from={cal.selected} {byDay} today={todayStart} weather={cal.weather} units={prefs.units} onopen={(e, r) => (popover = { event: e, anchor: r })} onpick={sidePick} />
    </div>
    <div class="side-foot">
      <SquareCheck size={14} />
      <button class="set-btn" onclick={(e) => (setMenu = e.currentTarget.getBoundingClientRect())}>
        {enabledCount === cal.calendars.length ? "All calendars" : `${enabledCount} of ${cal.calendars.length} calendars`} <ChevronDown size={12} />
      </button>
      <button class="side-btn" title="Calendar settings" onclick={() => api.openSettings("calendar")}><Settings2 size={14} /></button>
    </div>
  </aside>
</div>

{#if popover}
  {@const p = popover}
  <EventPopover event={p.event} anchor={p.anchor} onclose={() => (popover = null)} onedit={() => { popover = null; editor = { kind: "edit", event: p.event }; }} ondelete={() => del(p.event)} />
{/if}
{#if editor}
  <EventEditor mode={editor} onclose={() => (editor = null)} onsaved={() => (editor = null)} />
{/if}
{#if setMenu}
  <Popover anchor={setMenu} width={280} height={360} align="right" onclose={() => (setMenu = null)}>
    <div class="sets">
      {#each groups as [acct, list] (acct)}
        <div class="menu-title">{acct}</div>
        {#each list as c (c.id)}
          <label class="setrow">
            <input type="checkbox" checked={c.enabled} onchange={(e) => { const box = e.currentTarget; calApi.setCalendar(c.id, box.checked, null).catch((err) => { box.checked = c.enabled; toastError(err); }); }} />
            <i style:background={c.color}></i>
            <span>{c.name}</span>
            {#if c.readOnly}<span class="ro">read-only</span>{/if}
          </label>
        {/each}
      {:else}
        <div class="none">No calendars yet.</div>
      {/each}
      <div class="menu-sep"></div>
      <button class="menu-item" onclick={() => { setMenu = null; api.openSettings("calendar"); }}>Calendar accounts…</button>
    </div>
  </Popover>
{/if}

<style>
  .calwin {
    flex: 1;
    min-height: 0;
    display: flex;
    background: var(--surface-reading);
    color: var(--content);
  }
  .side {
    width: 258px;
    flex: none;
    display: flex;
    flex-direction: column;
    background: #1c1c1e;
    color: #f2f2f7;
    border-left: 0.5px solid #000;
    --cal-muted: #8e8e93;
    --cal-hover: rgba(255, 255, 255, 0.12);
    --cal-week-bg: rgba(255, 255, 255, 0.1);
  }
  .side-top {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 46px;
    padding: 8px 12px 2px 14px;
  }
  .side-btn {
    border: none;
    background: none;
    color: #c7c7cc;
    width: 28px;
    height: 28px;
    border-radius: 7px;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .side-btn:hover {
    background: rgba(255, 255, 255, 0.1);
    color: #fff;
  }
  .side-btn.add {
    background: rgba(255, 255, 255, 0.12);
    color: #fff;
  }
  .spin {
    display: flex;
    animation: spin 1s linear infinite;
  }
  .side-title {
    display: flex;
    align-items: center;
    padding: 6px 10px 6px 14px;
  }
  .mt {
    font-size: 22px;
    font-weight: 700;
    letter-spacing: -0.3px;
  }
  .yr {
    color: var(--accent);
    font-weight: 600;
  }
  .side-mini {
    padding: 0 8px 8px;
    border-bottom: 0.5px solid rgba(255, 255, 255, 0.12);
  }
  .side-agenda {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding-top: 10px;
  }
  .side-foot {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 10px 10px 14px;
    border-top: 0.5px solid rgba(255, 255, 255, 0.12);
    color: #aeaeb2;
  }
  .set-btn {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 4px;
    border: none;
    background: none;
    color: #d1d1d6;
    font-size: 12px;
  }
  .grow {
    flex: 1;
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 14px;
    overflow: hidden;
    min-height: 46px;
    padding: 8px 14px 8px 16px;
    border-bottom: 0.5px solid var(--separator);
  }
  .lead {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .nav {
    flex: none;
    display: flex;
    border: 0.5px solid var(--separator);
    border-radius: 8px;
    overflow: hidden;
  }
  .nb {
    border: none;
    background: var(--button-surface);
    color: var(--content);
    height: 28px;
    padding: 0 8px;
    display: flex;
    align-items: center;
    font-size: 12.5px;
  }
  .nb:hover {
    background: var(--button-surface-hover);
  }
  .nb.today {
    padding: 0 14px;
    border-left: 0.5px solid var(--separator);
    border-right: 0.5px solid var(--separator);
  }
  .vtitle {
    font-size: 17px;
    font-weight: 700;
    flex: 0 1 150px;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .views {
    flex: none;
    margin: 0 auto;
    display: flex;
    gap: 2px;
  }
  .views button {
    border: none;
    background: none;
    color: var(--content-subtle);
    padding: 4px 13px;
    border-radius: 7px;
    font-size: 13px;
  }
  .views button:hover {
    color: var(--content);
    background: var(--surface-hover);
  }
  .views button.on {
    background: var(--accent);
    color: #fff;
    font-weight: 600;
  }
  .search {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: 0 1 190px;
    min-width: 110px;
    height: 28px;
    padding: 0 8px;
    border-radius: 8px;
    background: var(--surface-hover);
    color: var(--content-subtle);
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: none;
    outline: none;
    background: none;
    font-size: 12.5px;
    user-select: text;
  }
  .x {
    border: none;
    background: none;
    color: inherit;
    display: flex;
    padding: 0;
  }
  .banner {
    margin: 8px 14px 0;
    padding: 8px 12px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--indicator-error) 10%, transparent);
    color: var(--indicator-error);
    font-size: 12.5px;
  }
  .empty {
    margin: auto;
    max-width: 420px;
    text-align: center;
    color: var(--content-subtle);
  }
  .empty img {
    width: 64px;
    border-radius: 15px;
  }
  .empty h2 {
    color: var(--content);
    font-size: 18px;
    margin: 12px 0 6px;
  }
  .empty p {
    font-size: 13px;
    line-height: 19px;
    margin: 0 0 16px;
  }
  .results {
    flex: 1;
    overflow-y: auto;
    padding: 10px 18px;
  }
  .res {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 10px;
    border: none;
    background: none;
    padding: 9px 8px;
    border-bottom: 0.5px solid var(--separator-subtle);
    text-align: left;
    color: var(--content);
  }
  .res:hover {
    background: var(--surface-hover);
  }
  .res i {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    flex: none;
  }
  .rt {
    flex: 1;
    font-weight: 500;
    text-align: start;
  }
  .rd {
    font-size: 12px;
    color: var(--content-subtle);
  }
  .none {
    padding: 14px 8px;
    color: var(--content-subtle);
    font-size: 12.5px;
  }
  .sets {
    max-height: 340px;
    overflow-y: auto;
  }
  .setrow {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 8px;
    font-size: 13px;
    border-radius: 6px;
  }
  .setrow:hover {
    background: var(--surface-hover);
  }
  .setrow i {
    width: 10px;
    height: 10px;
    border-radius: 3px;
  }
  .ro {
    margin-left: auto;
    font-size: 11px;
    color: var(--content-muted);
  }
</style>
