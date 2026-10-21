<!--
  The app's drop-down list. Native <select>s are drawn by GTK on Linux: their
  text sits off-centre and the open list is a black system menu. This one
  looks the same on every system and in both themes.
-->
<script lang="ts" generics="T extends string | number">
  import { Check, ChevronDown } from "@lucide/svelte";
  import Popover from "./menus/Popover.svelte";

  let {
    value = $bindable(),
    options,
    onchange,
    disabled = false,
    id,
    label,
    width,
  }: {
    value: T;
    options: { value: T; label: string }[];
    onchange?: (value: T) => void;
    disabled?: boolean;
    id?: string;
    /** Accessible name when no <label for> points at it. */
    label?: string;
    /** Fixed width in px; otherwise it fills its container like an input. */
    width?: number;
  } = $props();

  let button: HTMLButtonElement | undefined = $state();
  let open = $state<DOMRect | null>(null);
  let active = $state(0);

  const current = $derived(options.find((o) => o.value === value));

  function show() {
    if (disabled || !button) return;
    active = Math.max(0, options.findIndex((o) => o.value === value));
    open = button.getBoundingClientRect();
  }

  function pick(v: T) {
    open = null;
    button?.focus();
    if (v === value) return;
    value = v;
    onchange?.(v);
  }

  function onKey(e: KeyboardEvent) {
    if (!open) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp" || e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        show();
      }
      return;
    }
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const n = options.length;
      active = (active + (e.key === "ArrowDown" ? 1 : n - 1)) % n;
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      const o = options[active];
      if (o) pick(o.value);
    } else if (e.key === "Tab") {
      open = null;
    }
  }
</script>

<button
  bind:this={button}
  {id}
  type="button"
  class="select"
  class:open={!!open}
  style:width={width ? `${width}px` : null}
  {disabled}
  aria-haspopup="listbox"
  aria-expanded={!!open}
  aria-label={label}
  onclick={() => (open ? (open = null) : show())}
  onkeydown={onKey}
>
  <span class="text" dir="auto">{current?.label ?? ""}</span>
  <ChevronDown size={14} class="chev" />
</button>

{#if open}
  <Popover anchor={open} width={Math.max(open.width, 160)} height={Math.min(320, options.length * 32 + 12)} align="left" onclose={() => (open = null)}>
    <div class="list" role="listbox" aria-label={label}>
      {#each options as o, i (o.value)}
        <button
          type="button"
          class="menu-item opt"
          class:selected={i === active}
          role="option"
          aria-selected={o.value === value}
          onmousedown={(e) => e.preventDefault()}
          onmousemove={() => (active = i)}
          onclick={() => pick(o.value)}
        >
          <span class="label" dir="auto">{o.label}</span>
          {#if o.value === value}<Check size={14} class="tick" />{/if}
        </button>
      {/each}
    </div>
  </Popover>
{/if}

<style>
  .select {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    min-width: 0;
    height: 30px;
    padding: 0 8px 0 10px;
    border-radius: var(--radius-lg);
    border: 0.5px solid var(--separator);
    background: var(--surface-overlay);
    color: var(--content);
    font: inherit;
    font-size: 13px;
    line-height: 1;
    text-align: start;
    cursor: pointer;
    transition: border-color var(--fast), box-shadow var(--fast);
  }
  .select:hover:not(:disabled) {
    border-color: var(--content-muted);
  }
  .select:focus-visible,
  .select.open {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .select:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .text {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .select :global(.chev) {
    flex: none;
    color: var(--icon);
  }
  .opt {
    padding: 6px 8px;
  }
  .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .opt :global(.tick) {
    flex: none;
    color: var(--accent);
  }
</style>
