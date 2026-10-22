<script lang="ts">
  // Search on the phone: a query box, quick filters, "search by" operators and
  // the results (local first; older mail from the server on request).
  import { ArrowLeft, CalendarRange, CloudDownload, EyeOff, FileText, Folder, Mail, Paperclip, Send, Star, User, Users, X } from "@lucide/svelte";
  import { onDestroy, onMount } from "svelte";
  import { t as tr, type MessageKey } from "../lib/i18n.svelte";
  import { act, key, mail, navigate, openPage, openThread, refOf, refreshThreads, runServerSearch, snooze, toggleRead, toggleStar, trashOrRestore } from "../lib/mail.svelte";
  import { L, type ThreadSummary } from "../lib/types";
  import { back, push } from "./nav.svelte";
  import SnoozeSheet from "./SnoozeSheet.svelte";
  import ThreadRow from "./ThreadRow.svelte";
  import type { SwipeAction } from "./views.svelte";

  const QUICK = [
    { op: "is:unread", name: "m.view.unread", icon: Mail },
    { op: "is:starred", name: "m.view.starred", icon: Star },
    { op: "has:attachment", name: "m.search.attachments", icon: Paperclip },
    { op: "newer_than:7d", name: "m.search.last7", icon: CalendarRange },
  ];
  const BY = [
    { op: "from:", name: "m.search.from", icon: User },
    { op: "to:", name: "m.search.to", icon: Send },
    { op: "cc:", name: "m.search.cc", icon: Users },
    { op: "bcc:", name: "m.search.bcc", icon: EyeOff },
    { op: "subject:", name: "m.search.subject", icon: FileText },
    { op: "in:", name: "m.search.folder", icon: Folder },
    { op: "after:", name: "m.search.after", icon: CalendarRange },
    { op: "before:", name: "m.search.before", icon: CalendarRange },
  ];

  const before = { ...mail.view };
  let input: HTMLInputElement | undefined = $state();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let snoozeOpen = $state(false);
  let snoozeTarget: ThreadSummary | null = null;

  const query = $derived(mail.searchQuery.trim());
  const scope = $derived(
    mail.accounts.length > 1 && !mail.view.accountId ? tr("m.search.scopeAll") : mail.accounts.length > 1 ? tr("m.search.scopeOne", { email: mail.accounts.find((a) => a.id === mail.view.accountId)?.email ?? "" }) : "",
  );

  onMount(() => {
    mail.searchQuery = "";
    openPage("search");
    setTimeout(() => input?.focus(), 150);
  });

  onDestroy(() => {
    clearTimeout(timer);
    mail.searchQuery = "";
    navigate(before.accountId, before.label || L.INBOX);
  });

  function changed() {
    clearTimeout(timer);
    mail.searchedServer = false;
    timer = setTimeout(() => refreshThreads(), 200);
  }

  function add(op: string) {
    const q = mail.searchQuery.trimEnd();
    // Operators that take a value leave the caret right after them.
    mail.searchQuery = op.endsWith(":") ? (q ? `${q} ${op}` : op) : q ? `${q} ${op} ` : `${op} `;
    changed();
    input?.focus();
  }

  function has(op: string) {
    return mail.searchQuery.split(/\s+/).includes(op);
  }

  function toggle(op: string) {
    if (has(op)) {
      mail.searchQuery = mail.searchQuery.split(/\s+/).filter((w) => w !== op).join(" ");
      changed();
    } else add(op);
  }

  function swipe(t: ThreadSummary, a: SwipeAction) {
    const refs = [refOf(t)];
    if (a === "star") void toggleStar(refs);
    else if (a === "trash") void trashOrRestore(refs);
    else if (a === "archive") void act({ type: "archive" }, refs);
    else if (a === "read") void toggleRead(refs);
    else if (a === "snooze") {
      snoozeTarget = t;
      snoozeOpen = true;
    }
  }
</script>

