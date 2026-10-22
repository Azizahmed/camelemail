<script lang="ts">
  import { dateLocale } from "../../lib/format";
  // The week above the phone's day view: seven day chips (weekday letter and
  // date), today marked, the shown days filled. Swipe for the next/previous week.
  import { addDays, dayKey, sameDay, startOfWeek, type CalEvent } from "../../lib/calendar.svelte";
  import { swipe } from "./swipe";

  let {
    selected,
    span = 1,
    weekStart,
    today,
    byDay,
    onpick,
    onweek,
  }: {
    selected: Date;
    /** How many days from `selected` the view shows (filled). */
    span?: number;
    weekStart: number;
    today: Date;
    byDay: Map<string, CalEvent[]>;
    onpick: (d: Date) => void;
    onweek: (dir: -1 | 1) => void;
  } = $props();

  const days = $derived(Array.from({ length: 7 }, (_, i) => addDays(startOfWeek(selected, weekStart), i)));
  // In the app's language (the shared weekdayNames follows the system's).
  const letters = $derived(Array.from({ length: 7 }, (_, i) => addDays(new Date(2023, 0, 1), (weekStart + i) % 7).toLocaleDateString(dateLocale(), { weekday: "narrow" })));
  const shown = (d: Date) => {
    const t = d.getTime();
    return t >= selected.getTime() && t < addDays(selected, span).getTime();
  };
  let drag = $state(0);
</script>

<div class="strip" use:swipe={{ onstep: onweek, ondrag: (dx) => (drag = dx) }}>
  <div class="row" class:dragging={drag !== 0} style:transform={drag ? `translateX(${drag * 0.4}px)` : null}>
    {#each days as d, i (d.getTime())}
      <button class="chip" class:today={sameDay(d, today)} class:on={shown(d)} class:sel={sameDay(d, selected)} aria-label={d.toLocaleDateString(dateLocale(), { weekday: "long", month: "long", day: "numeric" })} aria-pressed={shown(d)} onclick={() => onpick(d)}>
        <span class="wd">{letters[i]}</span>
        <span class="num">{d.getDate()}</span>
        <span class="dot" class:has={(byDay.get(dayKey(d))?.length ?? 0) > 0}></span>
      </button>
    {/each}
  </div>
</div>

<style>
  .strip {
    flex: none;
    padding: 2px 8px 8px;
    touch-action: pan-y;
    overflow: hidden;
  }
  .row {
    display: grid;
    grid-template-columns: repeat(7, 1fr);
    gap: 3px;
    transition: transform 0.18s ease-out;
  }
  .row.dragging {
    transition: none;
  }
  .chip {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    padding: 7px 0 5px;
    border: 0;
    border-radius: 15px;
    background: transparent;
    color: var(--m-text);
    font: inherit;
    font-variant-numeric: tabular-nums;
  }
  .wd {
    font-size: 11px;
    color: var(--m-text-2);
  }
  .num {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    font-size: 14px;
    font-weight: 500;
  }
  .today .wd,
  .today .num {
    color: var(--m-accent);
    font-weight: 650;
  }
  .on {
    background: var(--m-accent-soft);
  }
  .on:not(.sel) .num {
    color: var(--m-accent);
  }
  .sel .num {
    background: var(--m-accent);
    color: #fff;
  }
  .dot {
    width: 4px;
    height: 4px;
    border-radius: 50%;
  }
  .dot.has {
    background: var(--m-text-3);
  }
  .sel .dot.has {
    background: var(--m-accent);
  }
</style>
