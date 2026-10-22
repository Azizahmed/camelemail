<script module lang="ts">
  import { dateLocale } from "../../lib/format";
  const WEEKDAY = { format: (d: Date) => d.toLocaleDateString(dateLocale(), { weekday: "short" }) };
  const MONTH = { format: (d: Date) => d.toLocaleDateString(dateLocale(), { month: "long", year: "numeric" }) };
</script>

<script lang="ts">
  // The phone's agenda: the days from a date with their events as cards,
  // weather on the day, more days as it scrolls, pull down to sync.
  import { MapPin, Repeat } from "@lucide/svelte";
  import { fmtTime, isBar, sameDay, temp, type CalEvent, type DayWeather } from "../../lib/calendar.svelte";
  import WeatherIcon from "../../components/calendar/WeatherIcon.svelte";
  import { agendaList } from "./model";
  import { t } from "../../lib/i18n.svelte";

  let {
    from,
    count,
    byDay,
    today,
    weather,
    units,
    syncing = false,
    onopen,
    onpick,
    onmore,
    onrefresh,
    onvisible,
  }: {
    from: Date;
    /** Days listed from `from`. */
    count: number;
    byDay: Map<string, CalEvent[]>;
    today: Date;
    weather: Record<string, DayWeather>;
    units: "c" | "f";
    syncing?: boolean;
    onopen: (e: CalEvent) => void;
    /** A day's date was tapped. */
    onpick: (d: Date) => void;
    /** Scrolled near the end: list more days. */
    onmore: () => void;
    onrefresh: () => void;
    /** The day at the top of the list changed. */
    onvisible: (d: Date) => void;
  } = $props();

  const days = $derived(agendaList(from, count, byDay, today));
  let scroller: HTMLDivElement | undefined = $state();

  // A new start date shows from the top.
  $effect(() => {
    void from.getTime();
    scroller?.scrollTo({ top: 0 });
  });

  function onScroll() {
    if (!scroller) return;
    if (scroller.scrollTop + scroller.clientHeight > scroller.scrollHeight - 800) onmore();
    const top = scroller.getBoundingClientRect().top;
    for (const el of scroller.querySelectorAll<HTMLElement>("[data-day]")) {
      if (el.getBoundingClientRect().bottom > top + 4) {
        onvisible(new Date(Number(el.dataset.day)));
        break;
      }
    }
  }

  // Pull down at the top to sync.
  let pullStart: number | null = null;
  let pull = $state(0);
  function touchStart(e: TouchEvent) {
    pullStart = scroller && scroller.scrollTop <= 0 ? e.touches[0].clientY : null;
  }
  function touchMove(e: TouchEvent) {
    if (pullStart === null) return;
    pull = Math.max(0, Math.min(110, (e.touches[0].clientY - pullStart) * 0.5));
  }
  function touchEnd() {
    if (pull >= 70) onrefresh();
    pull = 0;
    pullStart = null;
  }

  const timeOf = (e: CalEvent, d: Date) => {
    if (isBar(e)) return t("m.cal.allDay");
    const starts = sameDay(new Date(e.startMs), d);
    const ends = sameDay(new Date(e.endMs), d) || (e.endMs - e.startMs < 86_400_000 && new Date(e.endMs).getHours() === 0 && new Date(e.endMs).getMinutes() === 0);
    if (starts && ends) return `${fmtTime(e.startMs)} – ${fmtTime(e.endMs)}`;
    if (starts) return t("m.cal.from", { time: fmtTime(e.startMs) });
    return ends ? t("m.cal.until", { time: fmtTime(e.endMs) }) : t("m.cal.allDay");
  };
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="list" bind:this={scroller} onscroll={onScroll} ontouchstart={touchStart} ontouchmove={touchMove} ontouchend={touchEnd}>
  {#if pull > 0 || syncing}
    <div class="pull" style:height="{syncing ? 56 : pull}px"><span class="spinner" class:ready={syncing || pull >= 70}></span></div>
  {/if}
  {#each days as day (day.key)}
    {@const w = weather[day.key]}
    {@const isToday = sameDay(day.d, today)}
    {#if day.monthStart}
      <h2 class="month">{MONTH.format(day.d)}</h2>
    {/if}
    <section class="day" data-day={day.d.getTime()}>
      <button class="date" class:today={isToday} onclick={() => onpick(day.d)} aria-label={day.d.toLocaleDateString(dateLocale(), { weekday: "long", month: "long", day: "numeric" })}>
        <span class="wd">{WEEKDAY.format(day.d)}</span>
        <span class="num">{day.d.getDate()}</span>
        {#if w}<span class="wx"><WeatherIcon code={w.code} size={13} /> {temp(w.max, units)}</span>{/if}
      </button>
      <div class="events">
        {#each day.events as e (e.key)}
          <button class="ev" class:bar={isBar(e)} style:--c={e.color} onclick={() => onopen(e)}>
            <span class="t" dir="auto">{e.summary || t("m.cal.noTitle")}{#if e.recurring}<Repeat size={11} strokeWidth={1.8} />{/if}</span>
            <span class="meta"><bdi class="when">{timeOf(e, day.d)}</bdi>{#if e.location}<span class="loc"><MapPin size={11} strokeWidth={1.8} /> <bdi>{e.location}</bdi></span>{/if}</span>
          </button>
        {:else}
          <div class="none">{isToday ? t("m.cal.nothingToday") : t("m.cal.noEvents")}</div>
        {/each}
      </div>
    </section>
  {/each}
  <div class="tail"></div>
</div>

<style>
  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 3px 11px 0;
  }
  .pull {
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: hidden;
  }
  .pull .spinner:not(.ready) {
    animation-play-state: paused;
    opacity: 0.5;
  }
  .month {
    margin: 18px 7px 8px;
    font-size: 12px;
    font-weight: 600;
    color: var(--m-text-2);
  }
  .day {
    display: flex;
    gap: 8px;
    padding: 5px 0;
  }
  .date {
    flex: none;
    width: 44px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    padding: 5px 0;
    border: 0;
    border-radius: 15px;
    background: transparent;
    color: var(--m-text);
    font: inherit;
    font-variant-numeric: tabular-nums;
  }
  .date:active {
    background: var(--m-pill);
  }
  .wd {
    font-size: 11px;
    color: var(--m-text-2);
  }
  .num {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 31px;
    height: 31px;
    border-radius: 50%;
    font-size: 16px;
    font-weight: 500;
  }
  .today .wd {
    color: var(--m-accent);
    font-weight: 600;
  }
  .today .num {
    background: var(--m-accent);
    color: #fff;
  }
  .wx {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    margin-top: 2px;
    font-size: 10px;
    color: var(--m-text-2);
  }
  .events {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 7px;
    padding-top: 3px;
  }
  .ev {
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 100%;
    padding: 10px 13px;
    border: 0;
    border-radius: 14px;
    padding-inline-start: 19px;
    background: var(--m-surface);
    color: var(--m-text);
    font: inherit;
    text-align: start;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.04);
    position: relative;
  }
  /* The calendar's colour: a rounded bar inside the card's start edge. It
     keeps clear of the corners, so it stays straight at any size (a coloured
     border would curve with them). */
  .ev::before {
    content: "";
    position: absolute;
    top: 9px;
    bottom: 9px;
    inset-inline-start: 7px;
    width: 4px;
    border-radius: 2px;
    background: var(--c);
  }
  .when {
    /* Times read left to right in Arabic too ("7:00 PM – 8:30 PM"). */
    direction: ltr;
    unicode-bidi: isolate;
  }
  .ev.bar {
    background: color-mix(in srgb, var(--c) 16%, var(--m-surface));
  }
  .ev:active {
    filter: brightness(0.96);
  }
  .t {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 14px;
    font-weight: 500;
    line-height: 1.3;
    overflow-wrap: anywhere;
  }
  .t :global(svg) {
    flex: none;
    color: var(--m-text-3);
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    column-gap: 10px;
    font-size: 11px;
    color: var(--m-text-2);
    font-variant-numeric: tabular-nums;
  }
  .loc {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    min-width: 0;
    max-width: 100%;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .none {
    padding: 11px 3px;
    font-size: 12px;
    color: var(--m-text-3);
  }
  .tail {
    height: 90px;
  }
</style>
