<script lang="ts">
  import { RefreshCw, Sparkles, Square, X } from "@lucide/svelte";
  import { onDestroy, onMount } from "svelte";
  import { api, errorText, newRequestId } from "../../lib/api";
  import { textDirection } from "../../lib/bidi";
  import { openLinks, renderMarkdown } from "../../lib/markdown";

  let { accountId, threadId, onclose }: { accountId: string; threadId: string; onclose: () => void } = $props();

  let text = $state("");
  let running = $state(false);
  let error = $state("");
  let requestId = "";
  // Streamed text reaches the screen (and the Markdown parser) once per frame.
  let streamed = "";
  let frame = 0;

  function onDelta(d: string) {
    streamed += d;
    if (!frame) frame = requestAnimationFrame(() => {
      frame = 0;
      text = streamed;
    });
  }

  async function run() {
    if (running) return;
    text = "";
    streamed = "";
    error = "";
    running = true;
    requestId = newRequestId();
    try {
      const out = await api.aiRun(requestId, { type: "summarize", accountId, threadId }, onDelta);
      cancelAnimationFrame(frame);
      frame = 0;
      text = out.text;
    } catch (e) {
      const msg = errorText(e);
      if (msg !== "Stopped") error = msg;
    } finally {
      running = false;
    }
  }

  function stop() {
    void api.aiCancel(requestId);
  }

  onMount(run);
  onDestroy(() => {
    cancelAnimationFrame(frame);
    if (running) stop();
  });
</script>

<div class="summary">
  <div class="head">
    <Sparkles size={14} />
    <span>Summary</span>
    <span class="grow"></span>
    {#if running}
      <button class="icon-btn" title="Stop" onclick={stop}><Square size={13} /></button>
    {:else}
      <button class="icon-btn" title="Summarize again" onclick={run}><RefreshCw size={13} /></button>
    {/if}
    <button class="icon-btn" title="Hide summary" onclick={onclose}><X size={14} /></button>
  </div>
  {#if error}
    <div class="error selectable">{error}</div>
  {:else if !text && running}
    <div class="thinking"><span class="spinner"></span> Reading the conversation…</div>
  {:else}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <!-- The whole card follows the summary's language, so an Arabic summary lines up on the right. -->
    <div class="md selectable" dir={textDirection(text) ?? "auto"} onclick={openLinks}>{@html renderMarkdown(text)}</div>
  {/if}
</div>

<style>
  .summary {
    margin: 10px 0 4px 44px;
    padding: 10px 14px 12px;
    border-radius: 12px;
    background: linear-gradient(135deg, var(--accent-soft), transparent 70%), var(--surface-elevated);
    border: 0.5px solid var(--separator);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    font-weight: 600;
    color: var(--accent);
    margin-bottom: 4px;
  }
  .grow {
    flex: 1;
  }
  .thinking,
  .error {
    font-size: 12.5px;
    color: var(--content-subtle);
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 0;
  }
  .error {
    color: var(--indicator-error);
  }
  .md {
    font-size: 13px;
    line-height: 1.5;
    overflow-wrap: anywhere;
  }
  .md :global(p) {
    margin: 4px 0;
  }
  .md :global(ul),
  .md :global(ol) {
    margin: 4px 0;
    padding-inline-start: 20px;
    padding-inline-end: 0;
  }
  /* An item in the other language keeps its bullet beside its text. */
  .md :global(:is(ul, ol):dir(ltr) > li:dir(rtl)),
  .md :global(:is(ul, ol):dir(rtl) > li:dir(ltr)) {
    list-style-position: inside;
  }
  .md :global(li) {
    margin: 2px 0;
    text-align: start;
  }
  .md :global(p),
  .md :global(h4) {
    text-align: start;
  }
  .md :global(h4) {
    margin: 8px 0 2px;
    font-size: 13px;
  }
  .md :global(code) {
    font-family: var(--font-mono);
    font-size: 12px;
  }
</style>
