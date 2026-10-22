<script lang="ts">
  // A conversation on the phone: the messages, actions in the top bar and a
  // "More" sheet, and reply buttons that open the full-screen composer.
  import {
    Archive,
    ArrowLeft,
    Clock3,
    FolderInput,
    Forward,
    Inbox,
    Mail,
    MailMinus,
    MoreVertical,
    Reply,
    ReplyAll,
    RotateCcw,
    ShieldAlert,
    Sparkles,
    Star,
    Tags,
    Trash2,
  } from "@lucide/svelte";
  import { api, errorText } from "../lib/api";
  import { subjectDir } from "../lib/bidi";
  import { wakeDate } from "../lib/format";
  import { account, act, aiAvailable, labelName, mail, ownEmailSet, reply, snooze, toggleStar, trashOrRestore, unsnooze } from "../lib/mail.svelte";
  import { BUCKET_NAMES, L, type Category, type Draft, type ReplyKind, type ThreadDetail } from "../lib/types";
  import { toast, toastError } from "../lib/ui.svelte";
  import { t, type MessageKey } from "../lib/i18n.svelte";
  import AiReply from "../components/thread/AiReply.svelte";
  import MessageCard from "../components/thread/MessageCard.svelte";
  import SummaryCard from "../components/thread/SummaryCard.svelte";
  import MoveSheet from "./MoveSheet.svelte";
  import { back } from "./nav.svelte";
  import Sheet from "./Sheet.svelte";
  import SnoozeSheet from "./SnoozeSheet.svelte";


  // Captured on open: the list may move `mail.open` on while this screen leaves.
  const ref = $state.snapshot(mail.open)!;

  let detail = $state<ThreadDetail | null>(null);
  let error = $state("");
  let expanded = $state<Record<string, boolean>>({});
  let showEarlier = $state(false);
  let showSummary = $state(false);
  let aiReply = $state<ReplyKind | null>(null);
  let moreOpen = $state(false);
  let sortOpen = $state(false);
  let snoozeOpen = $state(false);
  let moveOpen = $state(false);
  let more: Sheet | undefined = $state();
  let scroller: HTMLDivElement | undefined = $state();

  const thread = $derived(mail.threads.find((t) => t.accountId === ref.accountId && t.threadId === ref.threadId));
  const inInbox = $derived(detail?.labels.includes(L.INBOX) ?? false);
  const inTrash = $derived((detail?.labels.length ?? 0) > 0 && detail!.labels.every((l) => l === L.TRASH || l === L.STARRED));
  const inSpam = $derived(detail?.labels.includes(L.SPAM) ?? false);
  const starred = $derived(detail?.labels.includes(L.STARRED) ?? false);
  const unsubMessage = $derived(detail?.messages.findLast((m) => m.unsubscribe)?.id ?? null);
  const ownEmails = $derived(ownEmailSet());
  const sender = $derived(detail?.messages.findLast((m) => !ownEmails.has(m.from.email.toLowerCase()))?.from ?? null);
  const replyAllDefault = $derived(mail.settings?.defaultReply === "replyAll");

  let first = true;
  async function load(markRead: boolean) {
    try {
      const d = await api.getThread(ref.accountId, ref.threadId);
      const lastIdx = d.messages.length - 1;
      if (first) {
        first = false;
        const next: Record<string, boolean> = {};
        d.messages.forEach((m, i) => (next[m.id] = m.isUnread || i === lastIdx || m.isDraft));
        expanded = next;
        queueMicrotask(() => {
          const firstUnread = d.messages.findIndex((m) => m.isUnread);
          if (firstUnread > 0) scroller?.querySelector(`[data-msg="${firstUnread}"]`)?.scrollIntoView({ block: "start" });
        });
      } else {
        d.messages.forEach((m, i) => {
          if (expanded[m.id] === undefined) expanded[m.id] = m.isUnread || i === lastIdx;
        });
      }
      detail = d;
      error = "";
      if (markRead && mail.settings?.markReadOnOpen !== false && d.messages.some((m) => m.isUnread)) {
        await api.modifyThreads([ref], { type: "markRead" });
      }
    } catch (e) {
      error = errorText(e);
    }
  }

  void load(true);
  let lastVersion = mail.threadVersion;
  $effect(() => {
    if (mail.threadVersion !== lastVersion) {
      lastVersion = mail.threadVersion;
      void load(false);
    }
  });

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

  function startReply(kind: ReplyKind) {
    const target = replyTarget();
    if (target) void reply(target.id, kind, true);
  }

  function startAiReply() {
    if (!aiAvailable()) return;
    aiReply = replyAllDefault ? "replyAll" : "reply";
    queueMicrotask(() => scroller?.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" }));
  }

  async function aiReplyDone(d: Draft) {
    aiReply = null;
    try {
      await api.openCompose(d.id);
    } catch (e) {
      toastError(e);
    }
  }

  /** Act on this thread, then go back to the list when it leaves the view. */
  async function leave(fn: () => Promise<unknown>) {
    await back();
    await fn();
  }

  async function fromMore(fn: () => unknown) {
    await more?.close();
    await fn();
  }

  async function doUnsubscribe() {
    if (!unsubMessage) return;
    try {
      const r = await api.unsubscribe(unsubMessage);
      const msg = { "one-click": t("m.thread.unsubDone"), url: t("m.thread.unsubPage"), email: t("m.thread.unsubSent"), none: r.error ?? t("m.thread.unsubNone") }[r.method];
      toast(msg, { kind: r.method === "none" ? "error" : "success" });
    } catch (e) {
      toastError(e, t("m.thread.unsubFailed"));
    }
  }

  async function sortSender(category: Category) {
    if (!sender) return;
    try {
      await api.setSenderCategory(sender.email, category);
      toast(t("m.thread.sorted", { email: sender.email, bucket: t(`m.bucket.${category}` as MessageKey) }));
    } catch (e) {
      toastError(e);
    }
  }
</script>

<div class="m-screen">
  <header class="m-topbar">
    <button class="m-icon-btn" aria-label={t("common.back")} onclick={() => back()}><ArrowLeft size={20} strokeWidth={1.6} /></button>
    <span class="grow"></span>
    {#if detail}
      {#if inTrash}
        <button class="m-icon-btn" aria-label={t("m.thread.restoreInbox")} onclick={() => leave(() => act({ type: "moveToInbox", from: L.TRASH }, [ref]))}><RotateCcw size={18} strokeWidth={1.6} /></button>
      {:else if inInbox}
        <button class="m-icon-btn" aria-label={t("m.home.archive")} onclick={() => leave(() => act({ type: "archive" }, [ref]))}><Archive size={18} strokeWidth={1.6} /></button>
      {:else}
        <button class="m-icon-btn" aria-label={t("m.thread.moveInbox")} onclick={() => leave(() => act({ type: "moveToInbox", from: null }, [ref]))}><Inbox size={18} strokeWidth={1.6} /></button>
      {/if}
      <button class="m-icon-btn" aria-label={inTrash ? t("m.home.deleteForever") : t("m.home.delete")} onclick={() => leave(() => (inTrash ? act({ type: "deleteForever" }, [ref]) : trashOrRestore([ref])))}><Trash2 size={18} strokeWidth={1.6} /></button>
      <button class="m-icon-btn" aria-label={t("m.home.markUnread")} onclick={() => leave(() => act({ type: "markUnread" }, [ref]))}><Mail size={18} strokeWidth={1.6} /></button>
      <button class="m-icon-btn" class:starred aria-label={starred ? t("m.home.unstar") : t("mset.action.star")} onclick={() => toggleStar([ref])}><Star size={18} strokeWidth={1.6} fill={starred ? "currentColor" : "none"} /></button>
      <button class="m-icon-btn" aria-label={t("m.home.more")} onclick={() => (moreOpen = true)}><MoreVertical size={18} strokeWidth={1.6} /></button>
    {/if}
  </header>

  <div class="m-scroll body" bind:this={scroller}>
    {#if error}
      <div class="m-empty">{error}</div>
    {:else if !detail}
      <div class="m-empty"><span class="spinner"></span></div>
    {:else}
      <h1 class="subject selectable" dir={subjectDir(detail.subject || thread?.subject || "")}>{detail.subject || thread?.subject || t("m.row.noSubject")}</h1>
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
        <div class="summary"><SummaryCard accountId={detail.accountId} threadId={detail.threadId} onclose={() => (showSummary = false)} /></div>
      {/if}

      {#if detail.snoozedUntil}
        <div class="snoozed">
          <Clock3 size={13} /> {t("m.thread.snoozedUntil", { when: wakeDate(detail.snoozedUntil) })}
          <button class="link" onclick={() => unsnooze([ref])}>{t("m.thread.restoreInbox")}</button>
        </div>
      {/if}

      <div class="messages">
        {#each detail.messages as m, i (m.id)}
          {#if i < hiddenCount}
            {#if i === 0}
              <button class="earlier" onclick={() => (showEarlier = true)}>{t("m.thread.showEarlier", { n: hiddenCount })}</button>
            {/if}
          {:else}
            <div data-msg={i}><MessageCard message={m} bind:expanded={expanded[m.id]} /></div>
          {/if}
        {/each}
      </div>

      {#if aiReply && replyTarget()}
        <div class="ai"><AiReply messageId={replyTarget()!.id} kind={aiReply} ondone={aiReplyDone} oncancel={() => (aiReply = null)} /></div>
      {/if}
      <div class="tail"></div>
    {/if}
  </div>

  {#if detail && !aiReply}
    <footer class="replies">
      {#if replyAllDefault}
        <button class="reply" onclick={() => startReply("replyAll")}><ReplyAll size={16} strokeWidth={1.6} /> {t("m.thread.replyAll")}</button>
        <button class="reply" onclick={() => startReply("reply")}><Reply size={16} strokeWidth={1.6} /> {t("m.thread.reply")}</button>
      {:else}
        <button class="reply" onclick={() => startReply("reply")}><Reply size={16} strokeWidth={1.6} /> {t("m.thread.reply")}</button>
        <button class="reply" onclick={() => startReply("replyAll")}><ReplyAll size={16} strokeWidth={1.6} /> {t("m.thread.replyAll")}</button>
      {/if}
      <button class="reply" aria-label={t("m.thread.forward")} onclick={() => startReply("forward")}><Forward size={16} strokeWidth={1.6} /></button>
      {#if mail.ai.enabled}
        <button class="reply ai-btn" aria-label={t("m.thread.aiAssist")} onclick={startAiReply}><Sparkles size={16} strokeWidth={1.6} /></button>
      {/if}
    </footer>
  {/if}
</div>

<Sheet bind:this={more} bind:open={moreOpen}>
  <button class="m-sheet-item" onclick={() => fromMore(() => (snoozeOpen = true))}><Clock3 size={18} strokeWidth={1.6} /> {t("mset.action.snooze")}</button>
  <button class="m-sheet-item" onclick={() => fromMore(() => (moveOpen = true))}><FolderInput size={18} strokeWidth={1.6} /> {t("m.home.moveTo")}</button>
  {#if mail.ai.enabled}
    <button class="m-sheet-item" onclick={() => fromMore(() => { if (showSummary || aiAvailable()) showSummary = !showSummary; })}><Sparkles size={18} strokeWidth={1.6} /> {showSummary ? t("m.thread.hideSummary") : t("m.thread.summarize")}</button>
  {/if}
  {#if unsubMessage}
    <button class="m-sheet-item" onclick={() => fromMore(doUnsubscribe)}><MailMinus size={18} strokeWidth={1.6} /> {t("m.thread.unsubscribe")}</button>
  {/if}
  {#if inSpam}
    <button class="m-sheet-item" onclick={() => fromMore(() => leave(() => act({ type: "notSpam" }, [ref])))}><Inbox size={18} strokeWidth={1.6} /> {t("m.home.notSpam")}</button>
  {:else}
    <button class="m-sheet-item" onclick={() => fromMore(() => leave(() => act({ type: "spam" }, [ref])))}><ShieldAlert size={18} strokeWidth={1.6} /> {t("m.home.markSpam")}</button>
  {/if}
  {#if sender && mail.settings?.smartInbox}
    <button class="m-sheet-item" onclick={() => fromMore(() => (sortOpen = true))}><Tags size={18} strokeWidth={1.6} /> {t("m.thread.sortSender")}</button>
  {/if}
</Sheet>

<Sheet bind:open={sortOpen} title={sender ? t("m.thread.sortTitle", { name: sender.name || sender.email }) : ""}>
  {#each Object.keys(BUCKET_NAMES) as id (id)}
    <button class="m-sheet-item" class:on={thread?.category === id} onclick={() => { sortOpen = false; void sortSender(id as Category); }}>{t(`m.bucket.${id}` as MessageKey)}</button>
  {/each}
</Sheet>

<SnoozeSheet bind:open={snoozeOpen} onpick={(at) => leave(() => snooze(at, [ref]))} />
<MoveSheet bind:open={moveOpen} accountId={ref.accountId} current={mail.page === "list" ? mail.view.label : null} onpick={(to) => leave(() => act({ type: "move", from: mail.page === "list" ? mail.view.label : null, to }, [ref]))} />

<style>
  .grow {
    flex: 1;
  }
  .starred {
    color: var(--m-swipe-star);
  }
  .body {
    padding: 0 13px;
    -webkit-user-select: text;
    user-select: text;
  }
  .subject {
    margin: 5px 5px 8px;
    font-size: 20px;
    font-weight: 500;
    line-height: 25px;
    overflow-wrap: anywhere;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    margin: 0 5px 11px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 2px 8px;
    border-radius: 10px;
    background: var(--m-surface-2);
    color: var(--m-text-2);
    font-size: 11px;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
  }
  .summary,
  .ai {
    margin: 0 0 10px;
  }
  .snoozed {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 7px;
    margin: 0 5px 10px;
    color: var(--m-text-2);
    font-size: 12px;
  }
  .link {
    border: 0;
    background: none;
    color: var(--m-accent);
    font: inherit;
    padding: 0;
  }
  .messages {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .earlier {
    padding: 10px;
    border: 1px dashed var(--m-separator);
    border-radius: 11px;
    background: transparent;
    color: var(--m-text-2);
    font: inherit;
    font-size: 12px;
  }
  .tail {
    height: 82px;
  }
  .replies {
    position: fixed;
    left: 13px;
    right: 13px;
    bottom: 13px;
    display: flex;
    gap: 7px;
    padding: 5px;
    border-radius: 26px;
    background: var(--m-surface);
    box-shadow: var(--m-shadow);
  }
  .reply {
    flex: 1;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 7px;
    min-width: 39px;
    height: 39px;
    border: 0;
    border-radius: 20px;
    background: transparent;
    color: var(--m-text);
    font: inherit;
    font-size: 13px;
  }
  .reply[aria-label] {
    flex: none;
    width: 43px;
  }
  .reply:active {
    background: var(--m-pill);
  }
  .ai-btn {
    color: var(--m-accent);
  }
</style>
