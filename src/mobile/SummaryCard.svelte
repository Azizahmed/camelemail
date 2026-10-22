<script lang="ts">
  // The AI summary of a conversation, shown at the top of the thread on the
  // phone. It runs once when it appears (only ever after a tap) and streams in;
  // the text goes to the AI provider then and at "Summarize again", never on its own.
  import { RefreshCw, Sparkles, Square, X } from "@lucide/svelte";
  import { onDestroy, onMount } from "svelte";
  import { api, errorText, newRequestId } from "../lib/api";
  import { textDirection } from "../lib/bidi";
  import { t, type MessageKey } from "../lib/i18n.svelte";
  import { openLinks, renderMarkdown } from "../lib/markdown";

  let { accountId, threadId, onclose }: { accountId: string; threadId: string; onclose: () => void } = $props();

  let text = $state("");
  let running = $state(false);
  let error = $state<{ key: MessageKey; detail: string; settings: boolean } | null>(null);
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

  /** The engine's message, in words a phone user can act on. */
  function friendly(msg: string): { key: MessageKey; detail: string; settings: boolean } {
    const m = msg.toLowerCase();
    if (/^(set the ai provider|choose a model|add your api key)/.test(m)) return { key: "m.toast.aiFinish", detail: "", settings: true };
    if (m.includes("rejected the api key")) return { key: "m.summary.key", detail: msg, settings: true };
    if (m.includes("rate limiting") || /\b(429|503|529)\b/.test(m)) return { key: "m.summary.busy", detail: msg, settings: false };
    if (/error sending request|connect|timed out|timeout|dns|network|unreachable|closed the connection/.test(m)) return { key: "m.summary.offline", detail: msg, settings: false };
    return { key: "m.summary.failed", detail: msg, settings: false };
  }

  async function run() {
    if (running) return;
    text = "";
    streamed = "";
    error = null;
    running = true;
    requestId = newRequestId();
    try {
      const out = await api.aiRun(requestId, { type: "summarize", accountId, threadId }, onDelta);
      cancelAnimationFrame(frame);
      frame = 0;
      text = out.text;
      if (!text.trim()) error = { key: "m.summary.empty", detail: "", settings: false };
    } catch (e) {
      cancelAnimationFrame(frame);
      frame = 0;
      const msg = errorText(e);
      if (msg !== "Stopped") error = friendly(msg);
      else text = streamed;
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

<section class="card" aria-live="polite" aria-busy={running}>
  <div class="head">
    <Sparkles size={15} strokeWidth={1.8} />
    <span class="title">{t("m.summary.title")}</span>
    <span class="grow"></span>
    {#if running}
      <button class="m-icon-btn small" aria-label={t("m.summary.stop")} onclick={stop}><Square size={14} strokeWidth={1.8} /></button>
    {:else if !error}
      <button class="m-icon-btn small" aria-label={t("m.summary.again")} onclick={run}><RefreshCw size={15} strokeWidth={1.8} /></button>
    {/if}
    <button class="m-icon-btn small" aria-label={t("m.thread.hideSummary")} onclick={onclose}><X size={17} strokeWidth={1.8} /></button>
  </div>
  {#if error}
    <p class="error">{t(error.key)}</p>
    {#if error.detail}<p class="detail selectable" dir="auto">{error.detail}</p>{/if}
    <div class="actions">
      <button class="m-primary soft" onclick={run}>{t("common.retry")}</button>
      {#if error.settings}
        <button class="m-primary soft" onclick={() => void api.openSettings("ai")}>{t("m.toast.openSettings")}</button>
      {/if}
    </div>
  {:else if !text && running}
    <div class="thinking"><span class="spinner"></span> {t("m.summary.reading")}</div>
  {:else}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <!-- The card follows the summary's language; each paragraph and list then takes its own direction. -->
    <div class="md selectable" dir={textDirection(text) ?? "auto"} onclick={openLinks}>{@html renderMarkdown(text)}</div>
  {/if}
</section>

<style>
  .card {
    margin: 0 0 10px;
    padding: 6px 0 12px;
    padding-inline: 14px 6px;
    border-radius: 16px;
    background: linear-gradient(135deg, var(--m-accent-soft), transparent 75%), var(--m-surface);
    color: var(--m-text);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 7px;
    color: var(--m-accent);
    font-size: 13px;
    font-weight: 600;
  }
  .title {
    line-height: 1.6;
  }
  .grow {
    flex: 1;
  }
  .small {
    width: 34px;
    height: 34px;
    color: var(--m-text-2);
  }
  .thinking {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 4px 0 2px;
    color: var(--m-text-2);
    font-size: 13px;
  }
  .error {
    margin: 2px 0 0;
    margin-inline-end: 8px;
    font-size: 14px;
    line-height: 1.5;
  }
  .detail {
    margin: 4px 0 0;
    margin-inline-end: 8px;
    color: var(--m-text-3);
    font-size: 11.5px;
    line-height: 1.5;
    overflow-wrap: anywhere;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 10px;
  }
  .actions .m-primary {
    height: 34px;
    padding: 0 16px;
    font-size: 13px;
  }
  .md {
    padding-inline-end: 8px;
    font-size: 14px;
    line-height: 1.6;
    overflow-wrap: anywhere;
  }
  .md :global(p) {
    margin: 4px 0;
    text-align: start;
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
    margin: 3px 0;
    text-align: start;
  }
  .md :global(h4) {
    margin: 8px 0 2px;
    font-size: 14px;
    text-align: start;
  }
  .md :global(code) {
    font-family: var(--font-mono);
    font-size: 12.5px;
  }
</style>
