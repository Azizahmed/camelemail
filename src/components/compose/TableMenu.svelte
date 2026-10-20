<script lang="ts">
  import {
    AlignCenter,
    AlignLeft,
    AlignRight,
    ArrowDownToLine,
    ArrowLeftToLine,
    ArrowRightToLine,
    ArrowUpToLine,
    Bold,
    Check,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    Copy,
    Eraser,
    Italic,
    PanelLeft,
    PanelTop,
    Strikethrough,
    Trash2,
  } from "@lucide/svelte";
  import { BACKGROUNDS, TEXT_COLORS, type Align, type Part, type TableAction, type partState } from "../../lib/table";
  import Popover from "../menus/Popover.svelte";

  let {
    anchor,
    part,
    state: st,
    rtl,
    onaction,
    onclose,
  }: {
    anchor: DOMRect;
    part: Part;
    state: ReturnType<typeof partState>;
    /** The table runs right to left: "left" is toward the end. */
    rtl: boolean;
    onaction: (a: TableAction) => void;
    onclose: () => void;
  } = $props();

  const row = $derived(part === "row");
  /** Column "before/after" in on-screen terms. */
  const leftIs = $derived(rtl ? "After" : "Before") as "Before" | "After";
  const rightIs = $derived(rtl ? "Before" : "After") as "Before" | "After";
  const atLeft = $derived(rtl ? st.last : st.first);
  const atRight = $derived(rtl ? st.first : st.last);
  const ALIGNS: [Align, typeof AlignLeft, string][] = [
    ["left", AlignLeft, "Align left"],
    ["center", AlignCenter, "Align center"],
    ["right", AlignRight, "Align right"],
  ];
  const currentAlign = $derived(st.align || (rtl ? "right" : "left"));
</script>

<Popover {anchor} width={270} height={560} align="left" {onclose}>
  <div class="tmenu">
    {#if row}
      <button class="menu-item" onclick={() => onaction({ type: "insertBefore" })}><ArrowUpToLine size={15} /> Insert row above</button>
      <button class="menu-item" onclick={() => onaction({ type: "insertAfter" })}><ArrowDownToLine size={15} /> Insert row below</button>
      {#if !st.first}<button class="menu-item" onclick={() => onaction({ type: "moveBefore" })}><ChevronUp size={15} /> Move up</button>{/if}
      {#if !st.last}<button class="menu-item" onclick={() => onaction({ type: "moveAfter" })}><ChevronDown size={15} /> Move down</button>{/if}
    {:else}
      <button class="menu-item" onclick={() => onaction({ type: `insert${leftIs}` })}><ArrowLeftToLine size={15} /> Insert column left</button>
      <button class="menu-item" onclick={() => onaction({ type: `insert${rightIs}` })}><ArrowRightToLine size={15} /> Insert column right</button>
      {#if !atLeft}<button class="menu-item" onclick={() => onaction({ type: `move${leftIs}` })}><ChevronLeft size={15} /> Move left</button>{/if}
      {#if !atRight}<button class="menu-item" onclick={() => onaction({ type: `move${rightIs}` })}><ChevronRight size={15} /> Move right</button>{/if}
    {/if}
    <button class="menu-item" onclick={() => onaction({ type: "duplicate" })}><Copy size={15} /> Duplicate</button>

    <div class="menu-sep"></div>
    <div class="label">Background</div>
    <div class="swatches">
      {#each BACKGROUNDS as [color, name] (name)}
        <button
          class="swatch"
          class:on={st.background === color}
          class:none={!color}
          title={name}
          aria-label="{name} background"
          style:background={color || null}
          onclick={() => onaction({ type: "background", color })}
        ></button>
      {/each}
    </div>
    <div class="label">Text color</div>
    <div class="swatches">
      {#each TEXT_COLORS as [color, name] (name)}
        <button class="swatch letter" class:on={st.color === color} title={name} aria-label="{name} text" style:color={color || null} onclick={() => onaction({ type: "color", color })}>A</button>
      {/each}
    </div>
    <div class="label">Alignment</div>
    <div class="swatches">
      {#each ALIGNS as [a, Icon, name] (a)}
        <button class="swatch wide" class:on={currentAlign === a} title={name} aria-label={name} onclick={() => onaction({ type: "align", align: a })}><Icon size={15} /></button>
      {/each}
    </div>

    <div class="menu-sep"></div>
    <button class="menu-item" onclick={() => onaction({ type: "bold" })}><Bold size={15} /> Bold {#if st.bold}<Check size={14} class="tick" />{/if}</button>
    <button class="menu-item" onclick={() => onaction({ type: "italic" })}><Italic size={15} /> Italic {#if st.italic}<Check size={14} class="tick" />{/if}</button>
    <button class="menu-item" onclick={() => onaction({ type: "strike" })}><Strikethrough size={15} /> Strikethrough {#if st.strike}<Check size={14} class="tick" />{/if}</button>
    <button class="menu-item" onclick={() => onaction({ type: "clear" })}><Eraser size={15} /> Clear formatting</button>

    <div class="menu-sep"></div>
    {#if st.canHeader}
      <button class="menu-item" onclick={() => onaction({ type: "header" })}>
        {#if row}<PanelTop size={15} /> Header row{:else}<PanelLeft size={15} /> Header column{/if}
        {#if st.header}<Check size={14} class="tick" />{/if}
      </button>
    {/if}
    <button class="menu-item danger" onclick={() => onaction({ type: "delete" })}><Trash2 size={15} /> Delete {row ? "row" : "column"}</button>
  </div>
</Popover>

<style>
  .tmenu :global(.menu-item) {
    padding: 6px 8px;
    color: var(--content);
  }
  .tmenu :global(.menu-item svg) {
    color: var(--icon);
    flex: none;
  }
  .tmenu :global(.tick) {
    margin-left: auto;
    color: var(--accent) !important;
  }
  .danger,
  .danger :global(svg) {
    color: var(--indicator-error) !important;
  }
  .label {
    font-size: 11.5px;
    color: var(--content-subtle);
    padding: 6px 8px 4px;
  }
  .swatches {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    padding: 0 8px 4px;
  }
  .swatch {
    width: 20px;
    height: 20px;
    border-radius: 5px;
    border: 0.5px solid var(--separator);
    background: var(--surface);
    padding: 0;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 11px;
    font-weight: 500;
    color: var(--content);
  }
  .swatch.wide {
    width: 30px;
    color: var(--icon);
  }
  .swatch.on {
    outline: 1.5px solid var(--accent);
    outline-offset: 1px;
  }
  /* "No colour": a red slash. */
  .swatch.none {
    background: linear-gradient(to top right, transparent calc(50% - 1px), #d44c47 calc(50% - 1px), #d44c47 calc(50% + 1px), transparent calc(50% + 1px)) var(--surface);
  }
</style>
