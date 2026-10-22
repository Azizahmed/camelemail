<script lang="ts">
  // A bottom sheet that the back button closes too. Open it by setting `open`.
  import type { Snippet } from "svelte";
  import { back, onLeave, push, type Screen } from "./nav.svelte";

  let {
    open = $bindable(false),
    title = "",
    menu = false,
    children,
  }: { open?: boolean; title?: string; /** A card above the bottom bar instead of a full-width sheet. */ menu?: boolean; children: Snippet } = $props();

  let entry: Screen | null = null;

  /** Close, and wait until the history has caught up (before opening anything else). */
  export async function close() {
    if (entry) await back();
    open = false;
  }

  $effect(() => {
    if (open && !entry) {
      const s: Screen = { name: "sheet", id: Math.random().toString(36) };
      entry = s;
      push(s);
      onLeave(s, () => {
        entry = null;
        open = false;
      });
    } else if (!open && entry) {
      back();
    }
  });
</script>

{#if open}
  <div class="m-scrim" onclick={() => (open = false)} role="presentation"></div>
  <div class="m-sheet" class:menu role="dialog" aria-label={title || undefined}>
    {#if title}<div class="m-sheet-title">{title}</div>{/if}
    {@render children()}
  </div>
{/if}

<style>
  .menu {
    right: auto;
    left: 20px;
    width: min(271px, calc(100vw - 39px));
    bottom: 79px;
    padding: 7px 0;
  }
</style>
