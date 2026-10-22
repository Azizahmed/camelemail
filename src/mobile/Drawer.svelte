<script lang="ts">
  // Side drawer: mailboxes, views, Smart Inbox buckets, folders, calendar.
  import { CalendarDays, MailMinus, Plus, Settings, Sparkles } from "@lucide/svelte";
  import { t } from "../lib/i18n.svelte";
  import { accountName, mail, scheduledCount, unreadFor } from "../lib/mail.svelte";
  import { L, type Label } from "../lib/types";
  import { back, push } from "./nav.svelte";
  import { bucketName, currentView, openView, showView, SMART_BUCKETS, viewIcon, viewName } from "./views.svelte";

  let { onsettings }: { onsettings: (section?: string) => void } = $props();

  const smart = $derived(!!mail.settings?.smartInbox);
  const STANDARD = [L.INBOX, L.UNREAD, L.STARRED, L.SNOOZED, L.ARCHIVE, L.SENT, L.SCHEDULED, L.DRAFTS, L.SPAM, L.TRASH];
  const active = $derived(currentView());
  const acc = $derived(mail.view.accountId);

  function badge(id: string): number {
    if (id === L.SCHEDULED) return scheduledCount(acc);
    if (id === L.UNREAD) return unreadFor(L.INBOX, acc);
    if (id.startsWith("smart:")) return 0;
    return unreadFor(id, acc);
  }

  const folders = $derived.by(() => {
    const ids = acc ? [acc] : mail.accounts.map((a) => a.id);
    return ids.map((id) => ({ accountId: id, labels: (mail.labels[id] ?? []).filter((l) => l.kind === "user") })).filter((g) => g.labels.length > 0);
  });

  function depth(l: Label): number {
    let d = 0;
    let p = l.parentId;
    const all = mail.labels[l.accountId] ?? [];
    while (p && d < 6) {
      d++;
      p = all.find((x) => x.id === p)?.parentId ?? null;
    }
    return d;
  }

  async function go(id: string, accountId: string | null = acc) {
    await back();
    openView(id, accountId);
  }

  async function open(fn: () => void) {
    await back();
    fn();
  }

  async function pickAccount(id: string | null) {
    await back();
    showView(L.INBOX, id);
  }

  const fmt = (n: number) => (n > 99 ? "99+" : String(n));
</script>