<div class="m-screen">
  <div class="field">
    <button class="m-icon-btn" aria-label={tr("common.back")} onclick={() => back()}><ArrowLeft size={20} strokeWidth={1.6} /></button>
    <input
      bind:this={input}
      bind:value={mail.searchQuery}
      oninput={changed}
      onkeydown={(e) => e.key === "Enter" && (input?.blur(), refreshThreads())}
      type="search"
      enterkeyhint="search"
      placeholder={tr("m.search.placeholder")}
      dir="auto"
      autocomplete="off"
      autocapitalize="off"
      spellcheck="false"
    />
    {#if mail.searchQuery}
      <button class="m-icon-btn" aria-label={tr("m.search.clear")} onclick={() => { mail.searchQuery = ""; changed(); input?.focus(); }}><X size={18} strokeWidth={1.6} /></button>
    {/if}
  </div>
  <div class="chips">
    {#each QUICK as c (c.op)}
      {@const Icon = c.icon}
      <button class="chip" class:on={has(c.op)} onclick={() => toggle(c.op)}><Icon size={16} fill={c.op === "is:starred" || c.op === "is:unread" ? "currentColor" : "none"} strokeWidth={1.6} /> {tr(c.name as MessageKey)}</button>
    {/each}
  </div>

  <div class="m-scroll">
    {#if !query}
      {#if scope}<p class="scope">{scope}</p>{/if}
      <h3>{tr("m.search.by")}</h3>
      <div class="by">
        {#each BY as b (b.op)}
          {@const Icon = b.icon}
          <button class="op" onclick={() => add(b.op)}><Icon size={18} fill="currentColor" strokeWidth={1.4} /><span>{tr(b.name as MessageKey)}</span><code>{b.op}</code></button>
        {/each}
      </div>
    {:else}
      {#if scope}<p class="scope">{scope}</p>{/if}
      {#if mail.loading && mail.threads.length === 0}
        <div class="m-empty"><span class="spinner"></span></div>
      {:else if mail.threads.length === 0}
        <div class="m-empty">{tr("m.search.noMatch", { query })}</div>
      {:else}
        {#each mail.threads as t (key(t))}
          <ThreadRow thread={t} selecting={false} selected={false} onopen={() => { openThread(t); push({ name: "thread" }); }} onselect={() => {}} onswipe={(a) => swipe(t, a)} />
        {/each}
      {/if}
      <div class="server">
        <button class="m-primary soft" disabled={mail.searchingServer || mail.searchedServer} onclick={runServerSearch}>
          {#if mail.searchingServer}<span class="spinner"></span> {tr("m.search.searching")}{:else}<CloudDownload size={16} /> {mail.searchedServer ? tr("m.search.searched") : tr("m.search.older")}{/if}
        </button>
      </div>
    {/if}
  </div>
</div>

<SnoozeSheet bind:open={snoozeOpen} onpick={(at) => snoozeTarget && snooze(at, [refOf(snoozeTarget)])} />

<style>
  .field {
    flex: none;
    display: flex;
    align-items: center;
    gap: 3px;
    height: 52px;
    margin: 13px 13px 8px;
    padding: 0 7px;
    border-radius: 26px;
    background: var(--m-surface-2);
  }
  .field input {
    flex: 1;
    min-width: 0;
    height: 100%;
    border: 0;
    outline: 0;
    background: transparent;
    color: var(--m-text);
    font: inherit;
    font-size: 16px;
    -webkit-user-select: text;
    user-select: text;
  }
  .field input::placeholder {
    color: var(--m-text-3);
  }
  .field input::-webkit-search-cancel-button {
    display: none;
  }
  .chips {
    flex: none;
    display: flex;
    gap: 8px;
    overflow-x: auto;
    padding: 2px 13px 10px;
    scrollbar-width: none;
  }
  .chip {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    height: 38px;
    padding: 0 18px 0 15px;
    border: 0;
    border-radius: 19px;
    background: var(--m-surface-2);
    color: var(--m-text);
    font: inherit;
    font-size: 15px;
  }
  .chip :global(svg) {
    color: var(--m-badge);
  }
  .chip.on {
    background: var(--m-accent-soft);
    color: var(--m-accent);
  }
  .scope {
    margin: 11px var(--m-pad) 5px;
    font-size: 14px;
    color: var(--m-text-2);
  }
  h3 {
    margin: 21px var(--m-pad) 11px;
    font-size: 14px;
    font-weight: 450;
    letter-spacing: 0.6px;
    text-transform: uppercase;
    color: var(--m-text-2);
  }
  .by {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin: 0 28px 25px;
  }
  .op {
    display: flex;
    align-items: center;
    gap: 18px;
    height: 44px;
    padding: 0 15px;
    border: 0;
    border-radius: 8px;
    background: var(--m-surface-2);
    color: var(--m-text);
    font: inherit;
    font-size: 16px;
    text-align: start;
  }
  .op :global(svg) {
    flex: none;
    color: var(--m-badge);
  }
  .op span {
    flex: 1;
  }
  .op code {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--m-text-2);
  }
  .server {
    display: flex;
    justify-content: center;
    padding: 16px 13px 33px;
  }
</style>
