<script module lang="ts">
  const monthDay = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" });
</script>

<script lang="ts">
  import { textDirection } from "../../lib/bidi";
  import {
    addDays,
    dayKey,
    daysBetween,
    fmtTime,
    isBar,
    sameDay,
    weekNumber,
    weekdayNames,
    temp,
    type CalEvent,
    type DayWeather,
  } from "../../lib/calendar.svelte";
  import WeatherIcon from "./WeatherIcon.svelte";

  let {
    rowStarts,
    focusMonth = null,
    byDay,
    today,
    weather,
    weekStart,
    weekNumbers,
    fixedRowHeight = null,
    selected,
    units,
    onpick,
    onopen,
    oncreate,
    onmore,
  }: {
    rowStarts: Date[];
    /** Month view: dim days outside this month. Quarter view: null (month borders instead). */
    focusMonth?: Date | null;
    /** The loaded events per dayKey. */
    byDay: Map<string, CalEvent[]>;
    /** Start of the current day (moves at midnight). */
    today: Date;
    weather: Record<string, DayWeather>;
    weekStart: number;
    weekNumbers: boolean;
    /** Quarter view rows have a fixed height and scroll; month rows share the height. */
    fixedRowHeight?: number | null;
    selected: Date;
    units: "c" | "f";
    onpick: (d: Date) => void;
    onopen: (e: CalEvent, rect: DOMRect) => void;
    oncreate: (d: Date) => void;
    onmore: (d: Date) => void;
  } = $props();

  const HEADER = 24;
  const LANE = 19;
  const ITEM = 17;
  let bodyHeight = $state(600);
  const rowHeight = $derived(fixedRowHeight ?? Math.max(70, bodyHeight / Math.max(1, rowStarts.length)));
  const names = $derived(weekdayNames(weekStart));

  interface Bar {
    e: CalEvent;
    start: number;
    span: number;
    lane: number;
    continues: boolean;
    continued: boolean;
  }

  /** Bars (all-day / multi-day) per row with lanes, and the timed events of each day. */
  const layout = $derived.by(() =>
    rowStarts.map((rowStart) => {
      const rowEnd = addDays(rowStart, 7).getTime();
      const days = Array.from({ length: 7 }, (_, i) => addDays(rowStart, i));
      const lists = days.map((d) => byDay.get(dayKey(d)) ?? []);
      const barEvents = [...new Set(lists.flat().filter((e) => isBar(e)))]
        .sort((a, b) => a.startMs - b.startMs || b.endMs - b.startMs - (a.endMs - a.startMs));
      const lanes: number[][] = [];
      const bars: Bar[] = [];
      for (const e of barEvents) {
        // Calendar-day columns (not elapsed ms) so 23/25-hour DST days land right.
        const start = Math.max(0, daysBetween(rowStart, new Date(Math.max(e.startMs, rowStart.getTime()))));
        const endCol = Math.min(6, daysBetween(rowStart, new Date(Math.max(e.startMs, Math.min(e.endMs, rowEnd) - 1))));
        const span = Math.max(1, endCol - start + 1);
        let lane = lanes.findIndex((used) => used.every((c) => c < start || c > start + span - 1));
        if (lane < 0) {
          lane = lanes.length;
          lanes.push([]);
        }
        for (let c = start; c < start + span; c++) lanes[lane].push(c);
        bars.push({ e, start, span, lane, continues: e.endMs > rowEnd, continued: e.startMs < rowStart.getTime() });
      }
      const timed = lists.map((list) => list.filter((e) => !isBar(e)).sort((a, b) => a.startMs - b.startMs));
      return { rowStart, days, bars, timed };
    }),
  );

  /** How many lanes / timed rows fit in a cell. */
  const capacity = $derived(Math.max(1, Math.floor((rowHeight - HEADER - 6) / ITEM)));

  function visibleBars(bars: Bar[]): Bar[] {
    return bars.filter((b) => b.lane < Math.max(1, capacity - 1));
  }

  function cellItems(row: (typeof layout)[number], col: number) {
    const barsHere = row.bars.filter((b) => col >= b.start && col < b.start + b.span);
    const lanesUsed = barsHere.length ? Math.min(Math.max(...barsHere.map((b) => b.lane)) + 1, Math.max(1, capacity - 1)) : 0;
    const hiddenBars = barsHere.filter((b) => b.lane >= Math.max(1, capacity - 1)).length;
    const room = capacity - lanesUsed;
    const timed = row.timed[col];
    const total = timed.length + hiddenBars;
    if (total <= room) return { lanesUsed, shown: timed, more: 0 };
    const shown = timed.slice(0, Math.max(0, room - 1));
    return { lanesUsed, shown, more: total - shown.length };
  }

  const monthLabel = (d: Date) => monthDay.format(d);
