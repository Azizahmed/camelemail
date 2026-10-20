<script lang="ts">
  import { ArrowLeft, Check, MailMinus, Archive } from "@lucide/svelte";
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { displayName, listDate } from "../lib/format";
  import type { NewsletterSender } from "../lib/types";
  import { L } from "../lib/types";
  import { account, act, mail, navigate } from "../lib/mail.svelte";
  import { toast, toastError } from "../lib/ui.svelte";
  import Avatar from "./Avatar.svelte";

  let senders: NewsletterSender[] = $state([]);
  let loading = $state(true);
  let busy = $state<Record<string, boolean>>({});
  let done = $state<Record<string, string>>({});

  const keyOf = (s: NewsletterSender) => `${s.accountId}|${s.sender.email}`;

  onMount(async () => {
    try {
      senders = await api.newsletterSenders(mail.view.accountId);
    } catch (e) {
      toastError(e);
    } finally {
      loading = false;
    }
  });

  async function unsubscribe(s: NewsletterSender) {
    const k = keyOf(s);
    busy[k] = true;
    try {
      const r = await api.unsubscribe(s.latestMessageId);
      const name = displayName(s.sender);
      if (r.method === "one-click") {
        done[k] = "Unsubscribed";
        toast(`Unsubscribed from ${name}`, {
          action: { label: "Archive their mail", run: () => archiveAll(s) },
        });
      } else if (r.method === "url") {
        done[k] = "Opened";
        toast("The unsubscribe page is open in your browser");
      } else if (r.method === "email") {
        done[k] = "Request sent";
        toast(`Sent an unsubscribe request to ${name}`);
      } else {
        toast(r.error ?? "No unsubscribe method available", { kind: "error" });
      }
    } catch (e) {
      toastError(e, "Could not unsubscribe");
    } finally {
      busy[k] = false;
    }
  }

  async function archiveAll(s: NewsletterSender) {
    const refs = await api
      .listThreads({ accountId: s.accountId, label: L.INBOX, search: `from:${s.sender.email}`, limit: 500 })
      .catch(() => []);
    if (refs.length === 0) {
      toast("Nothing in the inbox from this sender");
      return;
    }
    await act({ type: "archive" }, refs.map((t) => ({ accountId: t.accountId, threadId: t.threadId })));
  }
</script>

<div class="page">
  <div class="head">
    <button class="icon-btn" title="Back" onclick={() => navigate(mail.view.accountId, L.INBOX)}><ArrowLeft size={16} /></button>
    <div>
      <h1>Manage Subscriptions</h1>
      <p>Newsletters and mailing lists that write to you. Leave the ones you no longer read.</p>
    </div>
  </div>

  {#if loading}
    <div class="state"><span class="spinner"></span> Scanning your mail for newsletters…</div>
  {:else if senders.length === 0}
    <div class="state column">
      <MailMinus size={28} strokeWidth={1.5} />
      <b>No newsletters found</b>
      <span>We couldn't find senders with unsubscribe options in your downloaded mail.</span>
    </div>
  {:else}
    <div class="count">Found {senders.length} newsletter{senders.length === 1 ? "" : "s"}</div>
    {#each senders as s (keyOf(s))}
      {@const k = keyOf(s)}
      <div class="item">
        <Avatar name={displayName(s.sender)} email={s.sender.email} size={32} ring={mail.accounts.length > 1 ? account(s.accountId)?.color : null} />
        <div class="who">
          <div class="name" dir="auto">{displayName(s.sender)}</div>
          <div class="email">{s.sender.email}</div>
        </div>
        <div class="stats">
          <span>{s.messageCount}+ emails</span>
          <span>Last {listDate(s.lastDate)}</span>
        </div>
        <button class="icon-btn" title="Archive their inbox mail" onclick={() => archiveAll(s)}><Archive size={15} /></button>
        {#if done[k]}
          <span class="done"><Check size={14} /> {done[k]}</span>
        {:else}
          <button class="btn" disabled={busy[k]} onclick={() => unsubscribe(s)}>
            {#if busy[k]}<span class="spinner"></span>{/if} Unsubscribe
          </button>
        {/if}
      </div>
    {/each}
  {/if}
</div>

<style>
  .page {
    flex: 1;
    overflow-y: auto;
    width: 100%;
    max-width: 820px;
    margin: 0 auto;
    padding: 16px 24px 40px;
  }
  .head {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    margin-bottom: 16px;
  }
  h1 {
    font-size: 20px;
    margin: 0;
  }
  p {
    margin: 4px 0 0;
    color: var(--content-subtle);
  }
  .state {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 10px;
    padding: 16vh 0;
    color: var(--content-subtle);
    text-align: center;
  }
  .state.column {
    flex-direction: column;
  }
  .state b {
    color: var(--content);
  }
  .count {
    font-size: 12px;
    color: var(--content-subtle);
    margin: 0 0 8px;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 4px;
    border-bottom: 0.5px solid var(--separator-subtle);
  }
  .who {
    flex: 1;
    min-width: 0;
  }
  .name {
    font-weight: 500;
  }
  .email {
    font-size: 12px;
    color: var(--content-subtle);
  }
  .stats {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    font-size: 11.5px;
    color: var(--content-subtle);
  }
  .done {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 12px;
    color: var(--indicator-success);
    min-width: 104px;
    justify-content: center;
  }
</style>
