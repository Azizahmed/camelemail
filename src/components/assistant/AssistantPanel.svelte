<script lang="ts">
  import { ArrowUp, Check, CircleAlert, Mail, Plug, Plus, Settings2, Sparkles, Square, Wrench, X } from "@lucide/svelte";
  import { onDestroy, tick, untrack } from "svelte";
  import { api } from "../../lib/api";
  import { approve, ask, chat, newChat, stop } from "../../lib/assistant.svelte";
  import { mail } from "../../lib/mail.svelte";
  import { openLinks, renderMarkdown } from "../../lib/markdown";
  import { titled } from "../../lib/shortcuts.svelte";

  let input = $state("");
  let useContext = $state(true);
  let scroller: HTMLDivElement | undefined = $state();
  let box: HTMLTextAreaElement | undefined = $state();

  const openThread = $derived(mail.open ? mail.threads.find((t) => t.accountId === mail.open!.accountId && t.threadId === mail.open!.threadId) : undefined);
  const contextLabel = $derived(mail.open ? (openThread?.subject || "This email") : null);

  const SUGGESTIONS = $derived(
    mail.open && useContext
      ? ["Summarize this email", "Draft a reply that accepts", "What do they want from me?", "Find related emails"]
      : ["What needs a reply from me?", "Summarize today's unread emails", "Find invoices from last month", "Draft an email to my team"],
  );

  // The answer being streamed is re-rendered (and its Markdown parsed) at most once per frame.
  type Item = (typeof chat.items)[number];
  let live = $state.raw<{ item: Item; text: string } | null>(null);
  let frame = 0;
  const lastAnswer = () => {
    const last = chat.items[chat.items.length - 1];
    return last?.kind === "assistant" ? last : null;
  };
  $effect(() => {
    void lastAnswer()?.text;
    if (frame) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      const a = untrack(lastAnswer);
      live = a ? { item: a, text: a.text } : null;
    });
  });
  onDestroy(() => cancelAnimationFrame(frame));

  /** Follow new output only while the user is at the bottom (not reading further up). */
  let stick = true;
  function onScroll() {
    if (scroller) stick = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 40;
  }
  $effect(() => {
    void chat.items.length;
    void live;
    void tick().then(() => {
      if (stick) scroller?.scrollTo({ top: scroller.scrollHeight });
    });
  });

  $effect(() => {
    box?.focus();
  });

  async function send(text = input) {
    if (!text.trim() || chat.running) return;
    input = "";
    resize();
    stick = true;
    const ctx = useContext && mail.open ? mail.open : null;
    await ask(text, ctx, ctx ? contextLabel : null);
  }

  function resize() {
    if (!box) return;
    box.style.height = "auto";
    box.style.height = Math.min(box.scrollHeight, 160) + "px";
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void send();
    } else if (e.key === "Escape") {
      e.stopPropagation();
      if (chat.running) stop();
      else mail.assistantOpen = false;
    }
  }

  function preview(input: unknown): { label: string; value: string }[] {
    if (!input || typeof input !== "object") return [];
    return Object.entries(input as Record<string, unknown>)
      .filter(([, v]) => v !== null && v !== undefined && v !== "")
      .map(([k, v]) => ({
        label: k.replace(/_/g, " "),
        value: Array.isArray(v) ? v.map((x) => (typeof x === "object" ? JSON.stringify(x) : String(x))).join(", ") : typeof v === "object" ? JSON.stringify(v) : String(v),
      }));
  }
</script>

