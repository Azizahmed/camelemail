<script lang="ts">
  import { CalendarDays, Coffee, Car, Clock3, Sun, Sunset, Sunrise, Beer, Briefcase } from "@lucide/svelte";
  import { onMount } from "svelte";
  import { api } from "../../lib/api";
  import type { TimePreset } from "../../lib/types";
  import Popover from "./Popover.svelte";

  let { anchor, onpick, onclose }: { anchor: DOMRect; onpick: (until: number) => void; onclose: () => void } = $props();

  let presets: TimePreset[] = $state([]);
  let picking = $state(false);
  let date = $state("");

  onMount(async () => {
    presets = await api.snoozePresets();
  });

  const icons: Record<string, { icon: typeof Clock3; color: string }> = {
    later_today: { icon: Clock3, color: "var(--identity-blue)" },
    afternoon: { icon: Sun, color: "var(--identity-gold)" },
    evening: { icon: Sunset, color: "var(--identity-gold)" },
    morning: { icon: Sunrise, color: "var(--identity-gold)" },
    tomorrow: { icon: Coffee, color: "var(--identity-magenta)" },
    day_after: { icon: Car, color: "var(--identity-violet)" },
    this_weekend: { icon: Beer, color: "var(--identity-teal)" },
    next_week: { icon: Briefcase, color: "var(--identity-pink)" },
  };
  const short: Record<string, string> = {
    later_today: "Later",
    afternoon: "Afternoon",
    evening: "Evening",
    morning: "Morning",
    tomorrow: "Tomorrow",
    day_after: "",
    this_weekend: "Saturday",
    next_week: "Monday",
  };

  function sub(p: TimePreset): string {
    const d = new Date(p.at);
    const t = d.toLocaleTimeString(undefined, { hour: "numeric", minute: d.getMinutes() ? "2-digit" : undefined }).toLowerCase();
    if (p.id === "later_today") {
      const h = Math.round((p.at - Date.now()) / 3_600_000);
      return h <= 1 ? "1 hour" : `${h} hours`;
    }
    if (p.id === "day_after") return `${d.toLocaleDateString(undefined, { weekday: "short" })} ${t}`;
    return t;
  }

  function title(p: TimePreset): string {
    if (p.id === "day_after") return new Date(p.at).toLocaleDateString(undefined, { weekday: "long" });
    return short[p.id] || p.label;
  }

  const today = new Date();
  const minDate = (() => {
    const d = new Date();
    if (d.getHours() >= 22) d.setDate(d.getDate() + 1);
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
  })();

  function pickDate() {
    if (!date) return;
    const [y, m, d] = date.split("-").map(Number);
    const at = new Date(y, m - 1, d, 22, 0, 0).getTime();
    if (at > Date.now()) onpick(at);
  }
  void today;
</script>

<Popover {anchor} width={330} height={picking ? 330 : 270} {onclose}>
  <div class="grid">
    {#each presets as p (p.id)}
      {@const ic = icons[p.id] ?? icons.later_today}
      <button class="cell" onclick={() => onpick(p.at)} title={new Date(p.at).toLocaleString()}>
        <span class="ic" style:color={ic.color}><ic.icon size={22} strokeWidth={1.6} /></span>
        <span class="t">{title(p)}</span>
        <span class="s">{sub(p)}</span>
      </button>
    {/each}
    <button class="cell" class:on={picking} onclick={() => (picking = !picking)}>
      <span class="ic" style:color="var(--identity-green)"><CalendarDays size={22} strokeWidth={1.6} /></span>
      <span class="t">Pick date</span>
      <span class="s">10 pm</span>
    </button>
  </div>
  {#if picking}
    <div class="pick">
      <input class="input" type="date" min={minDate} bind:value={date} />
      <button class="btn primary" disabled={!date} onclick={pickDate}>Snooze</button>
    </div>
  {/if}
</Popover>

<style>
  .grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 4px;
  }
  .cell {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    padding: 12px 4px 10px;
    border: none;
    background: none;
    border-radius: 6px;
  }
  .cell:hover,
  .cell.on {
    background: var(--surface-elevated-hover);
  }
  .ic {
    width: 36px;
    height: 36px;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .t {
    font-size: 12px;
    font-weight: 500;
  }
  .s {
    font-size: 11px;
    color: var(--content-subtle);
  }
  .pick {
    display: flex;
    gap: 8px;
    padding: 8px 4px 2px;
  }
</style>
