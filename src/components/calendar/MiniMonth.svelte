<script lang="ts">
  import { addDays, dayKey, sameDay, startOfWeek, weekNumber, weekdayNames, type CalEvent } from "../../lib/calendar.svelte";

  let {
    month,
    selected,
    weekStart,
    weekNumbers = true,
    byDay,
    today,
    highlightWeek = true,
    compact = false,
    onpick,
  }: {
    month: Date;
    selected: Date | null;
    weekStart: number;
    weekNumbers?: boolean;
    /** The loaded events per dayKey. */
    byDay: Map<string, CalEvent[]>;
    /** Start of the current day (moves at midnight). */
    today: Date;
    highlightWeek?: boolean;
    compact?: boolean;
    onpick: (d: Date) => void;
  } = $props();

  const rows = $derived.by(() => {
    const first = new Date(month.getFullYear(), month.getMonth(), 1);
    const start = startOfWeek(first, weekStart);
    return Array.from({ length: 6 }, (_, w) => Array.from({ length: 7 }, (_, d) => addDays(start, w * 7 + d)));
  });
  const names = $derived(weekdayNames(weekStart, "short").map((n) => n.slice(0, 3)));

  /** Up to three calendar colours per day. */
  const dots = $derived.by(() => {
    const map: Record<string, string[]> = {};
    for (const row of rows) {
      for (const d of row) {
        const colors: string[] = [];
        for (const e of byDay.get(dayKey(d)) ?? []) {
          if (!colors.includes(e.color)) colors.push(e.color);
          if (colors.length === 3) break;
        }
        map[dayKey(d)] = colors;
      }
    }
    return map;
  });
</script>

<div class="mini" class:compact class:nocw={!weekNumbers}>
  <div class="row head">
    {#if weekNumbers}<span class="cw">CW</span>{/if}
    {#each names as n, i (i)}<span class="dn">{n}</span>{/each}
  </div>
  {#each rows as row, r (r)}
    <div class="row" class:sel-week={highlightWeek && selected && row.some((d) => sameDay(d, selected!))}>
      {#if weekNumbers}<span class="cw">{weekNumber(row[0])}</span>{/if}
      {#each row as d (d.getTime())}
        <button
          class="day"
          class:other={d.getMonth() !== month.getMonth()}
          class:today={sameDay(d, today)}
          class:selected={selected && sameDay(d, selected)}
          onclick={() => onpick(d)}
        >
          <span class="num">{d.getDate()}</span>
          <span class="dots">{#each dots[dayKey(d)] ?? [] as c (c)}<i style:background={c}></i>{/each}</span>
        </button>
      {/each}
    </div>
  {/each}
</div>

<style>
  .mini {
    display: flex;
    flex-direction: column;
    gap: 1px;
    font-variant-numeric: tabular-nums;
  }
  .row {
    display: grid;
    grid-template-columns: 26px repeat(7, 1fr);
    align-items: center;
    border-radius: 7px;
  }
  .nocw .row {
    grid-template-columns: repeat(7, 1fr);
  }
  .row.sel-week {
    background: var(--cal-week-bg, rgba(255, 255, 255, 0.09));
  }
  .head {
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.3px;
    color: var(--cal-muted, #8e8e93);
    padding-bottom: 2px;
  }
  .cw {
    font-size: 10px;
    color: var(--cal-muted, #8e8e93);
    text-align: center;
  }
  .dn {
    text-align: center;
  }
  .day {
    border: none;
    background: none;
    color: inherit;
    padding: 2px 0 1px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 1px;
    font-size: 12.5px;
    font-weight: 600;
    border-radius: 6px;
  }
  .compact .day {
    font-size: 11px;
    font-weight: 500;
    padding: 1px 0 0;
  }
  .day:hover .num {
    background: var(--cal-hover, rgba(255, 255, 255, 0.12));
  }
  .num {
    width: 24px;
    height: 24px;
    line-height: 24px;
    border-radius: 50%;
    text-align: center;
  }
  .compact .num {
    width: 20px;
    height: 20px;
    line-height: 20px;
  }
  .other {
    color: var(--cal-muted, #8e8e93);
    font-weight: 500;
  }
  .selected .num {
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
  .today .num {
    background: var(--accent) !important;
    color: #fff;
  }
  .dots {
    display: flex;
    gap: 2px;
    height: 4px;
  }
  .dots i {
    width: 4px;
    height: 4px;
    border-radius: 50%;
  }
  .compact .dots {
    height: 3px;
  }
  .compact .dots i {
    width: 3px;
    height: 3px;
  }
</style>
