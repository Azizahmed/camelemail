<script lang="ts">
  import { X } from "@lucide/svelte";
  import { api } from "../../lib/api";
  import type { Contact, EmailAddress } from "../../lib/types";

  let {
    label,
    value = $bindable([]),
    autofocus = false,
    trailing,
  }: { label: string; value: EmailAddress[]; autofocus?: boolean; trailing?: import("svelte").Snippet } = $props();

  let text = $state("");
  let suggestions: Contact[] = $state([]);
  let index = $state(0);
  let input: HTMLInputElement | undefined = $state();
  let timer: ReturnType<typeof setTimeout> | undefined;
  /** Bumped on every query and commit: only the newest suggestion reply may show. */
  let seq = 0;

  $effect(() => {
    if (autofocus) queueMicrotask(() => input?.focus());
  });

  function parse(s: string): EmailAddress | null {
    const t = s.trim().replace(/[,;]$/, "").trim();
    if (!t) return null;
    const m = t.match(/^(.*)<([^>]+)>$/);
    if (m) {
      const email = m[2].trim();
      return email.includes("@") ? { name: m[1].trim().replace(/^"|"$/g, ""), email } : null;
    }
    return t.includes("@") && !/\s/.test(t) ? { name: "", email: t } : null;
  }

  function add(a: EmailAddress | null) {
    if (!a) return false;
    if (!value.some((v) => v.email.toLowerCase() === a.email.toLowerCase())) value = [...value, a];
    text = "";
    suggestions = [];
    clearTimeout(timer);
    seq++;
    return true;
  }

  function commit() {
    if (text.trim()) add(parse(text));
  }

  function onInput() {
    // Pasted lists: split on commas/semicolons/newlines.
    if (/[,;\n]/.test(text)) {
      const parts = text.split(/[,;\n]/);
      const last = parts.pop() ?? "";
      for (const p of parts) add(parse(p));
      text = last;
    }
    clearTimeout(timer);
    const mine = ++seq;
    const q = text.trim();
    if (q.length < 1) {
      suggestions = [];
      return;
    }
    timer = setTimeout(async () => {
      const found = await api.suggestContacts(q).catch(() => []);
      // A slower reply for older text, or the field was committed meanwhile.
      if (mine !== seq || text.trim() !== q) return;
      suggestions = found.filter((c) => !value.some((v) => v.email.toLowerCase() === c.email.toLowerCase()));
      index = 0;
    }, 120);
  }

  function keydown(e: KeyboardEvent) {
    if (suggestions.length > 0 && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
      e.preventDefault();
      index = (index + (e.key === "ArrowDown" ? 1 : -1) + suggestions.length) % suggestions.length;
      return;
    }
    if (e.key === "Enter" || e.key === "Tab" || e.key === ",") {
      if (suggestions.length > 0 && text.trim()) {
        e.preventDefault();
        const c = suggestions[index];
        add({ name: c.name, email: c.email });
        return;
      }
      if (text.trim()) {
        if (e.key !== "Tab") e.preventDefault();
        commit();
      }
      return;
    }
    if (e.key === "Backspace" && !text && value.length > 0) {
      value = value.slice(0, -1);
    }
    if (e.key === "Escape" && suggestions.length) {
      e.stopPropagation();
      suggestions = [];
    }
  }
</script>

<div class="field">
  <span class="label">{label}</span>
  <div class="chips">
    {#each value as a (a.email)}
      <span class="chip" title={a.email}>
        <bdi>{a.name || a.email}</bdi>
        <button title="Remove" onclick={() => (value = value.filter((v) => v !== a))}><X size={11} /></button>
      </span>
    {/each}
    <input
      bind:this={input}
      bind:value={text}
      oninput={onInput}
      onkeydown={keydown}
      onblur={() => setTimeout(() => { commit(); suggestions = []; seq++; }, 150)}
      spellcheck="false"
      autocomplete="off"
      dir="auto"
    />
  </div>
  {@render trailing?.()}
  {#if suggestions.length > 0}
    <div class="suggest">
      {#each suggestions as c, i (c.email)}
        <button class="menu-item" class:selected={i === index} onmousedown={(e) => { e.preventDefault(); add({ name: c.name, email: c.email }); }}>
          <span class="s-name" dir="auto">{c.name || c.email}</span>
          {#if c.name}<span class="detail" dir="ltr">{c.email}</span>{/if}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .field {
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 10px 0;
    border-bottom: 1px solid var(--separator-subtle);
  }
  .label {
    width: 34px;
    flex: none;
    color: var(--content-subtle);
    font-size: 13px;
    line-height: 24px;
  }
  .chips {
    flex: 1;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
    min-width: 0;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 12px;
    padding: 2px 4px 2px 8px;
    border-radius: 8px;
    background: var(--surface-hover);
    max-width: 260px;
    overflow: hidden;
    white-space: nowrap;
  }
  .chip button {
    border: none;
    background: none;
    display: flex;
    padding: 2px;
    color: var(--icon);
    border-radius: 4px;
  }
  .chip button:hover {
    background: var(--surface-active);
  }
  input {
    flex: 1;
    min-width: 120px;
    border: none;
    outline: none;
    background: none;
    font-size: 13px;
    line-height: 24px;
    user-select: text;
  }
  .suggest {
    position: absolute;
    left: 44px;
    top: 100%;
    z-index: 50;
    width: min(420px, 90%);
    background: var(--surface-elevated);
    border: 0.5px solid var(--separator);
    border-radius: 10px;
    box-shadow: var(--shadow-dropdown);
    padding: 4px;
  }
  .s-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
