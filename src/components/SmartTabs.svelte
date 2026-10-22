<script lang="ts">
  import { Inbox, Layers, Megaphone, MessagesSquare, Bell, Users } from "@lucide/svelte";
  import { mail, setSmartTab } from "../lib/mail.svelte";
  import Select from "./Select.svelte";
  import type { Category } from "../lib/types";

  let { compact = false }: { compact?: boolean } = $props();

  const BUCKETS: { id: Exclude<Category, "primary">; label: string; icon: typeof Inbox }[] = [
    { id: "updates", label: "Updates", icon: Bell },
    { id: "promotions", label: "Promotions", icon: Megaphone },
    { id: "social", label: "Social", icon: Users },
    { id: "forums", label: "Forums", icon: MessagesSquare },
  ];

  type OtherPick = "all" | Exclude<Category, "primary">;
  const OPTIONS: { value: OtherPick; label: string }[] = [{ value: "all", label: "All" }, ...BUCKETS.map((b) => ({ value: b.id, label: b.label }))];
  // Picking a category also switches to Other.
  const picked = $derived<OtherPick>(mail.smart.tab === "other" ? (mail.smart.other ?? "all") : "all");
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
  <div class="other-pick" class:on={mail.smart.tab === "other"}>
    <Select size="sm" value={picked} options={OPTIONS} label="Other: category" onchange={(v) => setSmartTab("other", v === "all" ? null : v)} />
  </div>
</div>

<style>
  /* One row: the category picker sits right after the Other tab and
     shrinks (its text ellipsizes) rather than wrapping below the tabs. */
  .smart {
    display: flex;
    align-items: center;
    gap: 6px;
    padding-block: 6px 8px;
    padding-inline: 36px 12px;
    container-type: inline-size;
  }
  .smart.compact {
    padding-inline: 10px;
  }
  .seg {
    display: inline-flex;
    flex: none;
    padding: 2px;
    border-radius: 9px;
    background: var(--surface-hover);
  }
  .seg button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    /* With the 2px track: 26px, the picker's height. */
    height: 22px;
    border: none;
    background: none;
    padding: 0 12px;
    border-radius: 7px;
    font-size: 12.5px;
    font-weight: 500;
    white-space: nowrap;
    color: var(--content-subtle);
  }
  /* Narrow list column: tighter tabs, then no tab icons. */
  @container (max-width: 400px) {
    .seg button {
      gap: 4px;
      padding: 0 9px;
    }
  }
  @container (max-width: 300px) {
    .seg button :global(svg) {
      display: none;
    }
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
  .other-pick {
    display: flex;
    flex: 0 1 auto;
    min-width: 0;
    opacity: 0.85;
  }
  .other-pick.on {
    opacity: 1;
  }
</style>
