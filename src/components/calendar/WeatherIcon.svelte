<script lang="ts">
  import { Cloud, CloudDrizzle, CloudFog, CloudLightning, CloudRain, CloudSnow, CloudSun, Sun } from "@lucide/svelte";
  import { weatherKind } from "../../lib/calendar.svelte";

  let { code, size = 14 }: { code: number; size?: number } = $props();
  const ICONS = { sun: Sun, partly: CloudSun, cloud: Cloud, fog: CloudFog, drizzle: CloudDrizzle, rain: CloudRain, snow: CloudSnow, storm: CloudLightning };
  const kind = $derived(weatherKind(code));
  const Icon = $derived(ICONS[kind]);
</script>

<span class="wx {kind}"><Icon {size} strokeWidth={1.8} /></span>

<style>
  .wx {
    display: inline-flex;
  }
  .sun {
    color: #f5a623;
  }
  .partly {
    color: #e0a43a;
  }
  .rain,
  .drizzle,
  .storm {
    color: #5b9bd5;
  }
  .cloud,
  .fog,
  .snow {
    color: var(--cal-muted, #8e8e93);
  }
</style>
