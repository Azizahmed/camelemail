<script lang="ts">
  import { dateLocale } from "../lib/format";
  // The phone calendar: an agenda, or one or three days on an hour grid under
  // a swipeable week strip. Events open in a bottom sheet; new and edited
  // events use the desktop editor laid out full-screen. Shares `cal` (events,
  // calendars, weather, prefs) and the engine's change events with the desktop
  // CalendarView, which isn't mounted on phones.
  import { ArrowLeft, Check, ChevronDown, ChevronLeft, ChevronRight, Columns3, Eye, List, MoreVertical, Plus, RectangleVertical, RefreshCw, Settings } from "@lucide/svelte";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import { api, errorText } from "../lib/api";
  import { t } from "../lib/i18n.svelte";
  import {
    addDays,
    addMonths,
    cal,
    calApi,
    deleteEvent,
    eventsByDay,
    sameDay,
    startOfDay,
    type CalEvent,
    type CalendarPrefs,
  } from "../lib/calendar.svelte";
  import type { AppSettings, EngineEvent } from "../lib/types";
  import { toast, toastError } from "../lib/ui.svelte";
  import EventEditor from "../components/calendar/EventEditor.svelte";
  import MiniMonth from "../components/calendar/MiniMonth.svelte";
  import TimeView from "../components/calendar/TimeView.svelte";
  import EventSheet from "./calendar/EventSheet.svelte";
  import PhoneAgenda from "./calendar/PhoneAgenda.svelte";
  import WeekStrip from "./calendar/WeekStrip.svelte";
  import { PHONE_VIEWS, loadRange, periodDays, periodStep, type PhoneView } from "./calendar/model";
  import { swipe } from "./calendar/swipe";
  import { back, onLeave, push, type Screen } from "./nav.svelte";
  import Sheet from "./Sheet.svelte";

  type EditorMode = { kind: "new"; start: Date; end: Date; allDay: boolean } | { kind: "edit"; event: CalEvent };

  const VIEW_KEY = "remail.mobile.calendarView";
  const savedView = typeof localStorage !== "undefined" ? localStorage.getItem(VIEW_KEY) : null;
  let view = $state<PhoneView>(PHONE_VIEWS.some(([v]) => v === savedView) ? (savedView as PhoneView) : "agenda");

  let error = $state("");
  /** Settings and calendars are in, so the first fetch uses the right week start. */
  let ready = $state(false);
  let now = $state(Date.now());
  const todayStart = $derived(startOfDay(new Date(now)));
  const prefs = $derived<CalendarPrefs>(cal.prefs ?? { weekStart: 0, weekNumbers: true, weather: null, units: "c", defaultCalendar: null, view: "month" });

  /** Days the agenda lists; grows as it scrolls. */
  let agendaCount = $state(60);
  /** The day at the top of the agenda (for the title). */
  let agendaTop = $state<Date | null>(null);
  /** Bumped by Today so the hour grid scrolls back to now. */
  let jump = $state(0);

  let picker = $state(false);
  let pickerMonth = $state(new Date(cal.selected.getFullYear(), cal.selected.getMonth(), 1));
  let viewMenu = $state(false);
  let moreMenu = $state(false);
  let calsMenu = $state(false);
  let moreSheet: Sheet | undefined = $state();
  let detail = $state<CalEvent | null>(null);
  let detailOpen = $state(false);
  let editor = $state<EditorMode | null>(null);
  let editorEntry: Screen | null = null;
  let drag = $state(0);

  const shown = $derived(view === "agenda" ? (agendaTop ?? cal.selected) : cal.selected);
  const titleMonth = $derived(shown.toLocaleDateString(dateLocale(), { month: "long" }));
  const days = $derived(periodDays(view, cal.selected));
  /** Open the hour grid at the current time when today is shown. */
  const scrollHour = $derived(days.some((d) => sameDay(d, todayStart)) ? Math.max(0, new Date(now).getHours() - 1.5) : 7.5);
  const ViewIcon = $derived(view === "agenda" ? List : view === "day" ? RectangleVertical : Columns3);

  // ------------------------------------------------------------------ data

  const enabledKey = $derived(cal.calendars.map((c) => `${c.id}:${c.enabled ? 1 : 0}`).join());

  let loadSeq = 0;
  /** The range and calendars last asked for, so moving within them doesn't refetch. */
  let wanted: { from: number; to: number; key: string } | null = null;
  async function load(from: number, to: number, force = false) {
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
  const range = $derived(loadRange(cal.selected, prefs.weekStart, agendaCount, picker ? pickerMonth : null));
  const reload = (force = false) => load(range[0], range[1], force);

  $effect(() => {
    const [from, to] = range;
    void enabledKey;
    if (ready) void load(from, to);
  });

  /** The loaded events per local day. */
  const byDay = $derived(eventsByDay(cal.events, new Date(cal.range[0]), new Date(cal.range[1])));

  async function loadMeta() {
    try {
      [cal.calendars, cal.accounts] = await Promise.all([calApi.calendars(), calApi.accounts()]);
    } catch (e) {
      error = errorText(e);
    }
  }

  async function loadWeather() {
    if (!cal.prefs?.weather) {
      cal.weather = {};
      return;
    }
    try {
      const list = await calApi.weather();
      cal.weather = Object.fromEntries(list.map((d) => [d.date, d]));
    } catch {
      /* weather is decoration */
    }
  }

  function applySettings(s: AppSettings) {
    cal.settings = s;
    const hadWeather = JSON.stringify(cal.prefs?.weather ?? null);
    cal.prefs = (s as AppSettings & { calendar: CalendarPrefs }).calendar;
    if (JSON.stringify(cal.prefs?.weather ?? null) !== hadWeather || cal.prefs?.units) void loadWeather();
  }

  onMount(() => {
    // The phone opens on today.
    cal.selected = todayStart;
    cal.anchor = todayStart;
    const un: (() => void)[] = [];
    let disposed = false;
    const keep = (u: () => void) => (disposed ? u() : un.push(u));
    (async () => {
      applySettings(await api.getSettings());
      await loadMeta();
      ready = true;
      keep(await listen<AppSettings>("settings-changed", (e) => applySettings(e.payload)));
      keep(
        await listen<EngineEvent>("engine-event", (e) => {
          // Every change (sync, edits, shown calendars) lands here.
          if ((e.payload as { type: string }).type === "calendarChanged") void loadMeta().then(() => reload(true));
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

  async function syncNow() {
    if (cal.syncing) return;
    cal.syncing = true;
    try {
      await calApi.sync();
    } catch (e) {
      toastError(e, t("m.cal.syncFailed"));
    } finally {
      cal.syncing = false;
    }
  }

  // ------------------------------------------------------------ navigation

  function select(d: Date) {
    cal.selected = startOfDay(d);
    cal.anchor = cal.selected;
    agendaTop = null;
  }

  function today() {
    select(todayStart);
    pickerMonth = new Date(todayStart.getFullYear(), todayStart.getMonth(), 1);
    agendaCount = 60;
    jump++;
  }

  function setView(v: PhoneView) {
    view = v;
    agendaTop = null;
    try {
      localStorage.setItem(VIEW_KEY, v);
    } catch {
      /* private mode */
    }
  }

  function step(dir: -1 | 1) {
    select(addDays(cal.selected, dir * periodStep(view)));
  }

  function togglePicker() {
    if (!picker) pickerMonth = new Date(shown.getFullYear(), shown.getMonth(), 1);
    picker = !picker;
  }

  // Back closes the month picker.
  let pickerEntry: Screen | null = null;
  $effect(() => {
    if (picker && !pickerEntry) {
      const s: Screen = { name: "sheet", id: "calendar-month" };
      pickerEntry = s;
      push(s);
      onLeave(s, () => {
        pickerEntry = null;
        picker = false;
      });
    } else if (!picker && pickerEntry) {
      void back();
    }
  });

  async function pickFromMonth(d: Date) {
    select(d);
    picker = false;
  }

  // ---------------------------------------------------------------- events

  function open(e: CalEvent) {
    detail = e;
    detailOpen = true;
  }

  function openEditor(m: EditorMode) {
    const s: Screen = { name: "sheet", id: "event-editor" };
    editor = m;
    editorEntry = s;
    push(s);
    onLeave(s, () => {
      editorEntry = null;
      editor = null;
    });
  }

  function closeEditor() {
    if (editorEntry) void back();
    else editor = null;
  }

  function newEvent(start?: Date, allDay = false) {
    if (!cal.calendars.some((c) => c.enabled && !c.readOnly)) {
      const msg = cal.calendars.some((c) => !c.readOnly)
        ? t("m.cal.showFirst")
        : cal.calendars.length
          ? t("m.cal.readOnly")
          : t("m.cal.connectFirst");
      toast(msg, { action: { label: t("m.toast.openSettings"), run: () => void api.openSettings("calendar") } });
      return;
    }
    const base = view === "agenda" ? shown : cal.selected;
    let s = start ?? new Date(base.getFullYear(), base.getMonth(), base.getDate(), new Date().getHours() + 1);
    if (!start && !sameDay(base, new Date())) s = new Date(s.getFullYear(), s.getMonth(), s.getDate(), 9);
    openEditor(
      allDay
        ? { kind: "new", start: startOfDay(s), end: addDays(startOfDay(s), 1), allDay: true }
        : { kind: "new", start: s, end: new Date(s.getTime() + 3600_000), allDay: false },
    );
  }

  async function del(e: CalEvent) {
    await deleteEvent(e);
  }

  // ----------------------------------------------------------------- menus

  async function fromMore(fn: () => void) {
    await moreSheet?.close();
    fn();
  }

  function toggleCalendar(id: string) {
    const c = cal.calendars.find((x) => x.id === id);
    if (!c) return;
    const was = c.enabled;
    c.enabled = !was;
    calApi.setCalendar(id, !was, null).catch((err) => {
      c.enabled = was;
      toastError(err);
    });
  }

  const groups = $derived.by(() => {
    const g: Record<string, typeof cal.calendars> = {};
    for (const c of cal.calendars) (g[c.accountName] ??= []).push(c);
    return Object.entries(g);
  });
</script>

<div class="m-screen calendar">
  <header class="m-topbar">
    <button class="m-icon-btn" aria-label={t("common.back")} onclick={() => back()}><ArrowLeft size={20} strokeWidth={1.6} /></button>
    <button class="title" aria-expanded={picker} aria-label={t("m.cal.chooseDate")} onclick={togglePicker}>
      <span class="month">{titleMonth}</span>
      {#if shown.getFullYear() !== todayStart.getFullYear()}<span class="year">{shown.getFullYear()}</span>{/if}
      <span class="chev" class:open={picker}><ChevronDown size={16} strokeWidth={1.8} /></span>
    </button>
    <button class="m-icon-btn" aria-label={t("m.cal.today")} onclick={today}>
      <span class="today-icon">{todayStart.getDate()}</span>
    </button>
    <button class="m-icon-btn" aria-label={t("m.cal.viewIs", { view: t(PHONE_VIEWS.find(([v]) => v === view)?.[1] ?? "m.cal.agenda") })} onclick={() => (viewMenu = true)}><ViewIcon size={19} strokeWidth={1.6} /></button>
    <button class="m-icon-btn" aria-label={t("m.home.more")} onclick={() => (moreMenu = true)}><MoreVertical size={19} strokeWidth={1.6} /></button>
  </header>

  {#if picker}
    <div class="picker" use:swipe={{ onstep: (d) => (pickerMonth = addMonths(pickerMonth, d)) }}>
      <div class="picker-head">
        <span class="pm">{pickerMonth.toLocaleDateString(dateLocale(), { month: "long", year: "numeric" })}</span>
        <button class="m-icon-btn small" aria-label={t("m.cal.prevMonth")} onclick={() => (pickerMonth = addMonths(pickerMonth, -1))}><span class="flip"><ChevronLeft size={18} strokeWidth={1.6} /></span></button>
        <button class="m-icon-btn small" aria-label={t("m.cal.nextMonth")} onclick={() => (pickerMonth = addMonths(pickerMonth, 1))}><span class="flip"><ChevronRight size={18} strokeWidth={1.6} /></span></button>
      </div>
      <MiniMonth month={pickerMonth} selected={cal.selected} weekStart={prefs.weekStart} weekNumbers={false} highlightWeek={false} {byDay} today={todayStart} onpick={pickFromMonth} />
    </div>
  {/if}

  {#if error}
    <div class="banner">{error}</div>
  {/if}

  {#if ready && cal.calendars.length === 0 && cal.accounts.length === 0}
    <div class="m-empty connect">
      <h2>{t("m.cal.connectTitle")}</h2>
      <p>{t("m.cal.connectBody")}</p>
      <button class="m-primary" onclick={() => api.openSettings("calendar")}>{t("m.cal.connect")}</button>
    </div>
  {:else if view === "agenda"}
    <PhoneAgenda
      from={cal.selected}
      count={agendaCount}
      {byDay}
      today={todayStart}
      weather={cal.weather}
      units={prefs.units}
      syncing={cal.syncing}
      onopen={open}
      onpick={(d) => { select(d); setView("day"); }}
      onmore={() => { if (agendaCount < 720) agendaCount += 60; }}
      onrefresh={syncNow}
      onvisible={(d) => (agendaTop = d)}
    />
  {:else}
    <WeekStrip selected={cal.selected} span={days.length} weekStart={prefs.weekStart} today={todayStart} {byDay} onpick={select} onweek={(d) => select(addDays(cal.selected, d * 7))} />
    <div class="daygrid" use:swipe={{ onstep: step, ondrag: (dx) => (drag = dx) }}>
      <div class="grid-inner" class:dragging={drag !== 0} style:transform={drag ? `translateX(${drag * 0.5}px)` : null}>
        {#key `${view}:${jump}`}
          <TimeView
            {days}
            {byDay}
            weather={cal.weather}
            units={prefs.units}
            hourHeight={46}
            {scrollHour}
            tapToCreate
            onopen={open}
            oncreate={(s, allDay) => newEvent(s, allDay)}
            onpick={(d) => { select(d); setView("day"); }}
          />
        {/key}
      </div>
    </div>
  {/if}

  <button class="fab" aria-label={t("m.cal.newEvent")} onclick={() => newEvent()}><Plus size={22} strokeWidth={1.6} /></button>
</div>

<EventSheet event={detail} bind:open={detailOpen} onedit={(e) => openEditor({ kind: "edit", event: e })} ondelete={del} />

<Sheet bind:open={viewMenu} title={t("m.cal.view")}>
  {#each PHONE_VIEWS as [v, label] (v)}
    {@const Icon = v === "agenda" ? List : v === "day" ? RectangleVertical : Columns3}
    <button class="m-sheet-item" class:on={view === v} onclick={() => { setView(v); viewMenu = false; }}>
      <Icon size={18} strokeWidth={1.5} /> {t(label)}
      {#if view === v}<small><Check size={16} strokeWidth={1.8} /></small>{/if}
    </button>
  {/each}
</Sheet>

<Sheet bind:this={moreSheet} bind:open={moreMenu}>
  <button class="m-sheet-item" onclick={() => { moreMenu = false; void syncNow(); }}>
    <span class="sync" class:spin={cal.syncing}><RefreshCw size={18} strokeWidth={1.5} /></span> {t("m.cal.sync")}
  </button>
  <button class="m-sheet-item" onclick={() => fromMore(() => (calsMenu = true))}>
    <Eye size={18} strokeWidth={1.5} /> {t("m.cal.calendars")}
    <small>{cal.calendars.filter((c) => c.enabled).length}/{cal.calendars.length}</small>
  </button>
  <div class="m-sheet-sep"></div>
  <button class="m-sheet-item" onclick={() => fromMore(() => void api.openSettings("calendar"))}><Settings size={18} strokeWidth={1.5} /> {t("m.cal.settings")}</button>
</Sheet>

<Sheet bind:open={calsMenu} title={t("m.cal.shown")}>
  {#each groups as [acct, list] (acct)}
    <div class="acct"><bdi>{acct}</bdi></div>
    {#each list as c (c.id)}
      <button class="m-sheet-item cal-row" role="menuitemcheckbox" aria-checked={c.enabled} onclick={() => toggleCalendar(c.id)}>
        <span class="box" class:on={c.enabled} style:--c={c.color}>{#if c.enabled}<Check size={13} strokeWidth={2.6} />{/if}</span>
        <span class="cname"><bdi>{c.name}</bdi></span>
        {#if c.readOnly}<small>{t("m.cal.readOnlyShort")}</small>{/if}
      </button>
    {/each}
  {:else}
    <div class="m-empty small">{t("m.cal.none")}</div>
  {/each}
</Sheet>

{#if editor}
  <div class="ed-host">
    <EventEditor mode={editor} onclose={closeEditor} onsaved={closeEditor} />
  </div>
{/if}

<style>
  /* The shared calendar parts (hour grid, month, editor) take the phone's colours. */
  .calendar,
  .ed-host {
    --accent: var(--m-accent);
    --accent-soft: var(--m-accent-soft);
    --surface-reading: var(--m-surface);
    --surface-overlay: var(--m-bg);
    --content: var(--m-text);
    --content-subtle: var(--m-text-2);
    --content-muted: var(--m-text-3);
    --separator: var(--m-separator);
    --separator-subtle: var(--m-separator);
    --cal-muted: var(--m-text-3);
    --cal-hover: var(--m-pill);
  }
  .flip {
    display: inline-flex;
  }
  :global([dir="rtl"]) .flip {
    transform: scaleX(-1);
  }
  .title {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 5px;
    height: 39px;
    padding: 0 8px;
    border: 0;
    border-radius: 20px;
    background: transparent;
    color: var(--m-text);
    font: inherit;
    white-space: nowrap;
  }
  .title:active {
    background: var(--m-pill);
  }
  .month {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: 18px;
    font-weight: 500;
  }
  .year {
    font-size: 18px;
    color: var(--m-text-2);
  }
  .chev {
    display: inline-flex;
    color: var(--m-text-2);
    transition: transform 0.15s ease;
  }
  .chev.open {
    transform: rotate(180deg);
  }
  .today-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    padding-top: 2px;
    border: 1.6px solid currentColor;
    border-top-width: 3px;
    border-radius: 5px;
    font-size: 10px;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }
  .picker {
    flex: none;
    margin: 0 10px 8px;
    padding: 3px 8px 10px;
    border-radius: 20px;
    background: var(--m-surface);
    touch-action: pan-y;
    animation: m-fade 0.15s ease;
  }
  .picker-head {
    display: flex;
    align-items: center;
    padding: 0 0 3px;
    padding-inline-start: 8px;
  }
  .pm {
    flex: 1;
    font-size: 14px;
    font-weight: 500;
  }
  .m-icon-btn.small {
    width: 34px;
    height: 34px;
  }
  .picker :global(.mini) {
    gap: 2px;
  }
  .picker :global(.mini .head) {
    font-size: 10px;
    padding-bottom: 3px;
  }
  .picker :global(.mini .day) {
    font-size: 12px;
    font-weight: 500;
  }
  .picker :global(.mini .num) {
    width: 30px;
    height: 30px;
    line-height: 30px;
  }
  .picker :global(.mini .selected .num) {
    box-shadow: none;
    background: var(--m-accent-soft);
    color: var(--m-accent);
  }
  /* Today keeps its filled circle and white number, selected or not. */
  .picker :global(.mini .today .num) {
    color: #fff;
  }
  .banner {
    margin: 0 11px 7px;
    padding: 8px 11px;
    border-radius: 13px;
    background: color-mix(in srgb, var(--m-swipe-delete) 12%, transparent);
    color: var(--m-swipe-delete);
    font-size: 11px;
  }
  .connect h2 {
    margin: 0 0 8px;
    font-size: 17px;
    font-weight: 500;
    color: var(--m-text);
  }
  .connect p {
    margin: 0 0 18px;
    line-height: 1.5;
  }

  /* The hour grid: a white card under the week strip. */
  .daygrid {
    flex: 1;
    min-height: 0;
    display: flex;
    overflow: hidden;
    border-radius: 20px 20px 0 0;
    background: var(--m-surface);
    touch-action: pan-y;
  }
  .grid-inner {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    transition: transform 0.18s ease-out;
  }
  .grid-inner.dragging {
    transition: none;
  }
  .daygrid :global(.tv .gutter) {
    width: 43px;
  }
  .daygrid :global(.tv .dh) {
    flex-wrap: wrap;
    row-gap: 2px;
    overflow: hidden;
    padding: 8px 7px 7px;
    font-size: 11px;
    gap: 5px;
  }
  .daygrid :global(.tv .dh .dn) {
    font-size: 16px;
    font-weight: 500;
  }
  .daygrid :global(.tv .dh .wx) {
    margin-left: 0;
    margin-inline-start: auto;
    font-size: 10px;
  }
  .daygrid :global(.tv .hl),
  .daygrid :global(.tv .lbl) {
    font-size: 10px;
  }
  .daygrid :global(.tv .block) {
    border-radius: 8px;
    padding: 3px 7px;
  }
  .daygrid :global(.tv .bt) {
    font-size: 11px;
    font-weight: 500;
  }
  .daygrid :global(.tv .bm) {
    font-size: 10px;
  }
  .daygrid :global(.tv .bar) {
    border-radius: 5px;
    font-size: 11px;
  }
  .daygrid :global(.tv .col.today) {
    background: transparent;
  }

  .fab {
    position: fixed;
    inset-inline-end: 13px;
    bottom: 13px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 48px;
    height: 48px;
    border: 0;
    border-radius: 50%;
    background: var(--m-fab);
    color: #fff;
    box-shadow: var(--m-shadow);
  }
  .fab:active {
    filter: brightness(1.15);
  }

  .sync {
    display: inline-flex;
  }
  .spin {
    animation: spin 1s linear infinite;
  }
  .acct {
    margin: 10px 25px 3px;
    font-size: 11px;
    color: var(--m-text-2);
  }
  .cal-row {
    min-height: 43px;
  }
  .cname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .box {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    border: 2px solid var(--c);
    border-radius: 6px;
    color: #fff;
  }
  .box.on {
    background: var(--c);
  }
  .m-empty.small {
    padding: 20px;
  }

  /* The desktop's event dialog, full-screen. */
  .ed-host :global(.scrim) {
    padding: 0;
    align-items: stretch;
    background: var(--m-bg);
    animation: m-slide 0.18s ease-out;
  }
  .ed-host :global(.editor) {
    width: 100%;
    max-height: none;
    border: 0;
    border-radius: 0;
    box-shadow: none;
    background: var(--m-bg);
    color: var(--m-text);
    padding: 7px 15px 15px;
    gap: 10px;
  }
  .ed-host :global(.editor .head) {
    min-height: 46px;
    font-size: 11px;
    letter-spacing: 0.3px;
  }
  .ed-host :global(.editor .head .icon-btn) {
    width: 36px;
    height: 36px;
    border-radius: 50%;
  }
  .ed-host :global(.editor .head .icon-btn svg) {
    width: 20px;
    height: 20px;
  }
  .ed-host :global(.editor .title) {
    font-size: 20px;
    font-weight: 500;
    padding: 7px 3px;
    border-bottom: 1px solid var(--m-separator);
  }
  .ed-host :global(.editor .hint) {
    font-size: 11px;
    padding: 5px 10px;
    border-radius: 11px;
  }
  .ed-host :global(.editor .grid) {
    grid-template-columns: 1fr;
    gap: 5px;
    padding: 7px 13px 13px;
    border-radius: 20px;
    background: var(--m-surface);
  }
  .ed-host :global(.editor .grid > label),
  .ed-host :global(.editor .grid > .lbl) {
    margin-top: 8px;
    text-align: start;
    font-size: 11px;
    color: var(--m-text-2);
  }
  .ed-host :global(.editor .grid > .lbl) {
    display: none;
  }
  .ed-host :global(.editor .grid .switch) {
    min-height: 36px;
    font-size: 14px;
    gap: 10px;
    color: var(--m-text);
  }
  .ed-host :global(.editor .grid .switch input) {
    width: 18px;
    height: 18px;
  }
  .ed-host :global(.editor .input),
  .ed-host :global(.editor .select) {
    height: 39px;
    padding: 0 11px;
    border-radius: 11px;
    font-size: 14px;
    background: var(--m-surface-2);
    border-color: transparent;
    color: var(--m-text);
  }
  .ed-host :global(.editor .notes) {
    height: auto;
    min-height: 90px;
    padding: 10px 11px;
  }
  .ed-host :global(.editor .foot) {
    position: sticky;
    bottom: 0;
    margin: 5px -18px -18px;
    padding: 10px 15px 15px;
    gap: 8px;
    background: var(--m-bg);
  }
  .ed-host :global(.editor .foot .btn) {
    height: 39px;
    padding: 0 16px;
    border-radius: 20px;
    font-size: 14px;
  }
  .ed-host :global(.menu) {
    border-radius: 15px;
  }
  .ed-host :global(.menu .menu-item) {
    font-size: 12px;
  }
</style>
