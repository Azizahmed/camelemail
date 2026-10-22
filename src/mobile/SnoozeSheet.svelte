<script lang="ts">
  import { dateLocale } from "../lib/format";
  // Snooze until a preset time or a picked date and time.
  import { CalendarDays, Clock3 } from "@lucide/svelte";
  import { api } from "../lib/api";
  import { t } from "../lib/i18n.svelte";
  import type { TimePreset } from "../lib/types";
  import Sheet from "./Sheet.svelte";
  import { presetName } from "./views.svelte";

  let { open = $bindable(false), onpick }: { open?: boolean; onpick: (until: number) => void } = $props();

  let sheet: Sheet | undefined = $state();
  let presets = $state<TimePreset[]>([]);
  let custom = $state("");

  $effect(() => {
    if (open) api.snoozePresets().then((p) => (presets = p)).catch(() => {});
  });

  function when(p: TimePreset) {
    return new Date(p.at).toLocaleString(dateLocale(), { weekday: "short", hour: "numeric", minute: "2-digit" });
  }

  async function pick(at: number) {
    await sheet?.close();
    onpick(at);
  }

  function pickCustom() {
    const at = new Date(custom).getTime();
    if (Number.isFinite(at) && at > Date.now()) void pick(at);
  }
</script>

<Sheet bind:this={sheet} bind:open title={t("m.snooze.title")}>
  {#each presets as p (p.id)}
    <button class="m-sheet-item" onclick={() => pick(p.at)}><Clock3 size={18} strokeWidth={1.6} /> {presetName(p)}<small>{when(p)}</small></button>
  {/each}
  <div class="m-sheet-sep"></div>
  <label class="m-sheet-item custom">
    <CalendarDays size={18} strokeWidth={1.6} />
    <input type="datetime-local" bind:value={custom} onchange={pickCustom} aria-label={t("m.snooze.pick")} />
  </label>
</Sheet>

<style>
  .custom input {
    flex: 1;
    min-width: 0;
    border: 0;
    background: transparent;
    color: var(--m-text);
    font: inherit;
    font-size: 14px;
  }
</style>
