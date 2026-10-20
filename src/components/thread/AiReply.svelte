<script lang="ts">
  import { Sparkles, Square, X } from "@lucide/svelte";
  import { onDestroy } from "svelte";
  import { api, errorText, newRequestId } from "../../lib/api";
  import type { Draft, ReplyKind } from "../../lib/types";
  import { toast } from "../../lib/ui.svelte";

  let {
    messageId,
    kind,
    ondone,
    oncancel,
  }: { messageId: string; kind: ReplyKind; ondone: (d: Draft) => void; oncancel: () => void } = $props();

  const QUICK = ["Yes, sounds good", "Politely decline", "Ask for more details", "Thank them", "Suggest a meeting time"];

  let wish = $state("");
  let running = $state(false);
  let preview = $state("");
  let requestId = "";
  let input: HTMLInputElement | undefined = $state();
  let destroyed = false;

  $effect(() => {
    input?.focus();
  });

  async function generate(text = wish) {
    if (running) return;
    wish = text;
    running = true;
    preview = "";
    requestId = newRequestId();
    try {
      const out = await api.aiRun(requestId, { type: "reply", messageId, kind, instructions: text }, (d) => (preview += d));
      // Another thread opened meanwhile: the draft stays in Drafts, not in this view.
      if (out.draft && !destroyed) ondone(out.draft);
    } catch (e) {
      const msg = errorText(e);
      if (msg !== "Stopped") toast(msg, { kind: "error", duration: 6000 });
    } finally {
      running = false;
    }
  }

  onDestroy(() => {
    destroyed = true;
    if (running) void api.aiCancel(requestId);
  });
</script>

<div class="ai-reply">
  <div class="row">
    <Sparkles size={15} />
    <input
      bind:this={input}
      class="prompt"
      dir="auto"
      placeholder={kind === "forward" ? "Add a note for the forward… (optional)" : "What should the reply say? (optional)"}
      bind:value={wish}
      disabled={running}
      onkeydown={(e) => {
        if (e.key === "Enter") { e.preventDefault(); void generate(); }
        if (e.key === "Escape") { e.stopPropagation(); if (running) void api.aiCancel(requestId); else oncancel(); }
      }}
    />
    {#if running}
      <button class="btn" onclick={() => api.aiCancel(requestId)}><Square size={12} /> Stop</button>
    {:else}
      <button class="btn primary" onclick={() => generate()}>Write</button>
      <button class="icon-btn" title="Cancel" onclick={oncancel}><X size={15} /></button>
    {/if}
  </div>
  {#if running}
    <div class="preview selectable" dir="auto">{preview || "Writing…"}</div>
  {:else}
    <div class="quick">
      {#each QUICK as q (q)}
        <button class="chip" onclick={() => generate(q)}>{q}</button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .ai-reply {
    margin: 24px 0 0 44px;
    padding: 10px 12px;
    border-radius: 12px;
    border: 1px solid var(--accent);
    background: var(--surface-overlay);
    box-shadow: var(--shadow-panel);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--accent);
  }
  .prompt {
    flex: 1;
    border: none;
    outline: none;
    background: none;
    font-size: 13.5px;
    padding: 6px 0;
    user-select: text;
  }
  .quick {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding-top: 8px;
  }
  .chip {
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
  .preview {
    margin-top: 8px;
    font-size: 13px;
    line-height: 1.45;
    white-space: pre-wrap;
    color: var(--content-subtle);
    max-height: 220px;
    overflow-y: auto;
  }
</style>
