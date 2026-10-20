<script lang="ts">
  import { AlertCircle, CalendarClock, Clock3, Pencil, Send, X } from "@lucide/svelte";
  import { subjectDir } from "../lib/bidi";
  import { api } from "../lib/api";
  import { absoluteDate, wakeDate } from "../lib/format";
  import type { OutboxItem } from "../lib/types";
  import { account, mail, refreshOutbox } from "../lib/mail.svelte";
  import { confirmDialog, toast, toastError } from "../lib/ui.svelte";
  import Avatar from "./Avatar.svelte";
  import EmptyState from "./EmptyState.svelte";
  import SendLaterMenu from "./menus/SendLaterMenu.svelte";

  let menu = $state<{ item: OutboxItem; anchor: DOMRect } | null>(null);

  const items = $derived(
    mail.outbox.filter((o) => (o.kind === "scheduled" || o.status === "failed") && (!mail.view.accountId || o.accountId === mail.view.accountId)),
  );

  function recipients(o: OutboxItem): string {
    const all = [...o.draft.to, ...o.draft.cc, ...o.draft.bcc];
    if (all.length === 0) return "(no recipient)";
    const first = all[0].name || all[0].email;
    return all.length > 1 ? `${first} +${all.length - 1}` : first;
  }

  async function sendNow(o: OutboxItem) {
    try {
      await api.sendNow(o.id);
      toast("Sending now");
    } catch (e) {
      toastError(e, "Failed to send");
    }
  }

  async function cancel(o: OutboxItem) {
    const r = await confirmDialog({
      title: "Cancel scheduled send?",
      message: "The message will move back to your drafts.",
      confirmLabel: "Cancel send",
      cancelLabel: "Keep",
    });
    if (r !== "confirm") return;
    try {
      await api.undoSend(o.id);
      toast("Scheduled send cancelled");
      await refreshOutbox();
    } catch (e) {
      toastError(e, "Failed to cancel");
    }
  }

  async function edit(o: OutboxItem) {
    try {
      const d = await api.undoSend(o.id);
      if (!d) {
        toast("This email is already being sent", { kind: "error" });
        return;
      }
      await api.openCompose(d.id);
      toast("Opened for editing — schedule it again when you're done");
    } catch (e) {
      toastError(e, "Failed to open for editing");
    }
  }

  async function reschedule(o: OutboxItem, at: number) {
    menu = null;
    try {
      await api.reschedule(o.id, at);
      toast(`Schedule updated: ${wakeDate(at)}`);
    } catch (e) {
      toastError(e, "Failed to reschedule");
    }
  }
</script>

<div class="page">
  <div class="title"><CalendarClock size={18} /> Scheduled</div>
  <p class="note">Scheduled emails are sent by Remail on this computer — keep Remail running (it can stay in the tray) at the send time. Anything overdue goes out the next time Remail starts.</p>
  {#if items.length === 0}
    <EmptyState label="SCHEDULED" />
  {:else}
    {#each items as o (o.id)}
      {@const acc = account(o.accountId)}
      <div class="row" class:failed={o.status === "failed"}>
        <span class="lead">
          {#if o.status === "failed"}<span title={o.lastError ?? ""}><AlertCircle size={15} /></span>{:else}<Clock3 size={15} />{/if}
        </span>
        <Avatar name={recipients(o)} size={22} ring={mail.accounts.length > 1 ? acc?.color : null} />
        <span class="who">{recipients(o)}</span>
        <span class="preview">
          {#if o.status === "failed"}<span class="err-badge">Send failed</span>{/if}
          {#if o.draft.attachments.length > 0}<span class="att">{o.draft.attachments.length} file{o.draft.attachments.length > 1 ? "s" : ""}</span>{/if}
          <span class="subject" dir={subjectDir(o.draft.subject)}>{o.draft.subject || "(No subject)"}</span>
          {#if o.lastError}<span class="snippet">— {o.lastError}</span>{/if}
        </span>
        <span class="when" title={absoluteDate(o.sendAt)}>{o.status === "sending" ? "Sending…" : `Sends ${wakeDate(o.sendAt)}`}</span>
        <span class="actions">
          <button class="icon-btn" title="Send now" onclick={() => sendNow(o)}><Send size={15} /></button>
          <button class="icon-btn" title="Edit" onclick={() => edit(o)}><Pencil size={15} /></button>
          <button class="icon-btn" title="Change schedule time" onclick={(e) => (menu = { item: o, anchor: e.currentTarget.getBoundingClientRect() })}><Clock3 size={15} /></button>
          <button class="icon-btn" title="Cancel scheduled send" onclick={() => cancel(o)}><X size={15} /></button>
        </span>
      </div>
    {/each}
  {/if}
</div>

{#if menu}
  {@const item = menu.item}
  <SendLaterMenu anchor={menu.anchor} onclose={() => (menu = null)} onpick={(at) => reschedule(item, at)} />
{/if}

<style>
  .page {
    flex: 1;
    overflow-y: auto;
    width: 100%;
    max-width: 1196px;
    margin: 0 auto;
    padding: 8px 0 40px;
  }
  .title {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 16px;
    font-weight: 600;
    padding: 10px 28px 4px;
  }
  .note {
    margin: 0 28px 10px;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 45px;
    padding: 0 12px 0 8px;
    border-bottom: 0.5px solid var(--separator-subtle);
    position: relative;
    font-size: 12px;
  }
  .row:hover {
    background: var(--row-hover);
  }
  .lead {
    width: 20px;
    display: flex;
    justify-content: center;
    color: var(--icon);
  }
  .failed .lead {
    color: var(--indicator-error);
  }
  .who {
    width: 160px;
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .preview {
    flex: 1;
    min-width: 0;
    display: flex;
    gap: 6px;
    align-items: center;
    overflow: hidden;
    white-space: nowrap;
    color: var(--content-subtle);
  }
  .subject {
    color: var(--content);
    font-weight: 500;
  }
  .snippet {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .err-badge,
  .att {
    font-size: 10.5px;
    border-radius: 4px;
    padding: 0 6px;
    background: var(--surface-hover);
  }
  .err-badge {
    color: var(--indicator-error);
  }
  .when {
    color: var(--scheduled-text);
    white-space: nowrap;
  }
  .actions {
    position: absolute;
    right: 8px;
    display: none;
    gap: 2px;
    background: var(--surface);
    padding-left: 12px;
  }
  .row:hover .actions {
    display: flex;
  }
  .row:hover .when {
    visibility: hidden;
  }
</style>