<aside class="assistant" aria-label="AI assistant">
  <header>
    <Sparkles size={16} />
    <span class="title">Assistant</span>
    <span class="grow"></span>
    <button class="icon-btn" title="New chat" onclick={newChat} disabled={chat.items.length === 0}><Plus size={15} /></button>
    <button class="icon-btn" title="Assistant settings" onclick={() => api.openSettings("ai")}><Settings2 size={15} /></button>
    <button class="icon-btn" title={titled("Close", "assistant")} onclick={() => (mail.assistantOpen = false)}><X size={15} /></button>
  </header>

  <div class="scroll" bind:this={scroller} onscroll={onScroll}>
    {#if chat.items.length === 0}
      <div class="empty">
        <img src="/brand/remail-mark.png" alt="" />
        <p>Ask about your email, or have the assistant find, summarize and draft messages for you.</p>
        <div class="suggestions">
          {#each SUGGESTIONS as s (s)}
            <button class="suggestion" onclick={() => send(s)}>{s}</button>
          {/each}
        </div>
      </div>
    {/if}

    {#each chat.items as item, i (i)}
      {#if item.kind === "user"}
        <div class="user">
          {#if item.context}<div class="ctx"><Mail size={11} /> <bdi>{item.context}</bdi></div>{/if}
          <div class="bubble selectable" dir="auto">{item.text}</div>
        </div>
      {:else if item.kind === "assistant"}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="answer md selectable" onclick={openLinks}>{@html renderMarkdown(live?.item === item ? live.text : item.text)}</div>
      {:else if item.kind === "tool"}
        <div class="tool" class:approval={item.status === "approval"}>
          <div class="tool-line">
            {#if item.status === "running"}<span class="spinner small"></span>
            {:else if item.status === "ok"}<Check size={13} class="ok" />
            {:else if item.status === "approval"}{#if item.server}<Plug size={13} />{:else}<Wrench size={13} />{/if}
            {:else}<CircleAlert size={13} class="bad" />{/if}
            <span class="tool-title">{item.title}{#if item.server}<span class="server"> · {item.server}</span>{/if}</span>
            {#if item.summary && item.status !== "approval"}<span class="tool-sum">{item.status === "denied" ? "declined" : item.summary}</span>{/if}
          </div>
          {#if item.status === "approval"}
            <div class="args selectable">
              {#each preview(item.input) as row (row.label)}
                <div class="arg"><span class="k">{row.label}</span><span class="v" dir="auto">{row.value}</span></div>
              {/each}
            </div>
            {#if item.reason}<div class="reason" dir="auto">{item.reason}</div>{/if}
            <div class="approve-row">
              <span class="ask">Allow the assistant to do this?</span>
              <button class="btn" onclick={() => approve(item.id, false)}>Deny</button>
              <button class="btn primary" onclick={() => approve(item.id, true)}>Allow</button>
            </div>
          {/if}
        </div>
      {:else}
        <div class="err selectable"><CircleAlert size={14} /> <span dir="auto">{item.text}</span></div>
      {/if}
    {/each}
    {#if chat.running && chat.items[chat.items.length - 1]?.kind === "user"}
      <div class="thinking"><span class="dots"><i></i><i></i><i></i></span></div>
    {/if}
  </div>

  <div class="composer">
    {#if mail.open && contextLabel}
      <button class="ctx-chip" class:off={!useContext} title={useContext ? "The assistant can see this email. Click to exclude it." : "Click to let the assistant see the open email"} onclick={() => (useContext = !useContext)}>
        <Mail size={11} /> <span dir="auto">{contextLabel}</span>
        {#if useContext}<X size={11} />{/if}
      </button>
    {/if}
    <div class="input-row">
      <textarea
        bind:this={box}
        bind:value={input}
        rows="1"
        dir="auto"
        placeholder="Ask the assistant…"
        oninput={resize}
        onkeydown={onKey}
      ></textarea>
      {#if chat.running}
        <button class="send stop" title="Stop" onclick={stop}><Square size={13} /></button>
      {:else}
        <button class="send" title="Send (Enter)" disabled={!input.trim()} onclick={() => send()}><ArrowUp size={16} /></button>
      {/if}
    </div>
  </div>
</aside>

<style>
  .assistant {
    width: clamp(320px, 30vw, 420px);
    flex: none;
    display: flex;
    flex-direction: column;
    border-left: 0.5px solid var(--separator);
    background: var(--surface-overlay);
    min-height: 0;
    animation: slide 0.18s ease;
  }
  @keyframes slide {
    from {
      transform: translateX(24px);
      opacity: 0;
    }
  }
  header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 10px 10px 14px;
    color: var(--accent);
    border-bottom: 0.5px solid var(--separator-subtle);
  }
  .title {
    font-weight: 600;
    color: var(--content);
  }
  .grow {
    flex: 1;
  }
  .scroll {
    flex: 1;
    overflow-y: auto;
    padding: 14px 14px 8px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .empty {
    margin: auto 0;
    text-align: center;
    color: var(--content-subtle);
    padding: 20px 6px;
  }
  .empty img {
    width: 44px;
    opacity: 0.9;
  }
  .empty p {
    font-size: 13px;
    line-height: 19px;
    margin: 10px 0 16px;
  }
  .suggestions {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .suggestion {
    text-align: left;
    border: 0.5px solid var(--separator);
    background: var(--surface-elevated);
    border-radius: 10px;
    padding: 8px 12px;
    font-size: 12.5px;
    color: var(--content);
  }
  .suggestion:hover {
    border-color: var(--accent);
    background: var(--accent-soft);
  }
  .user {
    align-self: flex-end;
    max-width: 88%;
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 3px;
  }
  .ctx {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    color: var(--content-subtle);
    max-width: 100%;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .bubble {
    background: var(--accent-soft);
    border-radius: 14px 14px 4px 14px;
    padding: 8px 12px;
    font-size: 13px;
    line-height: 1.45;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .answer {
    font-size: 13px;
    line-height: 1.5;
    overflow-wrap: anywhere;
  }
  .md :global(p) {
    margin: 0 0 8px;
  }
  .md :global(p:last-child) {
    margin-bottom: 0;
  }
  .md :global(ul),
  .md :global(ol) {
    margin: 4px 0 8px;
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
  }
  .md :global(h4) {
    margin: 10px 0 4px;
    font-size: 13px;
  }
  .md :global(pre) {
    background: var(--surface-hover);
    border-radius: 8px;
    padding: 8px 10px;
    overflow-x: auto;
    font-size: 12px;
  }
  .md :global(code) {
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .tool {
    border-radius: 10px;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .tool.approval {
    border: 1px solid var(--accent);
    background: var(--surface-elevated);
    padding: 10px 12px;
    color: var(--content);
  }
  .tool-line {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
  }
  .tool-line :global(.ok) {
    color: var(--indicator-success);
  }
  .tool-line :global(.bad) {
    color: var(--indicator-error);
  }
  .tool-title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
  }
  .server {
    font-weight: 400;
    color: var(--content-muted);
  }
  .tool-sum {
    margin-left: auto;
    flex: none;
    color: var(--content-muted);
    max-width: 45%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .spinner.small {
    width: 12px;
    height: 12px;
    border-width: 1.5px;
  }
  .args {
    margin: 8px 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-height: 220px;
    overflow-y: auto;
  }
  .arg {
    display: grid;
    grid-template-columns: 70px 1fr;
    gap: 8px;
    font-size: 12px;
  }
  .k {
    color: var(--content-subtle);
    text-transform: capitalize;
  }
  .v {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .approve-row {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .ask {
    flex: 1;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .reason {
    margin-top: 6px;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .err {
    display: flex;
    gap: 6px;
    align-items: flex-start;
    font-size: 12.5px;
    color: var(--indicator-error);
    background: color-mix(in srgb, var(--indicator-error) 8%, transparent);
    border-radius: 8px;
    padding: 8px 10px;
  }
  .thinking {
    padding: 4px 2px;
  }
  .dots {
    display: inline-flex;
    gap: 4px;
  }
  .dots i {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
    animation: blink 1.2s infinite ease-in-out;
  }
  .dots i:nth-child(2) {
    animation-delay: 0.15s;
  }
  .dots i:nth-child(3) {
    animation-delay: 0.3s;
  }
  @keyframes blink {
    0%,
    80%,
    100% {
      opacity: 0.25;
    }
    40% {
      opacity: 1;
    }
  }
  .composer {
    padding: 8px 12px 12px;
    border-top: 0.5px solid var(--separator-subtle);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .ctx-chip {
    align-self: flex-start;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    max-width: 100%;
    font-size: 11.5px;
    border: none;
    border-radius: 999px;
    padding: 3px 9px;
    background: var(--accent-soft);
    color: var(--accent);
  }
  .ctx-chip span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ctx-chip.off {
    background: var(--surface-hover);
    color: var(--content-muted);
    text-decoration: line-through;
  }
  .input-row {
    display: flex;
    align-items: flex-end;
    gap: 8px;
    border: 0.5px solid var(--separator);
    border-radius: 14px;
    padding: 6px 6px 6px 12px;
    background: var(--surface);
  }
  .input-row:focus-within {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  textarea {
    flex: 1;
    border: none;
    outline: none;
    resize: none;
    background: none;
    font-size: 13px;
    line-height: 19px;
    padding: 4px 0;
    max-height: 160px;
    user-select: text;
  }
  .send {
    width: 30px;
    height: 30px;
    flex: none;
    border-radius: 50%;
    border: none;
    background: var(--accent);
    color: #fff;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .send:disabled {
    opacity: 0.35;
  }
  .send.stop {
    background: var(--content);
    color: var(--surface);
  }
</style>
