<script module lang="ts">
  const weekday = new Intl.DateTimeFormat(undefined, { weekday: "short" });
</script>

<script lang="ts">
  import { onMount } from "svelte";
  import { addDays, dayKey, daysBetween, fmtTime, isBar, minutesOfDay, sameDay, startOfDay, temp, type CalEvent, type DayWeather } from "../../lib/calendar.svelte";
  import WeatherIcon from "./WeatherIcon.svelte";

  let {
    days,
    byDay,
    weather,
    units,
    onopen,
    oncreate,
    onpick,
  }: {
    days: Date[];
    /** The loaded events per dayKey. */
    byDay: Map<string, CalEvent[]>;
    weather: Record<string, DayWeather>;
    units: "c" | "f";
    onopen: (e: CalEvent, rect: DOMRect) => void;
    oncreate: (start: Date, allDay: boolean) => void;
    onpick: (d: Date) => void;
  } = $props();

  const HOUR = 44;
  let scroller: HTMLDivElement | undefined = $state();
  let now = $state(Date.now());

  onMount(() => {
    scroller?.scrollTo({ top: HOUR * 7.5 });
    const t = setInterval(() => (now = Date.now()), 60_000);
    return () => clearInterval(t);
  });

  /** All-day/multi-day bars across the visible days, with lanes. */
  const bars = $derived.by(() => {
    const first = startOfDay(days[0]).getTime();
    const last = addDays(startOfDay(days[days.length - 1]), 1).getTime();
    const out: { e: CalEvent; start: number; span: number; lane: number }[] = [];
    const lanes: number[][] = [];
    const barEvents = new Set(days.flatMap((d) => byDay.get(dayKey(d)) ?? []).filter((e) => isBar(e) && e.startMs < last && e.endMs > first));
    for (const e of [...barEvents].sort((a, b) => a.startMs - b.startMs)) {
      const start = Math.max(0, daysBetween(days[0], new Date(Math.max(e.startMs, first))));
      const lastCol = Math.min(days.length - 1, daysBetween(days[0], new Date(Math.max(e.startMs, Math.min(e.endMs, last) - 1))));
      const span = Math.max(1, lastCol - start + 1);
      let lane = lanes.findIndex((used) => used.every((c) => c < start || c >= start + span));
      if (lane < 0) {
        lane = lanes.length;
        lanes.push([]);
      }
      for (let c = start; c < start + span; c++) lanes[lane].push(c);
      out.push({ e, start, span, lane });
    }
    return { bars: out, lanes: lanes.length };
  });

  /** Timed events of a day placed in side-by-side columns where they overlap. */
  function placed(d: Date) {
    const dayStart = startOfDay(d).getTime();
    const dayEnd = startOfDay(addDays(d, 1)).getTime();
    const list = (byDay.get(dayKey(d)) ?? [])
      .filter((e) => !isBar(e))
      .map((e) => ({ e, s: Math.max(e.startMs, dayStart), en: Math.min(Math.max(e.endMs, e.startMs + 20 * 60_000), dayEnd) }))
      .sort((a, b) => a.s - b.s || b.en - a.en);
    const out: { e: CalEvent; top: number; height: number; col: number; cols: number }[] = [];
    let cluster: typeof out = [];
    let clusterEnd = 0;
    const colEnds: number[] = [];
    const flush = () => {
      const n = Math.max(1, ...cluster.map((c) => c.col + 1));
      for (const c of cluster) c.cols = n;
      out.push(...cluster);
      cluster = [];
      colEnds.length = 0;
    };
    for (const it of list) {
      if (cluster.length && it.s >= clusterEnd) flush();
      let col = colEnds.findIndex((end) => end <= it.s);
      if (col < 0) col = colEnds.length;
      colEnds[col] = it.en;
      clusterEnd = Math.max(clusterEnd, it.en);
      // Wall-clock position, matching the hour labels on DST days.
      const top = it.s <= dayStart ? 0 : (minutesOfDay(it.s) / 60) * HOUR;
      const bottom = it.en >= dayEnd ? 24 * HOUR : (minutesOfDay(it.en) / 60) * HOUR;
      cluster.push({ e: it.e, top, height: Math.max(18, bottom - top - 2), col, cols: 1 });
    }
    flush();
    return out;
  }

  function slotClick(ev: MouseEvent, d: Date) {
    const rect = (ev.currentTarget as HTMLElement).getBoundingClientRect();
    const minutes = Math.floor(((ev.clientY - rect.top) / HOUR) * 2) * 30;
    const s = new Date(d.getFullYear(), d.getMonth(), d.getDate(), 0, minutes);
    oncreate(s, false);
  }

  const hours = Array.from({ length: 24 }, (_, h) => h);
  const hourLabel = (h: number) => new Date(2000, 0, 1, h).toLocaleTimeString(undefined, { hour: "numeric" });
</script>

