<script lang="ts">
  import { bindingCaps } from "../lib/shortcuts.svelte";

  /** A shortcut in binding notation ("Ctrl+K", "g i"), shown as key caps. */
  let { binding }: { binding: string } = $props();
</script>

<span class="caps">
  {#each bindingCaps(binding) as caps, s (s)}
    {#if s > 0}<span class="sep">then</span>{/if}
    {#each caps as cap, i (i)}
      {#if i > 0}<span class="sep">+</span>{/if}<kbd>{cap}</kbd>
    {/each}
  {/each}
</span>

<style>
  .caps {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    white-space: nowrap;
  }
  .sep {
    font-size: 11px;
    color: var(--content-muted);
  }
</style>
