<script lang="ts">
  import { Download, Forward, MoreHorizontal, Paperclip, Reply, ReplyAll, Star, FileText, ImageOff, Pencil } from "@lucide/svelte";
  import { api } from "../../lib/api";
  import { addressList, displayName, fileSize, fullDate, messageDate } from "../../lib/format";
  import type { MessageFull, ReplyKind } from "../../lib/types";
  import { account, mail, reply } from "../../lib/mail.svelte";
  import { toast, toastError } from "../../lib/ui.svelte";
  import Avatar from "../Avatar.svelte";
  import EmailFrame from "./EmailFrame.svelte";
  import Popover from "../menus/Popover.svelte";

  let { message, expanded = $bindable(false) }: { message: MessageFull; expanded?: boolean } = $props();

  let details = $state(false);
  let more = $state<DOMRect | null>(null);
  let withImages = $state<MessageFull | null>(null);
  let starred = $state(false);
  $effect(() => {
    starred = message.isStarred;
  });

  const shown = $derived(withImages ?? message);
  const own = $derived(account(message.accountId)?.email.toLowerCase() === message.from.email.toLowerCase());
  const toSummary = $derived.by(() => {
    const all = [...message.to, ...message.cc];
    const me = account(message.accountId)?.email.toLowerCase();
    const names = all.map((a) => (a.email.toLowerCase() === me ? "me" : displayName(a)));
    if (names.length === 0) return "";
    return names.length > 3 ? `${names.slice(0, 3).join(", ")} +${names.length - 3}` : names.join(", ");
  });

  async function loadImages() {
    try {
      withImages = await api.messageWithImages(message.id);
    } catch (e) {
      toastError(e);
    }
  }

  async function toggleStar() {
    starred = !starred;
    try {
      await api.setMessageFlags(message.id, { starred });
    } catch (e) {
      starred = !starred;
      toastError(e);
    }
  }

  async function markUnreadFromHere() {
    more = null;
    try {
      await api.setMessageFlags(message.id, { unread: true });
      toast("Marked as unread");
    } catch (e) {
      toastError(e);
    }
  }

  function doReply(kind: ReplyKind, pop = false) {
    more = null;
    void reply(message.id, kind, pop);
  }

  async function editDraft() {
    try {
      const d = await api.editDraftMessage(message.id);
      await api.openCompose(d.id);
    } catch (e) {
      toastError(e);
    }
  }

  async function save(index: number) {
    try {
      const path = await api.saveAttachment(message.id, index);
      if (path) toast(`Saved to ${path}`);
    } catch (e) {
      toastError(e);
    }
  }
</script>