<div class="tv" style:--n={days.length}>
  <div class="dayhead">
    <span class="gutter"></span>
    {#each days as d (d.getTime())}
      {@const w = weather[dayKey(d)]}
      <button class="dh" class:today={sameDay(d, new Date(now))} onclick={() => onpick(d)}>
        <span class="wd">{weekday.format(d)}</span>
        <span class="dn">{d.getDate()}</span>
        {#if w}<span class="wx"><WeatherIcon code={w.code} size={13} /> {temp(w.max, units)}/{temp(w.min, units)}</span>{/if}
      </button>
    {/each}
  </div>
  <div class="allday" style:height="{Math.max(1, bars.lanes) * 20 + 6}px">
    <span class="gutter lbl">all-day</span>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="ad-days" ondblclick={(ev) => {
      const r = (ev.currentTarget as HTMLElement).getBoundingClientRect();
      const i = Math.floor(((ev.clientX - r.left) / r.width) * days.length);
      oncreate(startOfDay(days[Math.max(0, Math.min(days.length - 1, i))]), true);
    }}>
      {#each bars.bars as b (b.e.key)}
        <button
          class="bar"
          style:left="calc({(b.start / days.length) * 100}% + 2px)"
          style:width="calc({(b.span / days.length) * 100}% - 4px)"
          style:top="{3 + b.lane * 20}px"
          style:--c={b.e.color}
          dir="auto"
          onclick={(ev) => onopen(b.e, ev.currentTarget.getBoundingClientRect())}
        >{b.e.summary || "(No title)"}</button>
      {/each}
    </div>
  </div>
  <div class="scroll" bind:this={scroller}>
    <div class="hours" style:height="{HOUR * 24}px">
      <div class="gutter">
        {#each hours as h (h)}<span class="hl" style:top="{h * HOUR}px">{h === 0 ? "" : hourLabel(h)}</span>{/each}
      </div>
      {#each days as d (d.getTime())}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="col" class:today={sameDay(d, new Date(now))} ondblclick={(ev) => slotClick(ev, d)}>
          {#each hours as h (h)}<div class="line" style:top="{h * HOUR}px"></div>{/each}
          {#each placed(d) as p (p.e.key)}
            <button
              class="block"
              class:short={p.height < 34}
              style:top="{p.top}px"
              style:height="{p.height}px"
              style:left="calc({(p.col / p.cols) * 100}% + 2px)"
              style:width="calc({100 / p.cols}% - 4px)"
              style:--c={p.e.color}
              onclick={(ev) => { ev.stopPropagation(); onopen(p.e, ev.currentTarget.getBoundingClientRect()); }}
              ondblclick={(ev) => ev.stopPropagation()}
            >
              <span class="bt" dir="auto">{p.e.summary || "(No title)"}</span>
              <span class="bm">{fmtTime(p.e.startMs)}{#if p.e.location} · <bdi>{p.e.location}</bdi>{/if}</span>
            </button>
          {/each}
          {#if sameDay(d, new Date(now))}
            <div class="now" style:top="{(minutesOfDay(now) / 60) * HOUR}px"></div>
          {/if}
        </div>
      {/each}
    </div>
  </div>
</div>

<style>
  .tv {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    font-variant-numeric: tabular-nums;
  }
  .gutter {
    width: 58px;
    flex: none;
    position: relative;
  }
  .dayhead {
    display: flex;
    border-bottom: 0.5px solid var(--separator);
  }
  .dh {
    flex: 1;
    min-width: 0;
    border: none;
    background: none;
    display: flex;
    align-items: baseline;
    gap: 6px;
    padding: 8px 8px 6px;
    color: var(--content-subtle);
    font-size: 12px;
  }
  .wd {
    text-transform: uppercase;
    font-weight: 600;
    letter-spacing: 0.4px;
  }
  .dn {
    font-size: 20px;
    font-weight: 600;
    color: var(--content);
  }
  .dh.today .dn {
    color: var(--accent);
  }
  .wx {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 11px;
    font-weight: 600;
    color: var(--content);
  }
  .allday {
    display: flex;
    flex: none;
    border-bottom: 0.5px solid var(--separator);
  }
  .lbl {
    font-size: 10.5px;
    color: var(--content-muted);
    text-align: right;
    padding: 5px 8px 0 0;
  }
  .ad-days {
    flex: 1;
    position: relative;
    background: repeating-linear-gradient(90deg, transparent 0 calc(100% / var(--n) - 0.5px), var(--separator-subtle) calc(100% / var(--n) - 0.5px) calc(100% / var(--n)));
  }
  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .hours {
    display: flex;
    position: relative;
  }
  .hl {
    position: absolute;
    right: 8px;
    transform: translateY(-50%);
    font-size: 10.5px;
    color: var(--content-muted);
    white-space: nowrap;
  }
  .col {
    flex: 1;
    min-width: 0;
    position: relative;
    border-left: 0.5px solid var(--separator-subtle);
  }
  .col.today {
    background: color-mix(in srgb, var(--accent) 4%, transparent);
  }
  .line {
    position: absolute;
    left: 0;
    right: 0;
    border-top: 0.5px solid var(--separator-subtle);
  }
  .block {
    position: absolute;
    border: none;
    border-radius: 6px;
    padding: 3px 6px;
    text-align: start;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    gap: 1px;
    background: color-mix(in srgb, var(--c) 22%, var(--surface-reading));
    border-left: 3px solid var(--c);
    color: var(--content);
    z-index: 1;
  }
  .block:hover {
    background: color-mix(in srgb, var(--c) 34%, var(--surface-reading));
    z-index: 2;
  }
  .block.short {
    flex-direction: row;
    gap: 6px;
    align-items: center;
    padding-top: 1px;
  }
  .bt {
    font-size: 12px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .bm {
    font-size: 11px;
    color: var(--content-subtle);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .bar {
    position: absolute;
    height: 18px;
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
    border-left: 3px solid var(--c);
    color: var(--content);
  }
  .now {
    position: absolute;
    left: -4px;
    right: 0;
    height: 2px;
    background: #ff3b30;
    z-index: 3;
  }
  .now::before {
    content: "";
    position: absolute;
    left: -4px;
    top: -4px;
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: #ff3b30;
  }
</style>
