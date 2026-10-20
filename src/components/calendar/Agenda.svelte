<script module lang="ts">
  const shortDate = new Intl.DateTimeFormat(undefined, { month: "numeric", day: "numeric", year: "2-digit" });
  const weekday = new Intl.DateTimeFormat(undefined, { weekday: "long" });
</script>

<script lang="ts">
  import { addDays, dayKey, fmtTime, isBar, sameDay, startOfDay, temp, type CalEvent, type DayWeather } from "../../lib/calendar.svelte";
  import WeatherIcon from "./WeatherIcon.svelte";

  let {
    from,
    byDay,
    today,
    weather,
    units,
    onopen,
    onpick,
  }: {
    from: Date;
    /** The loaded events per dayKey. */
    byDay: Map<string, CalEvent[]>;
    /** Start of the current day (moves at midnight). */
    today: Date;
    weather: Record<string, DayWeather>;
    units: "c" | "f";
    onopen: (e: CalEvent, rect: DOMRect) => void;
    onpick: (d: Date) => void;
  } = $props();

  const days = $derived.by(() =>
    Array.from({ length: 21 }, (_, i) => addDays(startOfDay(from), i))
      .map((d) => {
        const list = byDay.get(dayKey(d)) ?? [];
        return {
          d,
          bars: list.filter((e) => isBar(e)),
          timed: list.filter((e) => !isBar(e)).sort((a, b) => a.startMs - b.startMs),
          w: weather[dayKey(d)],
        };
      })
      .filter((x, i) => i === 0 || x.bars.length || x.timed.length || x.w),
  );

  function title(d: Date): [string, string] {
    const date = shortDate.format(d);
    if (sameDay(d, today)) return ["TODAY", date];
    if (sameDay(d, addDays(today, 1))) return ["TOMORROW", date];
    return [weekday.format(d).toUpperCase(), date];
  }
</script>

<div class="agenda">
  {#each days as day (day.d.getTime())}
    {@const [name, date] = title(day.d)}
    <div class="day">
      <button class="dhead" class:today={sameDay(day.d, today)} onclick={() => onpick(day.d)}>
        <span class="dname">{name} <span class="ddate">{date}</span></span>
        {#if day.w}<span class="wx">{temp(day.w.max, units)}/{temp(day.w.min, units)} <WeatherIcon code={day.w.code} size={15} /></span>{/if}
      </button>
      {#each day.bars as e (e.key)}
        <button class="pill" dir="auto" style:--c={e.color} onclick={(ev) => onopen(e, ev.currentTarget.getBoundingClientRect())}>{e.summary || "(No title)"}</button>
      {/each}
      {#each day.timed as e (e.key)}
        <button class="ev" onclick={(ev) => onopen(e, ev.currentTarget.getBoundingClientRect())}>
          <span class="time"><i style:background={e.color}></i>{fmtTime(e.startMs)} – {fmtTime(e.endMs)}</span>
          <span class="t" dir="auto">{e.summary || "(No title)"}</span>
        </button>
      {/each}
      {#if day.bars.length === 0 && day.timed.length === 0}
        <div class="none">No events</div>
      {/if}
    </div>
  {/each}
</div>

<style>
  .agenda {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 4px 14px 16px;
  }
  .day {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .dhead {
    display: flex;
    align-items: center;
    justify-content: space-between;
    border: none;
    background: none;
    padding: 0;
    color: #f2f2f7;
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 0.2px;
  }
  .dhead.today .dname {
    color: var(--accent);
  }
  .ddate {
    font-weight: 700;
  }
  .wx {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-weight: 700;
  }
  .pill {
    align-self: flex-start;
    border: none;
    border-radius: 5px;
    padding: 1px 8px;
    font-size: 12px;
    font-weight: 600;
    background: color-mix(in srgb, var(--c) 75%, #000);
    color: #fff;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ev {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    border: none;
    background: none;
    padding: 2px 0 2px 2px;
    color: #f2f2f7;
    text-align: left;
    border-radius: 6px;
  }
  .ev:hover {
    background: rgba(255, 255, 255, 0.06);
  }
  .time {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 11.5px;
    color: #aeaeb2;
  }
  .time i {
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }
  .t {
    padding-left: 15px;
    font-size: 13px;
    font-weight: 600;
  }
  .none {
    font-size: 12px;
    color: #8e8e93;
  }
</style>