<article class="message" class:collapsed={!expanded}>
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div class="head" role="button" tabindex="-1" onclick={() => (expanded = !expanded)}>
    <Avatar name={displayName(message.from)} email={message.from.email} size={32} />
    <div class="who">
      <div class="line1">
        <span class="name" dir="auto">{own ? "Me" : displayName(message.from)}</span>
        {#if message.isUnread}<span class="unread-dot"></span>{/if}
        {#if message.isDraft}<span class="badge draft">Draft</span>{/if}
        {#if expanded}<span class="email selectable">&lt;{message.from.email}&gt;</span>{/if}
      </div>
      <div class="line2">
        {#if expanded}
          <button class="to" onclick={(e) => { e.stopPropagation(); details = !details; }}>to <bdi>{toSummary || "undisclosed recipients"}</bdi> ▾</button>
        {:else}
          <span class="snippet" dir="auto">{message.snippet}</span>
        {/if}
      </div>
    </div>
    <div class="meta">
      {#if message.attachments.length > 0}<Paperclip size={13} />{/if}
      <span class="date" title={fullDate(message.date)}>{messageDate(message.date)}</span>
      {#if expanded}
        <span class="tools" role="toolbar" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={() => {}}>
          <button class="icon-btn" class:starred title={starred ? "Unstar" : "Star"} onclick={toggleStar}><Star size={15} /></button>
          {#if message.isDraft}
            <button class="icon-btn" title="Edit draft" onclick={editDraft}><Pencil size={15} /></button>
          {:else}
            <button class="icon-btn" title="Reply" onclick={() => doReply("reply")}><Reply size={15} /></button>
          {/if}
          <button class="icon-btn" title="More" onclick={(e) => (more = e.currentTarget.getBoundingClientRect())}><MoreHorizontal size={15} /></button>
        </span>
      {/if}
    </div>
  </div>

  {#if expanded}
    {#if details}
      <div class="details selectable">
        <div><b>From</b> <bdi>{addressList([message.from])}</bdi></div>
        {#if message.to.length}<div><b>To</b> <bdi>{addressList(message.to)}</bdi></div>{/if}
        {#if message.cc.length}<div><b>Cc</b> <bdi>{addressList(message.cc)}</bdi></div>{/if}
        {#if message.replyTo.length}<div><b>Reply-To</b> <bdi>{addressList(message.replyTo)}</bdi></div>{/if}
        <div><b>Date</b> {fullDate(message.date)}</div>
        <div><b>Subject</b> <bdi>{message.subject}</bdi></div>
      </div>
    {/if}
    {#if shown.hasRemoteImages && !mail.settings?.loadRemoteImages}
      <div class="privacy">
        <ImageOff size={14} />
        <span>Pictures from the web are blocked, so the sender can't see when you open this.</span>
        <button class="link" onclick={loadImages}>Load images</button>
      </div>
    {/if}
    <div class="body selectable">
      <EmailFrame html={shown.html} text={shown.text} allowRemote={!!withImages || !!mail.settings?.loadRemoteImages} />
    </div>
    {#if message.attachments.length > 0}
      <div class="attachments">
        {#each message.attachments as a (a.index)}
          <div class="att">
            <button class="att-open" title="Open {a.filename}" onclick={() => api.openAttachment(message.id, a.index).catch(toastError)}>
              <FileText size={16} />
              <span class="att-name">{a.filename}</span>
              <span class="att-size">{fileSize(a.size)}</span>
            </button>
            <button class="icon-btn" title="Save…" onclick={() => save(a.index)}><Download size={14} /></button>
          </div>
        {/each}
      </div>
    {/if}
  {/if}
</article>

{#if more}
  <Popover anchor={more} width={220} height={190} onclose={() => (more = null)}>
    <button class="menu-item" onclick={() => doReply("reply")}><Reply size={14} /> Reply</button>
    <button class="menu-item" onclick={() => doReply("replyAll")}><ReplyAll size={14} /> Reply all</button>
    <button class="menu-item" onclick={() => doReply("forward")}><Forward size={14} /> Forward</button>
    <button class="menu-item" onclick={() => doReply("reply", true)}><Reply size={14} /> Reply in new window</button>
    <div class="menu-sep"></div>
    <button class="menu-item" onclick={markUnreadFromHere}>Mark as unread</button>
  </Popover>
{/if}

<style>
  .message {
    padding: 14px 0;
    border-bottom: 0.5px solid var(--separator-subtle);
  }
  .message.collapsed {
    padding: 10px 0;
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    outline: none;
  }
  .who {
    flex: 1;
    min-width: 0;
  }
  .line1 {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 14px;
    line-height: 18px;
    min-width: 0;
    white-space: nowrap;
  }
  .name {
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .email {
    font-size: 12px;
    color: var(--content-subtle);
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  /* Narrow reading pane (split view with the assistant open). */
  @container (max-width: 560px) {
    .email {
      display: none;
    }
  }
  .unread-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--interactive);
  }
  .badge {
    font-size: 10.5px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    font-weight: 600;
    border-radius: 4px;
    padding: 1px 5px;
  }
  .badge.draft {
    background: var(--draft-badge);
    color: var(--draft-text);
  }
  .line2 {
    font-size: 12px;
    color: var(--content-subtle);
    margin-top: 2px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .to {
    border: none;
    background: none;
    padding: 0;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .to:hover {
    color: var(--content);
  }
  .meta {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--content-subtle);
    flex: none;
  }
  .date {
    font-size: 13px;
    font-weight: 500;
  }
  .tools {
    display: flex;
    gap: 2px;
  }
  .starred {
    color: var(--identity-gold) !important;
  }
  .starred :global(svg) {
    fill: currentColor;
  }
  .details {
    margin: 10px 0 0 44px;
    padding: 10px 12px;
    border-radius: 8px;
    background: var(--surface-hover);
    font-size: 12px;
    line-height: 19px;
    color: var(--content-subtle);
  }
  .details b {
    color: var(--content);
    font-weight: 500;
    display: inline-block;
    width: 70px;
  }
  .privacy {
    margin: 10px 0 0 44px;
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .link {
    border: none;
    background: none;
    color: var(--accent);
    font-weight: 500;
    padding: 0;
  }
  .body {
    margin: 12px 0 0 44px;
  }
  .attachments {
    margin: 14px 0 0 44px;
    padding-top: 12px;
    border-top: 0.5px solid var(--separator-subtle);
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .att {
    display: flex;
    align-items: center;
    border: 0.5px solid var(--separator);
    border-radius: 8px;
    background: var(--surface-elevated);
    padding-right: 2px;
  }
  .att-open {
    display: flex;
    align-items: center;
    gap: 8px;
    border: none;
    background: none;
    padding: 7px 4px 7px 10px;
    max-width: 260px;
    color: var(--content);
  }
  .att-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
  }
  .att-size {
    font-size: 11px;
    color: var(--content-subtle);
    flex: none;
  }
</style>