</script>

<div class="grid" class:fixed={!!fixedRowHeight} class:nocw={!weekNumbers}>
  <div class="head">
    {#if weekNumbers}<span></span>{/if}
    {#each names as n, i (i)}<span>{n}</span>{/each}
  </div>
  <div class="body" bind:clientHeight={bodyHeight}>
    {#each layout as row (row.rowStart.getTime())}
      <div class="row" style:height="{rowHeight}px">
        {#if weekNumbers}<div class="cw">CW {weekNumber(row.rowStart)}</div>{/if}
        <div class="days">
          {#each row.days as d, col (d.getTime())}
            {@const items = cellItems(row, col)}
            {@const w = weather[dayKey(d)]}
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div
              class="cell"
              class:other={focusMonth && d.getMonth() !== focusMonth.getMonth()}
              class:weekend={d.getDay() === 0 || d.getDay() === 6}
              class:sel={sameDay(d, selected)}
              class:mtop={!focusMonth && d.getDate() <= 7 && d.getDate() > 0}
              class:mleft={!focusMonth && d.getDate() === 1 && col > 0}
              onclick={() => onpick(d)}
              ondblclick={() => oncreate(d)}
            >
              <div class="chead">
                {#if w}
                  <span class="wx"><WeatherIcon code={w.code} size={13} /><span>{temp(w.max, units)}/{temp(w.min, units)}</span></span>
                {/if}
                <span class="grow"></span>
                {#if d.getDate() === 1 && !(focusMonth && d.getMonth() === focusMonth.getMonth() && !sameDay(d, today))}
                  <span class="pill" class:today={sameDay(d, today)}>{monthLabel(d)}</span>
                {:else}
                  <span class="num" class:today={sameDay(d, today)}>{d.getDate()}</span>
                {/if}
              </div>
              <div class="items" style:padding-top="{items.lanesUsed * LANE}px">
                {#each items.shown as e (e.key)}
                  <button class="ev" class:cancel={e.status === "CANCELLED"} onclick={(ev) => { ev.stopPropagation(); onopen(e, ev.currentTarget.getBoundingClientRect()); }}>
                    <i style:background={e.color}></i>
                    <span class="t" dir="auto">{e.summary || "(No title)"}</span>
                    <span class="tm">{fmtTime(e.startMs, true)}</span>
                  </button>
                {/each}
                {#if items.more > 0}
                  <button class="more" onclick={(ev) => { ev.stopPropagation(); onmore(d); }}>{items.more} more…</button>
                {/if}
              </div>
            </div>
          {/each}
          {#each visibleBars(row.bars) as b (b.e.key + b.start)}
            <button
              class="bar"
              class:cont-r={b.continues}
              class:cont-l={b.continued}
              style:left="calc({(b.start / 7) * 100}% + 3px)"
              style:width="calc({(b.span / 7) * 100}% - 6px)"
              style:top="{HEADER + b.lane * LANE}px"
              style:--c={b.e.color}
              dir={textDirection(b.e.summary) ?? "auto"}
              title={b.e.summary}
              onclick={(ev) => onopen(b.e, ev.currentTarget.getBoundingClientRect())}
            >
              {#if !b.e.allDay}<span class="bt">{fmtTime(b.e.startMs, true)}</span>{/if}{b.e.summary || "(No title)"}
            </button>
          {/each}
        </div>
      </div>
    {/each}
  </div>
</div>

<style>
  .grid {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    font-variant-numeric: tabular-nums;
  }
  .head {
    display: grid;
    grid-template-columns: 52px repeat(7, 1fr);
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.4px;
    color: var(--content-subtle);
    text-align: right;
    border-bottom: 0.5px solid var(--separator);
  }
  .nocw .head {
    grid-template-columns: repeat(7, 1fr);
  }
  .head span {
    padding: 6px 8px;
  }
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .fixed .body {
    overflow-y: auto;
  }
  .row {
    flex: none;
    display: flex;
    border-bottom: 0.5px solid var(--separator-subtle);
  }
  .cw {
    width: 52px;
    flex: none;
    font-size: 10.5px;
    color: var(--accent);
    padding: 6px 0 0 8px;
    opacity: 0.85;
  }
  .days {
    flex: 1;
    min-width: 0;
    position: relative;
    display: grid;
    grid-template-columns: repeat(7, 1fr);
  }
  .cell {
    min-width: 0;
    border-left: 0.5px solid var(--separator-subtle);
    padding: 3px 4px 0;
    overflow: hidden;
    position: relative;
  }
  .cell.weekend {
    background: color-mix(in srgb, var(--surface-hover) 45%, transparent);
  }
  .cell.other {
    background: color-mix(in srgb, var(--surface-hover) 70%, transparent);
  }
  .cell.other .num {
    color: var(--content-muted);
  }
  .cell.sel {
    box-shadow: inset 0 0 0 1.5px color-mix(in srgb, var(--accent) 55%, transparent);
  }
  .cell.mtop {
    border-top: 2px solid var(--content-subtle);
  }
  .cell.mleft {
    border-left: 2px solid var(--content-subtle);
  }
  .chead {
    display: flex;
    align-items: center;
    gap: 4px;
    height: 20px;
  }
  .grow {
    flex: 1;
  }
  .wx {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 11px;
    font-weight: 600;
    color: var(--content);
    white-space: nowrap;
  }
  .num {
    font-size: 12.5px;
    font-weight: 500;
    color: var(--content-subtle);
    min-width: 22px;
    height: 20px;
    line-height: 20px;
    text-align: center;
    border-radius: 10px;
  }
  .num.today {
    background: var(--accent);
    color: #fff;
    font-weight: 700;
  }
  .pill {
    font-size: 11px;
    font-weight: 700;
    padding: 0 7px;
    height: 18px;
    line-height: 18px;
    border-radius: 9px;
    background: var(--content);
    color: var(--surface);
  }
  .pill.today {
    background: var(--accent);
    color: #fff;
  }
  .items {
    display: flex;
    flex-direction: column;
  }
  .ev,
  .more {
    display: flex;
    align-items: center;
    gap: 5px;
    height: 17px;
    border: none;
    background: none;
    padding: 0 2px;
    font-size: 11.5px;
    color: var(--content);
    text-align: left;
    border-radius: 4px;
    min-width: 0;
  }
  .ev:hover {
    background: var(--surface-hover);
  }
  .ev i {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex: none;
  }
  .t {
    flex: 1;
    min-width: 0;
    text-align: start;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .tm {
    flex: none;
    font-size: 10.5px;
    color: var(--content-subtle);
  }
  .cancel .t {
    text-decoration: line-through;
    color: var(--content-subtle);
  }
  .more {
    color: var(--content-subtle);
  }
  .more:hover {
    color: var(--content);
  }
  .bar {
    position: absolute;
    height: 17px;
    border: none;
    border-radius: 4px;
    padding: 0 6px;
    font-size: 11.5px;
    font-weight: 500;
    text-align: start;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    background: color-mix(in srgb, var(--c) 28%, var(--surface-reading));
    color: var(--content);
    border-left: 3px solid var(--c);
  }
  .bar:hover {
    background: color-mix(in srgb, var(--c) 40%, var(--surface-reading));
  }
  .bar.cont-r {
    border-top-right-radius: 0;
    border-bottom-right-radius: 0;
  }
  .bar.cont-l {
    border-top-left-radius: 0;
    border-bottom-left-radius: 0;
    border-left-width: 0;
  }
  .bt {
    font-weight: 400;
    color: var(--content-subtle);
    margin-inline-end: 5px;
  }
</style>
