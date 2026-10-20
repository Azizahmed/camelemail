<script lang="ts">
  import {
    AlertTriangle,
    Archive,
    CheckSquare,
    Clock3,
    FolderInput,
    Mail,
    MailOpen,
    PanelLeft,
    PenSquare,
    RefreshCw,
    Search,
    Square,
    Star,
    Trash2,
    WifiOff,
    X,
    ArrowLeft,
    CloudDownload,
    Columns2,
    Rows3,
    Sparkles,
  } from "@lucide/svelte";
  import { api } from "../lib/api";
  import { titled } from "../lib/shortcuts.svelte";
  import { L } from "../lib/types";
  import {
    act,
    compose,
    key,
    mail,
    navigate,
    openPage,
    refreshThreads,
    runServerSearch,
    selection,
    snooze,
    splitLayout,
    toggleAssistant,
    toggleLayout,
    toggleRead,
    toggleStar,
    trashOrRestore,
  } from "../lib/mail.svelte";
  import MoveMenu from "./menus/MoveMenu.svelte";
  import SnoozeMenu from "./menus/SnoozeMenu.svelte";
  import SpaceSwitch from "./SpaceSwitch.svelte";

  let searchInput: HTMLInputElement | undefined = $state();
  let menu = $state<null | { kind: "move" | "snooze"; anchor: DOMRect }>(null);
  let searchTimer: ReturnType<typeof setTimeout> | undefined;

  const syncing = $derived(Object.values(mail.syncing).some(Boolean));
  const authProblems = $derived(mail.accounts.filter((a) => a.status === "auth_error"));
  const offlineAccounts = $derived(mail.accounts.filter((a) => a.status === "offline"));
  const allSelected = $derived(mail.threads.length > 0 && mail.threads.every((t) => selection.has(key(t))));

  export function focusSearch() {
    openPage("search");
    queueMicrotask(() => searchInput?.focus());
  }

  function onSearchInput() {
    clearTimeout(searchTimer);
    mail.searchedServer = false;
    searchTimer = setTimeout(() => refreshThreads(), 180);
  }

  function exitSearch() {
    mail.searchQuery = "";
    navigate(mail.view.accountId, mail.view.label === "" ? L.INBOX : mail.view.label);
  }

  function addChip(op: string) {
    const q = mail.searchQuery.trim();
    mail.searchQuery = q ? `${q} ${op}` : op;
    onSearchInput();
    searchInput?.focus();
  }

  const bulkAccount = $derived.by(() => {
    const ids = new Set([...selection].map((k) => k.split("|")[0]));
    return ids.size === 1 ? [...ids][0] : null;
  });
</script>

