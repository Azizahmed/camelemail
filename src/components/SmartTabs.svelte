<script lang="ts">
  import { Inbox, Layers, Megaphone, MessagesSquare, Bell, Users } from "@lucide/svelte";
  import { mail, setSmartTab } from "../lib/mail.svelte";
  import type { Category } from "../lib/types";

  let { compact = false }: { compact?: boolean } = $props();

  const BUCKETS: { id: Exclude<Category, "primary">; label: string; icon: typeof Inbox }[] = [
    { id: "updates", label: "Updates", icon: Bell },
    { id: "promotions", label: "Promotions", icon: Megaphone },
    { id: "social", label: "Social", icon: Users },
    { id: "forums", label: "Forums", icon: MessagesSquare },
  ];
</script>

<div class="smart" class:compact>
  <div class="seg" role="tablist" aria-label="Smart Inbox">
    <button role="tab" aria-selected={mail.smart.tab === "important"} class:on={mail.smart.tab === "important"} onclick={() => setSmartTab("important")}>
      <Inbox size={14} /> Important
      {#if mail.smart.counts.important > 0}<span class="n">{mail.smart.counts.important}</span>{/if}
    </button>
    <button role="tab" aria-selected={mail.smart.tab === "other"} class:on={mail.smart.tab === "other"} onclick={() => setSmartTab("other")}>
      <Layers size={14} /> Other
      {#if mail.smart.counts.other > 0}<span class="n">{mail.smart.counts.other}</span>{/if}
    </button>
  </div>
  {#if mail.smart.tab === "other"}
    <div class="chips">
      <button class="chip" class:on={mail.smart.other === null} onclick={() => setSmartTab("other", null)}>All</button>
      {#each BUCKETS as b (b.id)}
        <button class="chip" class:on={mail.smart.other === b.id} onclick={() => setSmartTab("other", b.id)}>
          <b.icon size={12} />
          {b.label}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .smart {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px 14px;
    padding: 6px 12px 8px 36px;
  }
  .smart.compact {
    padding: 6px 10px 8px;
  }
  .seg {
    display: inline-flex;
    padding: 2px;
    border-radius: 9px;
    background: var(--surface-hover);
  }
  .seg button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: none;
    background: none;
    padding: 4px 12px;
    border-radius: 7px;
    font-size: 12.5px;
    font-weight: 500;
    color: var(--content-subtle);
  }
  .seg button.on {
    background: var(--surface-overlay);
    color: var(--content);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.08);
  }
  .seg button.on :global(svg) {
    color: var(--accent);
  }
  .n {
    font-size: 11px;
    min-width: 18px;
    padding: 0 5px;
    border-radius: 9px;
    background: var(--accent-soft);
    color: var(--accent);
    text-align: center;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 12px;
    border: 0.5px solid var(--border-tertiary);
    background: transparent;
    border-radius: 999px;
    padding: 3px 10px;
    color: var(--content-subtle);
  }
  .chip:hover {
    color: var(--content);
    background: var(--surface-hover);
  }
  .chip.on {
    border-color: var(--accent);
    color: var(--accent);
    background: var(--accent-soft);
  }
</style>
