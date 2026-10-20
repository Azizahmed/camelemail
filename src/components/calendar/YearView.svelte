<script module lang="ts">
  const monthName = new Intl.DateTimeFormat(undefined, { month: "long" });
</script>

<script lang="ts">
  import type { CalEvent } from "../../lib/calendar.svelte";
  import MiniMonth from "./MiniMonth.svelte";

  let {
    year,
    selected,
    weekStart,
    weekNumbers,
    byDay,
    today,
    onpick,
  }: { year: number; selected: Date; weekStart: number; weekNumbers: boolean; byDay: Map<string, CalEvent[]>; today: Date; onpick: (d: Date) => void } = $props();

  const months = $derived(Array.from({ length: 12 }, (_, m) => new Date(year, m, 1)));
</script>

<div class="year">
  {#each months as m (m.getMonth())}
    <div class="month">
      <div class="mname" class:current={m.getFullYear() === today.getFullYear() && m.getMonth() === today.getMonth()}>
        {monthName.format(m)}
      </div>
      <MiniMonth month={m} {selected} {weekStart} {weekNumbers} {byDay} {today} highlightWeek={false} compact {onpick} />
    </div>
  {/each}
</div>

<style>
  .year {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(230px, 1fr));
    gap: 18px 26px;
    padding: 16px 22px 28px;
    --cal-muted: var(--content-muted);
    --cal-hover: var(--surface-hover);
    color: var(--content);
  }
  .mname {
    font-size: 15px;
    font-weight: 700;
    margin: 0 0 6px 4px;
    color: var(--accent);
  }
  .mname.current {
    text-decoration: underline;
    text-underline-offset: 4px;
  }
</style>