<header class="header" class:search-mode={mail.page === "search"} class:bulk={selection.size > 0}>
  {#if !mail.sidebarDocked}
    <button class="icon-btn" title="Show sidebar" onclick={() => (mail.sidebarOpen = !mail.sidebarOpen)}><PanelLeft size={16} /></button>
  {/if}
  <SpaceSwitch />

  {#if selection.size > 0}
    <div class="bulk-bar">
      <button
        class="icon-btn"
        title={allSelected ? "Deselect all" : "Select all"}
        onclick={() => {
          if (allSelected) selection.clear();
          else for (const t of mail.threads) selection.add(key(t));
        }}
      >
        {#if allSelected}<CheckSquare size={16} />{:else}<Square size={16} />{/if}
      </button>
      <span class="count">{selection.size} selected</span>
      <button class="icon-btn" title="Clear selection (Esc)" onclick={() => selection.clear()}><X size={15} /></button>
      <div class="bulk-actions">
        {#if mail.view.label === L.INBOX || mail.view.label === L.UNREAD || mail.page === "search"}
          <button class="icon-btn" title={titled("Archive", "archive")} onclick={() => act({ type: "archive" })}><Archive size={16} /></button>
        {/if}
        <button class="icon-btn" title={mail.view.label === L.TRASH ? "Restore to inbox" : titled("Move to trash", "trash")} onclick={() => trashOrRestore()}><Trash2 size={16} /></button>
        <button class="icon-btn" title={titled("Mark read / unread", "unread")} onclick={() => toggleRead()}><MailOpen size={16} /></button>
        <button class="icon-btn" title={titled("Star", "star")} onclick={() => toggleStar()}><Star size={16} /></button>
        <button class="icon-btn" title={titled("Snooze", "snooze")} onclick={(e) => (menu = { kind: "snooze", anchor: e.currentTarget.getBoundingClientRect() })}><Clock3 size={16} /></button>
        {#if bulkAccount}
          <button class="icon-btn" title={titled("Move to folder", "move")} onclick={(e) => (menu = { kind: "move", anchor: e.currentTarget.getBoundingClientRect() })}><FolderInput size={16} /></button>
        {/if}
      </div>
    </div>
  {:else if mail.page === "search"}
    <div class="search-box">
      <button class="icon-btn" title="Back (Esc)" onclick={exitSearch}><ArrowLeft size={16} /></button>
      <Search size={15} />
      <input
        bind:this={searchInput}
        bind:value={mail.searchQuery}
        oninput={onSearchInput}
        onkeydown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            exitSearch();
          }
          if (e.key === "Enter") refreshThreads();
        }}
        placeholder="Search emails…   from:  to:  subject:  has:attachment  is:unread  before:2025/01/31"
        spellcheck="false"
        dir="auto"
      />
      {#if mail.searchQuery}
        <button class="icon-btn" title="Clear search" onclick={() => { mail.searchQuery = ""; onSearchInput(); searchInput?.focus(); }}><X size={14} /></button>
      {/if}
    </div>
  {:else}
    <button class="icon-btn" title={titled("Search", "search")} onclick={focusSearch}><Search size={16} /></button>
  {/if}

  <div class="right">
    {#if !mail.online}
      <span class="offline" title="You are offline"><WifiOff size={13} /> Offline</span>
    {/if}
    {#if authProblems.length > 0}
      <button class="warn-btn" title={authProblems.map((a) => `${a.email}: ${a.lastError ?? ""}`).join("\n")} onclick={() => api.openSettings("accounts")}>
        <AlertTriangle size={13} />
        {authProblems.length === 1 ? "Sign in again" : `${authProblems.length} accounts need sign-in`}
      </button>
    {:else if offlineAccounts.length > 0 && mail.online}
      <span class="offline" title={offlineAccounts.map((a) => `${a.email}: ${a.lastError ?? ""}`).join("\n")}><AlertTriangle size={13} /> Can't reach server</span>
    {/if}
    <button class="icon-btn" class:spinning={syncing} title={titled("Sync now", "sync")} onclick={() => api.syncNow()}><RefreshCw size={15} /></button>
    <button class="icon-btn" title={titled(splitLayout() ? "Full-width list" : "Split view with reading pane", "layout")} onclick={() => toggleLayout()}>
      {#if splitLayout()}<Rows3 size={16} />{:else}<Columns2 size={16} />{/if}
    </button>
    <button class="icon-btn ai" class:on={mail.assistantOpen} title={titled("AI assistant", "assistant")} onclick={toggleAssistant}><Sparkles size={16} /></button>
    <button class="icon-btn compose" title={titled("Compose", "compose")} onclick={() => compose()}><PenSquare size={16} /></button>
  </div>
</header>

{#if mail.page === "search"}
  <div class="chips">
    <button class="chip" onclick={() => addChip("is:unread")}><Mail size={12} /> Unread</button>
    <button class="chip" onclick={() => addChip("has:attachment")}>Has file</button>
    <button class="chip" onclick={() => addChip("is:starred")}><Star size={12} /> Starred</button>
    <button class="chip" onclick={() => addChip("newer_than:7d")}>Last 7 days</button>
    <button class="chip" onclick={() => addChip("newer_than:30d")}>Last 30 days</button>
    <button class="chip" onclick={() => addChip("in:sent")}>In Sent</button>
    <span class="grow"></span>
    {#if mail.searchQuery.trim()}
      <button class="chip server" disabled={mail.searchingServer} onclick={runServerSearch} title="Look for older mail that isn't downloaded yet">
        {#if mail.searchingServer}<span class="spinner"></span> Searching server…{:else}<CloudDownload size={13} /> Search server{/if}
      </button>
    {/if}
  </div>
{/if}

{#if menu?.kind === "snooze"}
  <SnoozeMenu anchor={menu.anchor} onclose={() => (menu = null)} onpick={(at) => { menu = null; snooze(at); }} />
{:else if menu?.kind === "move" && bulkAccount}
  <MoveMenu
    anchor={menu.anchor}
    accountId={bulkAccount}
    current={mail.view.label}
    onclose={() => (menu = null)}
    onpick={(to) => { menu = null; act({ type: "move", from: mail.page === "search" ? null : mail.view.label, to }); }}
  />
{/if}

<style>
  .header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 16px;
    min-height: 46px;
    background: var(--surface);
  }
  .right {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-left: auto;
  }
  .spinning :global(svg) {
    animation: spin 1s linear infinite;
  }
  .compose {
    color: var(--accent);
  }
  .ai.on {
    color: var(--accent);
    background: var(--accent-soft);
  }
  .offline,
  .warn-btn {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 12px;
    color: var(--content-subtle);
    padding: 3px 8px;
    border-radius: 999px;
    background: var(--surface-hover);
  }
  .warn-btn {
    border: none;
    color: var(--indicator-warning);
  }
  .search-box {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 8px;
    max-width: 900px;
    margin: 0 auto;
    color: var(--content-subtle);
  }
  .search-box input {
    flex: 1;
    border: none;
    outline: none;
    background: none;
    font-size: 15px;
    padding: 6px 0;
    user-select: text;
  }
  .chips {
    display: flex;
    gap: 8px;
    padding: 4px 24px 10px;
    border-bottom: 0.5px solid var(--separator-subtle);
    flex-wrap: wrap;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 12px;
    border: 0.5px solid var(--border-tertiary);
    background: transparent;
    border-radius: 8px;
    padding: 5px 9px;
    color: var(--content-subtle);
  }
  .chip:hover {
    color: var(--content);
    background: var(--surface-hover);
  }
  .chip.server {
    color: var(--accent);
    border-color: var(--accent);
  }
  .grow {
    flex: 1;
  }
  .bulk-bar {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 8px;
    max-width: 1100px;
    margin: 0 auto;
  }
  .count {
    font-size: 13px;
    color: var(--content-subtle);
  }
  .bulk-actions {
    margin-left: auto;
    display: flex;
    gap: 4px;
  }
</style>
