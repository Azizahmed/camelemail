<script lang="ts">
  import {
    Archive,
    ArrowLeft,
    ChevronDown,
    ChevronUp,
    Clock3,
    FolderInput,
    Forward,
    Inbox,
    MailMinus,
    MailOpen,
    MoreHorizontal,
    Reply,
    ReplyAll,
    RotateCcw,
    ShieldAlert,
    Sparkles,
    Star,
    Trash2,
    X,
  } from "@lucide/svelte";
  import { api, errorText } from "../../lib/api";
  import { subjectDir } from "../../lib/bidi";
  import { wakeDate } from "../../lib/format";
  import { BUCKET_NAMES, L, type Category, type Draft, type ReplyKind, type ThreadDetail } from "../../lib/types";
  import {
    account,
    act,
    aiAvailable,
    closeThread,
    labelName,
    mail,
    reply,
    snooze,
    stepThread,
    toggleStar,
    trashOrRestore,
    unsnooze,
    ownEmailSet,
  } from "../../lib/mail.svelte";
  import { titled } from "../../lib/shortcuts.svelte";
  import { toast, toastError } from "../../lib/ui.svelte";
  import Composer from "../compose/Composer.svelte";
  import MoveMenu from "../menus/MoveMenu.svelte";
  import Popover from "../menus/Popover.svelte";
  import SnoozeMenu from "../menus/SnoozeMenu.svelte";
  import AiReply from "./AiReply.svelte";
  import MessageCard from "./MessageCard.svelte";
  import SummaryCard from "./SummaryCard.svelte";

  /** `overlay` covers the list; `pane` sits beside it (split layout). */
  let { layout = "overlay" }: { layout?: "overlay" | "pane" } = $props();

  let detail = $state<ThreadDetail | null>(null);
  let error = $state("");
  let expanded = $state<Record<string, boolean>>({});
  let showEarlier = $state(false);
  let menu = $state<null | { kind: "move" | "snooze" | "more"; anchor: DOMRect }>(null);
  let showSummary = $state(false);
  let aiReply = $state<ReplyKind | null>(null);
  let scroller: HTMLDivElement | undefined = $state();
  let loadedFor = "";

  const ref = $derived(mail.open!);
  /** Which thread is open: a new `mail.open` object for the same thread is not a new thread. */
  const openKey = $derived(mail.open ? `${mail.open.accountId}\u0000${mail.open.threadId}` : "");
  const thread = $derived(mail.threads.find((t) => t.accountId === ref.accountId && t.threadId === ref.threadId));
  const inInbox = $derived(detail?.labels.includes(L.INBOX) ?? false);
  const inTrash = $derived((detail?.labels.length ?? 0) > 0 && detail!.labels.every((l) => l === L.TRASH || l === L.STARRED));
  const starred = $derived(detail?.labels.includes(L.STARRED) ?? false);
  const unsub = $derived(detail?.messages.findLast((m) => m.unsubscribe)?.unsubscribe ?? null);
  const unsubMessage = $derived(detail?.messages.findLast((m) => m.unsubscribe)?.id ?? null);
  const ownEmails = $derived(ownEmailSet());
  /** Latest sender who isn't the user (Smart Inbox rules are per sender). */
  const sender = $derived(detail?.messages.findLast((m) => !ownEmails.has(m.from.email.toLowerCase()))?.from ?? null);

  async function load(markRead: boolean) {
    const r = mail.open;
    const key = openKey;
    if (!r) return;
    try {
      const d = await api.getThread(r.accountId, r.threadId);
      if (openKey !== key) return;
      const first = loadedFor !== key;
      const lastIdx = d.messages.length - 1;
      if (first) {
        loadedFor = key;
        showEarlier = false;
        const next: Record<string, boolean> = {};
        d.messages.forEach((m, i) => (next[m.id] = m.isUnread || i === lastIdx || m.isDraft));
        expanded = next;
      } else {
        // Messages that arrived since the thread was opened need an entry:
        // binding a fallback-valued prop to `undefined` is a runtime error.
        d.messages.forEach((m, i) => {
          if (expanded[m.id] === undefined) expanded[m.id] = m.isUnread || i === lastIdx;
        });
      }
      detail = d;
      error = "";
      if (first) {
        queueMicrotask(() => {
          const firstUnread = d.messages.findIndex((m) => m.isUnread);
          const el = scroller?.querySelector(`[data-msg="${firstUnread >= 0 ? firstUnread : lastIdx}"]`);
          if (firstUnread >= 0) el?.scrollIntoView({ block: "start" });
          else scroller?.scrollTo({ top: scroller.scrollHeight });
        });
      }
      if (markRead && mail.settings?.markReadOnOpen !== false && d.messages.some((m) => m.isUnread)) {
        await api.modifyThreads([r], { type: "markRead" });
      }
    } catch (e) {
      error = errorText(e);
    }
  }

  // Load when the open thread changes; reload (without marking read) on mailbox updates.
  $effect(() => {
    void openKey;
    detail = null;
    loadedFor = "";
    showSummary = false;
    aiReply = null;
    void load(true);
  });
  let lastVersion = mail.threadVersion;
  $effect(() => {
    if (mail.threadVersion !== lastVersion) {
      lastVersion = mail.threadVersion;
      void load(false);
    }
  });

  /** Long threads show the last 3 messages (or everything from the first unread);
   *  the rest fold into one divider when at least 2 would be hidden. */
  const hiddenCount = $derived.by(() => {
    if (!detail || showEarlier) return 0;
    const n = detail.messages.length;
    const firstUnread = detail.messages.findIndex((m) => m.isUnread);
    let cut = n - 3;
    if (firstUnread >= 0) cut = Math.min(cut, firstUnread);
    return cut >= 2 ? cut : 0;
  });

  function replyTarget() {
    if (!detail) return null;
    return detail.messages.findLast((m) => !m.isDraft) ?? detail.messages[detail.messages.length - 1];
  }

  function startAiReply(kind: ReplyKind) {
    if (!aiAvailable()) return;
    aiReply = kind;
    queueMicrotask(() => scroller?.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" }));
  }

  function aiReplyDone(d: Draft) {
    aiReply = null;
    mail.inlineDraft = d;
    queueMicrotask(() => scroller?.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" }));
  }

  function summarize() {
    if (!showSummary && !aiAvailable()) return;
    showSummary = !showSummary;
  }

  async function sortSender(category: Category) {
    menu = null;
    if (!sender) return;
    try {
      await api.setSenderCategory(sender.email, category);
      toast(`Emails from ${sender.email} will go to ${BUCKET_NAMES[category]}`);
    } catch (e) {
      toastError(e);
    }
  }

  function startReply(kind: "reply" | "replyAll" | "forward") {
    const target = replyTarget();
    if (!target) return;
    void reply(target.id, kind);
    queueMicrotask(() => scroller?.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" }));
  }

  export function shortcut(k: "reply" | "replyAll" | "forward" | "move" | "snooze") {
    if (k === "move" || k === "snooze") {
      const btn = document.querySelector(`[data-tool="${k}"]`) as HTMLElement | null;
      if (btn) menu = { kind: k, anchor: btn.getBoundingClientRect() };
      return;
    }
    startReply(k);
  }

  async function doUnsubscribe() {
    menu = null;
    if (!unsubMessage) return;
    try {
      const r = await api.unsubscribe(unsubMessage);
      const msg = { "one-click": "You're unsubscribed", url: "The unsubscribe page is open in your browser", email: "Unsubscribe request sent", none: r.error ?? "No unsubscribe method available" }[r.method];
      toast(msg, { kind: r.method === "none" ? "error" : "success" });
    } catch (e) {
      toastError(e, "Could not unsubscribe");
    }
  }

  function inlineClosed() {
    mail.inlineDraft = null;
    void load(false);
  }

  async function popOut(d: Draft) {
    mail.inlineDraft = null;
    try {
      await api.saveDraft(d, false);
      await api.openCompose(d.id);
    } catch (e) {
      toastError(e);
    }
  }

  const replyAllDefault = $derived(mail.settings?.defaultReply === "replyAll");
</script>

<div class="overlay-view" class:pane={layout === "pane"} bind:this={scroller}>
  <div class="frame">
    <button class="back" title={layout === "pane" ? "Close (Esc)" : "Back (Esc)"} onclick={closeThread}>
      {#if layout === "pane"}<X size={16} />{:else}<ArrowLeft size={16} />{/if}
    </button>
    <div class="main">
      <div class="top-actions">
        {#if inTrash}
          <button class="icon-btn restore" title="Restore to Inbox" onclick={() => act({ type: "moveToInbox", from: L.TRASH }, [ref])}><RotateCcw size={16} /></button>
        {/if}
        <button class="icon-btn" class:starred title={titled(starred ? "Unstar" : "Star", "star")} onclick={() => toggleStar([ref])}><Star size={16} /></button>
        {#if inInbox}
          <button class="icon-btn" title={titled("Archive", "archive")} onclick={() => act({ type: "archive" }, [ref])}><Archive size={16} /></button>
        {:else if !inTrash}
          <button class="icon-btn" title="Move to Inbox" onclick={() => act({ type: "moveToInbox", from: null }, [ref])}><Inbox size={16} /></button>
        {/if}
        <button class="icon-btn" title={titled("Mark as unread", "unread")} onclick={() => act({ type: "markUnread" }, [ref])}><MailOpen size={16} /></button>
        <button class="icon-btn" title={inTrash ? "Delete forever" : titled("Delete", "trash")} onclick={() => (inTrash ? act({ type: "deleteForever" }, [ref]) : trashOrRestore([ref]))}><Trash2 size={16} /></button>
        <button class="icon-btn" data-tool="move" title={titled("Move", "move")} onclick={(e) => (menu = { kind: "move", anchor: e.currentTarget.getBoundingClientRect() })}><FolderInput size={16} /></button>
        <button class="icon-btn" data-tool="snooze" title={titled("Snooze", "snooze")} onclick={(e) => (menu = { kind: "snooze", anchor: e.currentTarget.getBoundingClientRect() })}><Clock3 size={16} /></button>
        {#if mail.ai.enabled}
          <button class="icon-btn" class:ai-on={showSummary} title="Summarize with AI" onclick={summarize}><Sparkles size={16} /></button>
        {/if}
        <button class="icon-btn" title="More" onclick={(e) => (menu = { kind: "more", anchor: e.currentTarget.getBoundingClientRect() })}><MoreHorizontal size={16} /></button>
        <span class="nav">
          <button class="icon-btn" title={titled("Previous", "prev")} onclick={() => stepThread(-1)}><ChevronUp size={16} /></button>
          <button class="icon-btn" title={titled("Next", "next")} onclick={() => stepThread(1)}><ChevronDown size={16} /></button>
        </span>
      </div>

      {#if error}
        <div class="error">{error}</div>
      {:else if !detail}
        <div class="loading"><span class="spinner"></span></div>
      {:else}
        <h1 class="subject selectable" dir={subjectDir(detail.subject || thread?.subject || "")}>{detail.subject || thread?.subject || "(No subject)"}</h1>
        <div class="chips">
          {#if mail.accounts.length > 1}
            {@const acc = account(detail.accountId)}
            <span class="chip"><span class="dot" style:background={acc?.color}></span>{acc?.email}</span>
          {/if}
          {#each detail.labels.filter((l) => l !== L.STARRED) as l (l)}
            <span class="chip">{labelName(l, detail.accountId)}</span>
          {/each}
        </div>

        {#if showSummary}
          {#key detail.threadId}
            <SummaryCard accountId={detail.accountId} threadId={detail.threadId} onclose={() => (showSummary = false)} />
          {/key}
        {/if}

        {#if detail.snoozedUntil}
          <div class="snooze-banner">
            <Clock3 size={14} /> Snoozed until {wakeDate(detail.snoozedUntil)}
            <button class="link" onclick={() => unsnooze([ref])}>Restore to Inbox</button>
          </div>
        {/if}

        <div class="messages">
          {#each detail.messages as m, i (m.id)}
            {#if i < hiddenCount}
              {#if i === 0}
                <button class="earlier" onclick={() => (showEarlier = true)} aria-label="Show {hiddenCount} earlier messages">
                  <span class="knob">{hiddenCount}</span> Show {hiddenCount} earlier message{hiddenCount === 1 ? "" : "s"}
                </button>
              {/if}
            {:else}
              <div data-msg={i}>
                <MessageCard message={m} bind:expanded={expanded[m.id]} />
              </div>
            {/if}
          {/each}
        </div>

        {#if mail.inlineDraft && mail.inlineDraft.threadId === detail.threadId}
          {#key mail.inlineDraft.id}
            <Composer draft={mail.inlineDraft} variant="inline" onclose={inlineClosed} onpopout={popOut} />
          {/key}
        {:else if mail.inlineDraft && mail.inlineDraft.replyKind === "forward"}
          {#key mail.inlineDraft.id}
            <Composer draft={mail.inlineDraft} variant="inline" onclose={inlineClosed} onpopout={popOut} />
          {/key}
        {:else if aiReply && replyTarget()}
          <AiReply messageId={replyTarget()!.id} kind={aiReply} ondone={aiReplyDone} oncancel={() => (aiReply = null)} />
        {:else}
          <div class="reply-bar">
            {#if replyAllDefault}
              <button class="btn pill" onclick={() => startReply("replyAll")}><ReplyAll size={15} /> Reply all</button>
              <button class="btn pill" onclick={() => startReply("reply")}><Reply size={15} /> Reply</button>
            {:else}
              <button class="btn pill" onclick={() => startReply("reply")}><Reply size={15} /> Reply</button>
              <button class="btn pill" onclick={() => startReply("replyAll")}><ReplyAll size={15} /> Reply all</button>
            {/if}
            <button class="btn pill" onclick={() => startReply("forward")}><Forward size={15} /> Forward</button>
            {#if mail.ai.enabled}
              <button class="btn pill ai" onclick={() => startAiReply(replyAllDefault ? "replyAll" : "reply")}><Sparkles size={15} /> AI assistance</button>
            {/if}
          </div>
        {/if}
      {/if}
    </div>
  </div>
</div>

{#if menu?.kind === "snooze"}
  <SnoozeMenu anchor={menu.anchor} onclose={() => (menu = null)} onpick={(at) => { menu = null; snooze(at, [ref]); }} />
{:else if menu?.kind === "move" && detail}
  <MoveMenu
    anchor={menu.anchor}
    accountId={detail.accountId}
    current={mail.page === "list" ? mail.view.label : null}
    onclose={() => (menu = null)}
    onpick={(to) => { menu = null; act({ type: "move", from: mail.page === "list" ? mail.view.label : null, to }, [ref]); }}
  />
{:else if menu?.kind === "more"}
  <Popover anchor={menu.anchor} width={270} height={sender && mail.settings?.smartInbox ? 380 : 170} onclose={() => (menu = null)}>
    <button class="menu-item" onclick={() => { menu = null; act({ type: "spam" }, [ref]); }}><ShieldAlert size={14} /> Mark as spam</button>
    {#if unsub}
      <button class="menu-item" onclick={doUnsubscribe}><MailMinus size={14} /> Unsubscribe from this sender</button>
    {/if}
    <button class="menu-item" onclick={() => { menu = null; startReply("forward"); }}><Forward size={14} /> Forward</button>
    {#if sender && mail.settings?.smartInbox}
      <div class="menu-sep"></div>
      <div class="menu-title">Always put mail from {sender.name || sender.email} in</div>
      {#each Object.entries(BUCKET_NAMES) as [id, name] (id)}
        <button class="menu-item" class:selected={thread?.category === id} onclick={() => sortSender(id as Category)}>{name}</button>
      {/each}
    {/if}
  </Popover>
{/if}

<style>
  .overlay-view {
    position: absolute;
    inset: 0;
    background: var(--surface);
    overflow-y: auto;
    z-index: 100;
    animation: fade 0.15s ease;
    padding: 12px 20px 0;
  }
  .overlay-view.pane {
    z-index: 1;
    animation: none;
    padding: 10px 18px 0 14px;
    background: var(--surface-reading);
  }
  .pane .frame {
    gap: 12px;
  }
  .pane .top-actions {
    background: var(--surface-reading);
    justify-content: flex-start;
    flex-wrap: wrap;
    gap: 8px;
  }
  .ai-on {
    color: var(--accent) !important;
  }
  .pill.ai {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 40%, transparent);
  }
  .frame {
    display: flex;
    gap: 16px;
    max-width: 1000px;
    margin: 0 auto;
  }
  .back {
    width: 28px;
    height: 28px;
    flex: none;
    border-radius: 8px;
    background: var(--canvas-subtle);
    border: 0.5px solid var(--separator);
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--icon);
    position: sticky;
    top: 0;
  }
  .back:hover {
    color: var(--icon-hover);
  }
  .main {
    flex: 1;
    min-width: 0;
    padding-bottom: 60px;
  }
  .top-actions {
    display: flex;
    justify-content: center;
    gap: 12px;
    margin-bottom: 18px;
    position: sticky;
    top: -12px;
    background: var(--surface);
    padding: 4px 0;
    z-index: 2;
  }
  .nav {
    display: flex;
    gap: 2px;
    margin-left: 12px;
    padding-left: 12px;
    border-left: 1px solid var(--separator-subtle);
  }
  .starred {
    color: var(--identity-gold) !important;
  }
  .starred :global(svg) {
    fill: currentColor;
  }
  .restore {
    color: var(--indicator-success) !important;
  }
  .subject {
    font-size: 20px;
    line-height: 28px;
    font-weight: 600;
    margin: 0 0 6px 44px;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin: 0 0 6px 44px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 11px;
    padding: 1px 7px;
    border-radius: 5px;
    background: var(--surface-hover);
    color: var(--content-subtle);
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
  }
  .snooze-banner {
    margin: 8px 0 0 44px;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--identity-gold) 12%, transparent);
    font-size: 12px;
  }
  .link {
    border: none;
    background: none;
    color: var(--accent);
    font-weight: 500;
    margin-left: auto;
  }
  .earlier {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 0;
    border: none;
    background: none;
    border-bottom: 0.5px solid var(--separator-subtle);
    color: var(--content-subtle);
    font-size: 12px;
  }
  .knob {
    width: 32px;
    height: 32px;
    border-radius: 50%;
    border: 0.5px solid var(--separator);
    background: var(--surface-elevated);
    display: flex;
    align-items: center;
    justify-content: center;
    font-weight: 600;
    color: var(--content);
  }
  .reply-bar {
    display: flex;
    gap: 8px;
    padding: 24px 0 0 44px;
  }
  .error,
  .loading {
    padding: 20vh 0;
    text-align: center;
    color: var(--content-subtle);
    display: flex;
    justify-content: center;
  }
</style>
