<script lang="ts" module>
  /** What the selection already has, for the pressed look. */
  export interface FormatState {
    bold: boolean;
    italic: boolean;
    underline: boolean;
    strike: boolean;
    highlight: boolean;
    block: string;
    list: "ul" | "ol" | "todo" | "";
  }

  export type FormatAction =
    | "h1"
    | "h2"
    | "h3"
    | "text"
    | "todo"
    | "ul"
    | "ol"
    | "bold"
    | "italic"
    | "underline"
    | "strike"
    | "highlight"
    | "table"
    | "attach"
    | "quote"
    | "code"
    | "divider"
    | "clear"
    | "ai";
</script>

<script lang="ts">
  import {
    Bold,
    ChevronDown,
    Code,
    Ellipsis,
    Heading,
    Highlighter,
    Italic,
    Link,
    List,
    ListOrdered,
    Minus,
    Paperclip,
    Quote,
    RemoveFormatting,
    Sparkles,
    SquareCheck,
    Strikethrough,
    Table,
    Underline,
  } from "@lucide/svelte";

  let {
    rect,
    state: fmt,
    ai,
    linking,
    onaction,
    onstartlink,
    onlink,
  }: {
    /** The selection on screen. */
    rect: DOMRect;
    state: FormatState;
    ai: boolean;
    /** Showing the link field instead of the buttons. */
    linking: boolean;
    onaction: (a: FormatAction) => void;
    onstartlink: () => void;
    /** Add a link to the selection (empty string: cancelled). */
    onlink: (url: string) => void;
  } = $props();

  let sub = $state<null | "heading" | "list" | "more">(null);
  let url = $state("https://");
  let width = $state(380);
  let height = $state(44);

  const pos = $derived.by(() => {
    const margin = 8;
    const left = Math.max(margin, Math.min(rect.left + rect.width / 2 - width / 2, window.innerWidth - width - margin));
    let top = rect.top - height - 8;
    if (top < margin) top = rect.bottom + 8;
    return { left, top };
  });

  function act(a: FormatAction) {
    sub = null;
    onaction(a);
  }

  function finishLink(ok: boolean) {
    const u = url.trim();
    url = "https://";
    onlink(ok && u && u !== "https://" ? u : "");
  }
</script>

<!-- Buttons keep the editor's selection: nothing here takes focus on mousedown except the link field. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="bubble"
  role="toolbar"
  tabindex="-1"
  aria-label="Text formatting"
  style:left="{pos.left}px"
  style:top="{pos.top}px"
  bind:offsetWidth={width}
  bind:offsetHeight={height}
  onmousedown={(e) => {
    if (!(e.target as Element).closest("input")) e.preventDefault();
  }}
