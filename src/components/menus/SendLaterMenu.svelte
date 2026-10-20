<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../../lib/api";
  import { presetDetail } from "../../lib/format";
  import type { TimePreset } from "../../lib/types";
  import Popover from "./Popover.svelte";

  let { anchor, onpick, onclose }: { anchor: DOMRect; onpick: (at: number) => void; onclose: () => void } = $props();

  let presets: TimePreset[] = $state([]);
  const tomorrow = new Date(Date.now() + 86_400_000);
  const pad = (n: number) => String(n).padStart(2, "0");
  let date = $state(`${tomorrow.getFullYear()}-${pad(tomorrow.getMonth() + 1)}-${pad(tomorrow.getDate())}`);
  let time = $state("09:00");
  let error = $state("");

  onMount(async () => {
    presets = await api.sendLaterPresets();
  });

  function custom() {
    const [y, m, d] = date.split("-").map(Number);
    const [hh, mm] = time.split(":").map(Number);
    const at = new Date(y, m - 1, d, hh, mm).getTime();
    if (!(at > Date.now() + 30_000)) {
      error = "Choose a time that hasn't passed yet";
      return;
    }
    onpick(at);
  }
</script>

<Popover {anchor} width={300} height={260} {onclose}>
  <div class="menu-title">Schedule send</div>
  {#each presets as p (p.id)}
    <button class="menu-item" onclick={() => onpick(p.at)}>
      <span>{p.label}</span>
      <span class="detail">{presetDetail(p.at)}</span>
    </button>
  {/each}
  <div class="menu-sep"></div>
  <div class="custom">
    <input class="input" type="date" bind:value={date} />
    <input class="input" type="time" bind:value={time} />
  </div>
  {#if error}<div class="err">{error}</div>{/if}
  <div class="actions">
    <button class="btn primary" onclick={custom}>Schedule</button>
  </div>
</Popover>

<style>
  .custom {
    display: grid;
    grid-template-columns: 1.4fr 1fr;
    gap: 6px;
    padding: 6px 4px;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    padding: 4px;
  }
  .err {
    color: var(--indicator-error);
    font-size: 12px;
    padding: 0 6px;
  }
</style>
