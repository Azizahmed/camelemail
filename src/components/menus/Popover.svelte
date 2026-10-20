<script lang="ts">
  import type { Snippet } from "svelte";
  import { placeMenu } from "../../lib/ui.svelte";

  let {
    anchor,
    width = 240,
    height = 300,
    align = "right",
    onclose,
    children,
  }: {
    anchor: DOMRect;
    width?: number;
    height?: number;
    align?: "left" | "right";
    onclose: () => void;
    children: Snippet;
  } = $props();

  let el: HTMLDivElement | undefined = $state();
  const pos = $derived(placeMenu(anchor, width, height, align));

  $effect(() => {
    const down = (e: MouseEvent) => {
      if (el && !el.contains(e.target as Node)) onclose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        e.preventDefault();
        onclose();
      }
    };
    // Defer so the click that opened us doesn't close us.
    const t = setTimeout(() => window.addEventListener("mousedown", down), 0);
    window.addEventListener("keydown", key, true);
    return () => {
      clearTimeout(t);
      window.removeEventListener("mousedown", down);
      window.removeEventListener("keydown", key, true);
    };
  });
</script>

<div bind:this={el} class="menu" style:left="{pos.left}px" style:top="{pos.top}px" style:width="{width}px">
  {@render children()}
</div>
