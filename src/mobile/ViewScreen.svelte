<script lang="ts">
  // Scheduled sends and newsletter subscriptions, as phone screens.
  import { ArrowLeft, CalendarClock, Clock3, Pencil, Send, X } from "@lucide/svelte";
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { t } from "../lib/i18n.svelte";
  import { subjectDir } from "../lib/bidi";
  import { wakeDate } from "../lib/format";
  import { account, mail, refreshOutbox } from "../lib/mail.svelte";
  import type { OutboxItem } from "../lib/types";
  import { confirmDialog, toast, toastError } from "../lib/ui.svelte";
  import UnsubscribeView from "../components/UnsubscribeView.svelte";
  import { back } from "./nav.svelte";
  import Sheet from "./Sheet.svelte";
  import { presetName } from "./views.svelte";

  let { kind }: { kind: "scheduled" | "unsubscribe" } = $props();

  let picking = $state<OutboxItem | null>(null);
  let pickOpen = $state(false);
  let sheet: Sheet | undefined = $state();
  let presets = $state<{ id: string; label: string; at: number }[]>([]);
  let custom = $state("");

  const items = $derived(
    mail.outbox.filter((o) => (o.kind === "scheduled" || o.status === "failed") && (!mail.view.accountId || o.accountId === mail.view.accountId)),
  );

  onMount(() => {
    if (kind === "scheduled") void refreshOutbox();
  });

  function recipients(o: OutboxItem): string {
    const all = [...o.draft.to, ...o.draft.cc, ...o.draft.bcc];
    if (all.length === 0) return t("m.sched.noRecipient");
    const first = all[0].name || all[0].email;
    return all.length > 1 ? `${first} +${all.length - 1}` : first;
  }

  async function sendNow(o: OutboxItem) {
    try {
      await api.sendNow(o.id);
      toast(t("m.sched.sendingNow"));
    } catch (e) {
      toastError(e, t("m.sched.sendFailed"));
    }
  }

  async function cancel(o: OutboxItem) {
    const r = await confirmDialog({ title: t("m.sched.cancelTitle"), message: t("m.sched.cancelMessage"), confirmLabel: t("m.sched.cancel"), cancelLabel: t("m.sched.keep") });
    if (r !== "confirm") return;
    try {
      await api.undoSend(o.id);
      toast(t("m.sched.cancelled"));
      await refreshOutbox();
    } catch (e) {
      toastError(e, t("m.sched.cancelFailed"));
    }
  }

  async function edit(o: OutboxItem) {
    try {
      const d = await api.undoSend(o.id);
      if (!d) return toast(t("m.sched.alreadySending"), { kind: "error" });
      await api.openCompose(d.id);
      toast(t("m.sched.editOpened"));
    } catch (e) {
      toastError(e, t("m.sched.editFailed"));
    }
  }

  async function reschedule(o: OutboxItem) {
    picking = o;
    presets = await api.sendLaterPresets().catch(() => []);
    pickOpen = true;
  }

  async function pick(at: number) {
    const o = picking;
    await sheet?.close();
    if (!o || !(at > Date.now())) return;
    try {
      await api.reschedule(o.id, at);
      toast(t("m.sched.rescheduled", { when: wakeDate(at) }));
      await refreshOutbox();
    } catch (e) {
      toastError(e, t("m.sched.rescheduleFailed"));
    }
  }
</script>

<div class="m-screen">
  {#if kind === "unsubscribe"}
    <div class="m-scroll unsub"><UnsubscribeView onback={() => back()} /></div>
  {:else}
    <header class="m-topbar">
      <button class="m-icon-btn" aria-label={t("common.back")} onclick={() => back()}><ArrowLeft size={20} strokeWidth={1.6} /></button>
      <h1>{t("m.view.scheduled")}</h1>
    </header>
    <div class="m-scroll">
      {#if items.length === 0}
        <div class="m-empty"><CalendarClock size={33} strokeWidth={1.2} /><p>{t("m.sched.none")}</p></div>
      {:else}
        {#each items as o (o.id)}
          {@const acc = account(o.accountId)}
          <div class="item" class:failed={o.status === "failed"}>
            <div class="line">
              {#if mail.accounts.length > 1 && acc}<span class="acc" style:background={acc.color}></span>{/if}
              <span class="who" dir="auto">{recipients(o)}</span>
              <span class="when">{o.status === "sending" ? t("m.sched.sending") : o.status === "failed" ? t("m.sched.failed") : wakeDate(o.sendAt)}</span>
            </div>
            <div class="subject" dir={subjectDir(o.draft.subject)}>{o.draft.subject || "(No subject)"}</div>
            {#if o.lastError}<div class="err">{o.lastError}</div>{/if}
            <div class="actions">
              <button class="m-icon-btn" aria-label={t("m.sched.sendNow")} onclick={() => sendNow(o)}><Send size={16} strokeWidth={1.6} /></button>
              <button class="m-icon-btn" aria-label={t("m.sched.edit")} onclick={() => edit(o)}><Pencil size={16} strokeWidth={1.6} /></button>
              <button class="m-icon-btn" aria-label={t("m.sched.changeTime")} onclick={() => reschedule(o)}><Clock3 size={16} strokeWidth={1.6} /></button>
              <button class="m-icon-btn" aria-label={t("m.sched.cancel")} onclick={() => cancel(o)}><X size={16} strokeWidth={1.6} /></button>
            </div>
          </div>
        {/each}
      {/if}
    </div>
  {/if}
</div>

<Sheet bind:this={sheet} bind:open={pickOpen} title={t("m.sched.sendAt")}>
  {#each presets as p (p.id)}
    <button class="m-sheet-item" onclick={() => pick(p.at)}><Clock3 size={18} strokeWidth={1.6} /> {presetName(p)}<small>{wakeDate(p.at)}</small></button>
  {/each}
  <div class="m-sheet-sep"></div>
  <label class="m-sheet-item"><CalendarClock size={18} strokeWidth={1.6} /><input class="dt" type="datetime-local" bind:value={custom} onchange={() => pick(new Date(custom).getTime())} aria-label={t("m.snooze.pick")} /></label>
</Sheet>

<style>
  .unsub {
    padding: 7px 3px;
  }
  .item {
    padding: 11px var(--m-pad) 5px;
    border-bottom: 1px solid var(--m-separator);
  }
  .line {
    display: flex;
    align-items: center;
    gap: 7px;
  }
  .acc {
    width: 3px;
    height: 13px;
    border-radius: 2px;
  }
  .who {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 16px;
  }
  .when {
    flex: none;
    font-size: 12px;
    color: var(--scheduled-text);
  }
  .failed .when,
  .err {
    color: var(--indicator-error);
  }
  .subject {
    margin-top: 2px;
    font-size: 14px;
    color: var(--m-text-read);
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .err {
    font-size: 11px;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    margin-inline-end: -12px;
  }
  .dt {
    flex: 1;
    border: 0;
    background: transparent;
    color: var(--m-text);
    font: inherit;
  }
</style>
