<script lang="ts" module>
  import type { Component } from "svelte";

  export interface SlashItem {
    id: string;
    label: string;
    hint: string;
    icon: Component<{ size?: number }>;
    /** Extra words the filter matches. */
    keywords?: string;
  }

  /** Items whose label or keywords start a word with the query. */
  export function filterItems(items: SlashItem[], query: string): SlashItem[] {
    const q = query.trim().toLowerCase();
    if (!q) return items;
    return items.filter((i) => `${i.label} ${i.keywords ?? ""}`.toLowerCase().split(/[\s-]+/).some((w) => w.startsWith(q)));
  }
</script>

<script lang="ts">
  import { placeMenu } from "../../lib/ui.svelte";

  // The editor keeps the focus: it moves `active` and picks with the keyboard.
  let {
    anchor,
    items,
    active,
    onpick,
    onhover,
  }: {
    anchor: DOMRect;
    items: SlashItem[];
    active: number;
    onpick: (item: SlashItem) => void;
    onhover: (index: number) => void;
  } = $props();

  const WIDTH = 260;
  const pos = $derived(placeMenu(anchor, WIDTH, Math.min(320, items.length * 44 + 40), "left"));
  let el: HTMLDivElement | undefined = $state();

  $effect(() => {
    el?.querySelector(`[data-i="${active}"]`)?.scrollIntoView({ block: "nearest" });
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  bind:this={el}
  class="menu slash"
  role="listbox"
  tabindex="-1"
  aria-label="Insert"
  style:left="{pos.left}px"
  style:top="{pos.top}px"
  style:width="{WIDTH}px"
  onmousedown={(e) => e.preventDefault()}
>
  {#if items.length === 0}
    <div class="none">No matching blocks</div>
  {:else}
    <div class="menu-title">Blocks</div>
    {#each items as item, i (item.id)}
      {@const Icon = item.icon}
      <button
        class="menu-item"
        class:selected={i === active}
        role="option"
        aria-selected={i === active}
        data-i={i}
        onmousemove={() => i !== active && onhover(i)}
        onclick={() => onpick(item)}
      >
        <span class="ico"><Icon size={16} /></span>
        <span class="text">
          <span class="label">{item.label}</span>
          <span class="hint">{item.hint}</span>
        </span>
      </button>
    {/each}
  {/if}
</div>

<style>
  .slash {
    max-height: 320px;
  }
  .ico {
    width: 30px;
    height: 30px;
    flex: none;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: 7px;
    border: 0.5px solid var(--separator);
    background: var(--surface);
    color: var(--icon);
  }
  .text {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .label {
    font-size: 13px;
    line-height: 17px;
  }
  .hint {
    font-size: 11.5px;
    line-height: 15px;
    color: var(--content-subtle);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .menu-item {
    padding: 5px 8px;
  }
  .none {
    padding: 8px;
    font-size: 12px;
    color: var(--content-subtle);
  }
</style>