>
  {#if linking}
    <Link size={15} />
    <!-- svelte-ignore a11y_autofocus -->
    <input
      class="link-input"
      bind:value={url}
      autofocus
      spellcheck="false"
      onkeydown={(e) => {
        if (e.key === "Enter") { e.preventDefault(); finishLink(true); }
        if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); finishLink(false); }
      }}
    />
    <button class="btn primary small" onclick={() => finishLink(true)}>Add</button>
  {:else}
    <div class="group">
      <button class="tb drop" class:on={/^H[1-3]$/.test(fmt.block)} title="Heading" onclick={() => (sub = sub === "heading" ? null : "heading")}>
        <Heading size={17} /><ChevronDown size={11} />
      </button>
      <button class="tb" class:on={fmt.list === "todo"} title="To-do list" onclick={() => act("todo")}><SquareCheck size={16} /></button>
      <button class="tb drop" class:on={fmt.list === "ul" || fmt.list === "ol"} title="List" onclick={() => (sub = sub === "list" ? null : "list")}>
        <List size={17} /><ChevronDown size={11} />
      </button>
    </div>
    <span class="sep"></span>
    <div class="group">
      <button class="tb" class:on={fmt.bold} title="Bold (Ctrl+B)" onclick={() => act("bold")}><Bold size={16} /></button>
      <button class="tb" class:on={fmt.italic} title="Italic (Ctrl+I)" onclick={() => act("italic")}><Italic size={16} /></button>
      <button class="tb hl" class:on={fmt.highlight} title="Highlight" onclick={() => act("highlight")}><Highlighter size={16} /></button>
    </div>
    <span class="sep"></span>
    <div class="group">
      <button class="tb" title="Link (Ctrl+K)" onclick={() => { sub = null; onstartlink(); }}><Link size={16} /></button>
      <button class="tb" title="Table" onclick={() => act("table")}><Table size={16} /></button>
      <button class="tb" title="Attach files" onclick={() => act("attach")}><Paperclip size={16} /></button>
      <button class="tb" title="More" class:on={sub === "more"} onclick={() => (sub = sub === "more" ? null : "more")}><Ellipsis size={16} /></button>
    </div>
  {/if}

  {#if sub === "heading"}
    <div class="sub" style:left="4px">
      <button class="menu-item" class:selected={fmt.block === "H1"} onclick={() => act("h1")}><span class="h1">Heading 1</span></button>
      <button class="menu-item" class:selected={fmt.block === "H2"} onclick={() => act("h2")}><span class="h2">Heading 2</span></button>
      <button class="menu-item" class:selected={fmt.block === "H3"} onclick={() => act("h3")}><span class="h3">Heading 3</span></button>
      <button class="menu-item" class:selected={!/^H[1-3]$/.test(fmt.block)} onclick={() => act("text")}>Text</button>
    </div>
  {:else if sub === "list"}
    <div class="sub" style:left="76px">
      <button class="menu-item" class:selected={fmt.list === "ul"} onclick={() => act("ul")}><List size={15} /> Bulleted list</button>
      <button class="menu-item" class:selected={fmt.list === "ol"} onclick={() => act("ol")}><ListOrdered size={15} /> Numbered list</button>
    </div>
  {:else if sub === "more"}
    <div class="sub" style:right="4px">
      <button class="menu-item" class:selected={fmt.underline} onclick={() => act("underline")}><Underline size={15} /> Underline</button>
      <button class="menu-item" class:selected={fmt.strike} onclick={() => act("strike")}><Strikethrough size={15} /> Strikethrough</button>
      <button class="menu-item" class:selected={fmt.block === "BLOCKQUOTE"} onclick={() => act("quote")}><Quote size={15} /> Quote</button>
      <button class="menu-item" class:selected={fmt.block === "PRE"} onclick={() => act("code")}><Code size={15} /> Code</button>
      <button class="menu-item" onclick={() => act("divider")}><Minus size={15} /> Divider</button>
      <button class="menu-item" onclick={() => act("clear")}><RemoveFormatting size={15} /> Clear formatting</button>
      {#if ai}
        <div class="menu-sep"></div>
        <button class="menu-item" onclick={() => act("ai")}><Sparkles size={15} /> Ask AI…</button>
      {/if}
    </div>
  {/if}
</div>

<style>
  .bubble {
    position: fixed;
    z-index: 350;
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 5px 8px;
    border-radius: 14px;
    background: var(--surface-elevated);
    border: 0.5px solid var(--separator);
    box-shadow: var(--shadow-dropdown);
    color: var(--icon);
    animation: pop 0.12s ease;
    white-space: nowrap;
  }
  .group {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .sep {
    width: 1px;
    height: 20px;
    margin: 0 6px;
    background: var(--separator);
  }
  .tb {
    height: 30px;
    min-width: 30px;
    padding: 0 5px;
    border: none;
    border-radius: 8px;
    background: none;
    color: var(--icon);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 1px;
  }
  .tb:hover {
    background: var(--surface-active);
    color: var(--icon-hover);
  }
  .tb.on {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .tb.hl {
    color: var(--accent);
  }
  .link-input {
    width: 240px;
    height: 28px;
    border: none;
    outline: none;
    background: none;
    font-size: 13px;
    color: var(--content);
    user-select: text;
  }
  .small {
    height: 26px;
    padding: 0 10px;
  }
  .sub {
    position: absolute;
    top: calc(100% + 6px);
    min-width: 180px;
    padding: 6px;
    border-radius: var(--radius-xl);
    background: var(--surface-elevated);
    border: 0.5px solid var(--separator);
    box-shadow: var(--shadow-dropdown);
  }
  .h1 {
    font-size: 18px;
    font-weight: 700;
  }
  .h2 {
    font-size: 15.5px;
    font-weight: 650;
  }
  .h3 {
    font-size: 13.5px;
    font-weight: 650;
  }
</style>