<div class="m-scrim" onclick={() => back()} role="presentation"></div>
<aside class="drawer" aria-label={t("m.drawer.label")}>
  <h2>Remail</h2>

  {#if mail.accounts.length > 1}
    <div class="accounts" role="tablist" aria-label={t("m.drawer.mailboxes")}>
      <button class="acc" class:on={acc === null} role="tab" aria-selected={acc === null} onclick={() => pickAccount(null)}>{t("m.drawer.allInboxes")}</button>
      {#each mail.accounts as a (a.id)}
        <button class="acc" class:on={acc === a.id} role="tab" aria-selected={acc === a.id} onclick={() => pickAccount(a.id)}>
          <span class="dot" style:background={a.color}></span>{accountName(a)}
        </button>
      {/each}
    </div>
  {/if}

  <div class="items">
    {#each STANDARD as id (id)}
      {@const Icon = viewIcon(id)}
      {@const n = badge(id)}
      <button class="item" class:on={active === id} onclick={() => go(id)}>
        <Icon size={20} strokeWidth={1.5} />
        <span class="label">{viewName(id)}</span>
        {#if n > 0}<span class="badge" class:round={n < 10}>{fmt(n)}</span>{/if}
      </button>
      {#if id === L.INBOX && smart}
        {#each SMART_BUCKETS as b (b.id)}
          {@const BIcon = b.icon}
          <button class="item" class:on={active === `smart:${b.id}`} onclick={() => go(`smart:${b.id}`)}>
            <BIcon size={20} strokeWidth={1.5} /><span class="label">{bucketName(b.id)}</span>
          </button>
        {/each}
      {/if}
    {/each}

    {#each folders as g (g.accountId)}
      <div class="group">{mail.accounts.length > 1 && !acc ? accountName(mail.accounts.find((a) => a.id === g.accountId)) : t("m.drawer.folders")}</div>
      {#each g.labels as l (l.id)}
        {@const id = `folder:${g.accountId}:${l.id}`}
        {@const Icon = viewIcon(id)}
        <button class="item" class:on={active === id} style:padding-inline-start="{16 + depth(l) * 15}px" onclick={() => go(id, g.accountId)}>
          <Icon size={18} strokeWidth={1.5} />
          <span class="label" dir="auto">{l.name.split(/[/.]/).pop() || l.name}</span>
          {#if l.unreadCount > 0}<span class="badge" class:round={l.unreadCount < 10}>{fmt(l.unreadCount)}</span>{/if}
        </button>
      {/each}
    {/each}

    <div class="sep"></div>
    <button class="item" onclick={() => open(() => push({ name: "calendar" }))}><CalendarDays size={20} strokeWidth={1.5} /><span class="label">{t("m.view.calendar")}</span></button>
    <button class="item" onclick={() => open(() => push({ name: "unsubscribe" }))}><MailMinus size={20} strokeWidth={1.5} /><span class="label">{t("m.drawer.subscriptions")}</span></button>
  </div>

  <div class="foot">
    <button class="m-icon-btn" aria-label={t("mset.title")} onclick={() => open(() => onsettings())}><Settings size={21} strokeWidth={1.5} /></button>
    {#if mail.ai.ready}
      <button class="m-primary soft pill" onclick={() => open(() => push({ name: "assistant" }))}><Sparkles size={16} strokeWidth={1.6} /> {t("m.drawer.assistant")}</button>
    {:else}
      <button class="m-primary soft pill" onclick={() => open(() => onsettings("accounts"))}><Plus size={16} strokeWidth={1.6} /> {t("m.drawer.addMailbox")}</button>
    {/if}
  </div>
</aside>

<style>
  .drawer {
    position: fixed;
    top: 0;
    bottom: 0;
    inset-inline-start: 0;
    width: min(279px, 84vw);
    display: flex;
    flex-direction: column;
    background: var(--m-surface);
    color: var(--m-text);
    animation: in 0.2s ease-out;
  }
  @keyframes in {
    from {
      transform: translateX(-40px);
      opacity: 0.5;
    }
  }
  h2 {
    margin: 21px 28px 18px;
    font-size: 25px;
    font-weight: 600;
    letter-spacing: 0.2px;
  }
  .accounts {
    display: flex;
    gap: 7px;
    overflow-x: auto;
    padding: 0 15px 10px;
    scrollbar-width: none;
  }
  .acc {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 30px;
    padding: 0 11px;
    border: 1px solid var(--m-separator);
    border-radius: 15px;
    background: transparent;
    color: var(--m-text);
    font: inherit;
    font-size: 12px;
  }
  .acc.on {
    background: var(--m-pill);
    border-color: transparent;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
  }
  .items {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0 11px 8px;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 21px;
    width: 100%;
    height: 44px;
    padding: 0 16px;
    border: 0;
    border-radius: 22px;
    background: transparent;
    color: var(--m-text);
    font: inherit;
    font-size: 16px;
    text-align: start;
  }
  .item.on,
  .item:active {
    background: var(--m-pill);
  }
  .label {
    flex: 1;
    min-width: 0;
    /* Tall enough that Arabic descenders (final ى, ض) aren't clipped. */
    line-height: 1.6;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .badge {
    flex: none;
    min-width: 25px;
    height: 25px;
    padding: 0 8px;
    border-radius: 12px;
    background: var(--m-badge);
    color: #fff;
    font-size: 12px;
    line-height: 25px;
    text-align: center;
  }
  .badge.round {
    padding: 0;
  }
  .group {
    margin: 15px 16px 5px;
    font-size: 11px;
    color: var(--m-text-2);
  }
  .sep {
    height: 1px;
    margin: 8px 5px;
    background: var(--m-separator);
  }
  .foot {
    flex: none;
    display: flex;
    align-items: center;
    gap: 11px;
    padding: 10px 18px 13px;
    border-top: 1px solid var(--m-separator);
  }
  .pill {
    flex: 1;
    height: 43px;
    border-radius: 21px;
    font-size: 15px;
  }
</style>
