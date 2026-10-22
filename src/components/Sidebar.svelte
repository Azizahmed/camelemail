<script lang="ts">
  import {
    Archive,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    Clock3,
    CalendarClock,
    FileText,
    Folder,
    FolderPlus,
    Inbox,
    Mail,
    MailMinus,
    PanelLeftClose,
    PanelLeftOpen,
    Eye,
    EyeOff,
    Send,
    Settings,
    CalendarDays,
    ShieldAlert,
    Star,
    Trash2,
    AlertCircle,
  } from "@lucide/svelte";
  import { api } from "../lib/api";
  import { L, type Label } from "../lib/types";
  import {
    accountName,
    isHidden,
    labelName,
    mail,
    navigate,
    showCalendar,
    openPage,
    setDocked,
    setSidebarWidth,
    toggleHidden,
    unreadFor,
    scheduledCount,
  } from "../lib/mail.svelte";
  import { toast, toastError } from "../lib/ui.svelte";

  const STANDARD = [L.INBOX, L.UNREAD, L.STARRED, L.SNOOZED, L.ARCHIVE, L.SENT, L.SCHEDULED, L.DRAFTS, L.SPAM, L.TRASH];
  const ICONS: Record<string, typeof Inbox> = {
    [L.INBOX]: Inbox,
    [L.UNREAD]: Mail,
    [L.STARRED]: Star,
    [L.SNOOZED]: Clock3,
    [L.ARCHIVE]: Archive,
    [L.SENT]: Send,
    [L.SCHEDULED]: CalendarClock,
    [L.DRAFTS]: FileText,
    [L.SPAM]: ShieldAlert,
    [L.TRASH]: Trash2,
  };

  let showMore = $state(localStorage.getItem("sidebarMore") === "1");
  let newFolder = $state<string | null>(null);
  let collapsed = $state<Record<string, boolean>>({});

  const single = $derived(mail.accounts.length === 1 ? mail.accounts[0] : null);
  /** The Inbox always stays; anything else can be tucked under "More". */
  const shown = $derived(STANDARD.filter((id) => id === L.INBOX || !isHidden(id)));
  const tucked = $derived(STANDARD.filter((id) => id !== L.INBOX && isHidden(id)));
  const folderKey = (accountId: string, id: string) => `folder:${accountId}:${id}`;

  function startResize(e: PointerEvent) {
    // No text selection across the page while dragging.
    e.preventDefault();
    document.body.style.userSelect = "none";
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => setSidebarWidth(ev.clientX);
    const up = (ev: PointerEvent) => {
      document.body.style.userSelect = "";
      setSidebarWidth(ev.clientX, true);
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  function go(accountId: string | null, label: string) {
    navigate(accountId, label);
    if (!mail.sidebarDocked) mail.sidebarOpen = false;
  }

  function active(accountId: string | null, label: string) {
    if (mail.space === "calendar") return false;
    return mail.page === "list" || mail.page === "scheduled"
      ? mail.view.label === label && mail.view.accountId === accountId
      : false;
  }

  function userFolders(accountId: string): Label[] {
    return (mail.labels[accountId] ?? []).filter((l) => l.kind === "user");
  }

  function depth(l: Label): number {
    let d = 0;
    let p = l.parentId;
    const all = mail.labels[l.accountId] ?? [];
    while (p && d < 8) {
      d++;
      p = all.find((x) => x.id === p)?.parentId ?? null;
    }
    return d;
  }

  function hidden(l: Label): boolean {
    let p = l.parentId;
    const all = mail.labels[l.accountId] ?? [];
    while (p) {
      if (collapsed[`${l.accountId}|${p}`]) return true;
      p = all.find((x) => x.id === p)?.parentId ?? null;
    }
    return false;
  }

  function hasChildren(l: Label): boolean {
    return (mail.labels[l.accountId] ?? []).some((x) => x.parentId === l.id);
  }

  async function createFolder(accountId: string) {
    const name = (newFolder ?? "").trim();
    newFolder = null;
    if (!name) return;
    try {
      await api.createFolder(accountId, name);
      toast(`Created ${name}`);
    } catch (e) {
      toastError(e, "Failed to create folder");
    }
  }
</script>

{#if !mail.sidebarDocked}
  <!-- Hover the left edge to reveal the sidebar. -->
  <div class="hover-zone" role="presentation" onmouseenter={() => (mail.sidebarOpen = true)}></div>
  {#if mail.sidebarOpen}
    <div class="backdrop" role="presentation" onclick={() => (mail.sidebarOpen = false)}></div>
  {/if}
{/if}

<nav
  class="sidenav"
  class:open={mail.sidebarOpen || mail.sidebarDocked}
  class:docked={mail.sidebarDocked}
  onmouseleave={(e) => {
    // Only auto-hide when the pointer leaves towards the content.
    if (!mail.sidebarDocked && e.clientX > 0) mail.sidebarOpen = false;
  }}
  aria-label="Folders"
>
  <!-- Drag the edge to make the sidebar wider or narrower. -->
  <div class="resizer" role="separator" aria-orientation="vertical" aria-label="Resize sidebar" onpointerdown={startResize}></div>
  <div class="brand">
    <img class="logo" src="/brand/camel-mail-icon.png" alt="" />
    <span class="word">Camel Mail</span>
    <button class="icon-btn dock" title={mail.sidebarDocked ? "Undock sidebar" : "Keep sidebar open"} onclick={() => setDocked(!mail.sidebarDocked)}>
      {#if mail.sidebarDocked}<PanelLeftClose size={15} />{:else}<PanelLeftOpen size={15} />{/if}
    </button>
  </div>

  <div class="pages" class:sub={mail.sidebarAccount !== null}>
    <!-- Page 1: unified folders + accounts -->
    <div class="page">
      <div class="section">
        {#each shown as id (id)}
          {@render item(null, id)}
        {/each}
        {@render more(null, single?.id ?? null)}
      </div>

      {#if single}
        <div class="divider"></div>
        {@render folders(single.id)}
      {:else if mail.accounts.length > 1}
        <div class="divider"></div>
        <div class="section">
          <div class="heading">Accounts</div>
          {#each mail.accounts as a (a.id)}
            <button
              class="item"
              class:active={mail.view.accountId === a.id}
              onclick={() => {
                mail.sidebarAccount = a.id;
                navigate(a.id, L.INBOX);
              }}
              title={a.email}
            >
              <span class="dot" style:background={a.color}></span>
              <span class="name" dir="auto">{accountName(a)}</span>
              {#if a.status !== "active"}<span class="warn" title={a.lastError ?? ""}><AlertCircle size={13} /></span>{/if}
              {#if (mail.unread[a.id] ?? 0) > 0}<span class="badge">{mail.unread[a.id]}</span>{/if}
              <ChevronRight size={13} class="chev" />
            </button>
          {/each}
        </div>
      {/if}

      <div class="section bottom">
        <button class="item" class:active={mail.space === "calendar"} onclick={showCalendar}><CalendarDays size={14} /><span class="name">Calendar</span></button>
        <button class="item" onclick={() => { openPage("unsubscribe"); if (!mail.sidebarDocked) mail.sidebarOpen = false; }} class:active={mail.space === "mail" && mail.page === "unsubscribe"}>
          <MailMinus size={14} /><span class="name">Subscriptions</span>
        </button>
        <button class="item" onclick={() => api.openSettings()}><Settings size={14} /><span class="name">Settings</span></button>
      </div>
    </div>

    <!-- Page 2: one account's folders -->
    <div class="page">
      {#if mail.sidebarAccount}
        {@const acc = mail.accounts.find((a) => a.id === mail.sidebarAccount)}
        <button class="back" onclick={() => { mail.sidebarAccount = null; navigate(null, L.INBOX); }} title="Back to all accounts">
          <ChevronLeft size={14} />
          <span class="dot" style:background={acc?.color}></span>
          <span dir="auto">{accountName(acc)}</span>
        </button>
        <div class="section">
          {#each shown as id (id)}
            {@render item(mail.sidebarAccount, id)}
          {/each}
          {@render more(mail.sidebarAccount, mail.sidebarAccount)}
        </div>
        <div class="divider"></div>
        {@render folders(mail.sidebarAccount)}
      {/if}
    </div>
  </div>
</nav>

{#snippet item(accountId: string | null, id: string)}
  {@const Icon = ICONS[id] ?? Folder}
  {@const count = id === L.SCHEDULED ? scheduledCount(accountId) : unreadFor(id, accountId)}
  <button class="item" class:active={active(accountId, id)} onclick={() => go(accountId, id)}>
    <Icon size={14} />
    <span class="name" dir="auto">{labelName(id, accountId)}</span>
    {#if count > 0}<span class="badge">{count > 999 ? "999+" : count}</span>{/if}
    {#if id !== L.INBOX}{@render tuck(id)}{/if}
  </button>
{/snippet}

{#snippet tuck(key: string)}
  {@const away = isHidden(key)}
  <span
    class="tuck"
    role="button"
    tabindex="-1"
    title={away ? "Show in the list" : "Move to More"}
    onclick={(e) => { e.stopPropagation(); toggleHidden(key); }}
    onkeydown={() => {}}
  >{#if away}<Eye size={12} />{:else}<EyeOff size={12} />{/if}</span>
{/snippet}

<!-- "More": what the user tucked away, for every account (accountId null) or one; and that account's tucked folders. -->
{#snippet more(accountId: string | null, foldersOf: string | null)}
  {@const folderList = foldersOf ? userFolders(foldersOf).filter((l) => isHidden(folderKey(foldersOf, l.id))) : []}
  {#if tucked.length + folderList.length > 0}
    <button class="item more" onclick={() => { showMore = !showMore; localStorage.setItem("sidebarMore", showMore ? "1" : "0"); }}>
      {#if showMore}<ChevronDown size={14} />{:else}<ChevronRight size={14} />{/if}
      <span>More</span>
    </button>
    {#if showMore}
      {#each tucked as id (id)}
        {@render item(accountId, id)}
      {/each}
      {#each folderList as l (l.id)}
        <button class="item" class:active={active(single ? null : foldersOf, l.id)} onclick={() => go(single ? null : foldersOf, l.id)} title={l.path ?? l.name}>
          <Folder size={14} />
          <span class="name" dir="auto">{l.name.split(/[/.]/).pop()}</span>
          {#if l.unreadCount > 0}<span class="badge">{l.unreadCount}</span>{/if}
          {@render tuck(folderKey(foldersOf!, l.id))}
        </button>
      {/each}
    {/if}
  {/if}
{/snippet}

{#snippet folders(accountId: string)}
  <div class="section">
    <div class="heading">
      Folders
      <button class="icon-btn add" title="Add folder" onclick={() => (newFolder = "")}><FolderPlus size={13} /></button>
    </div>
    {#if newFolder !== null}
      <!-- svelte-ignore a11y_autofocus -->
      <input
        class="input folder-input"
        placeholder="Folder name"
        bind:value={newFolder}
        autofocus
        onkeydown={(e) => {
          if (e.key === "Enter") createFolder(accountId);
          if (e.key === "Escape") { e.stopPropagation(); newFolder = null; }
        }}
        onblur={() => createFolder(accountId)}
      />
    {/if}
    {#each userFolders(accountId) as l (l.id)}
      {#if !hidden(l) && !isHidden(folderKey(accountId, l.id))}
        <button class="item" class:active={active(accountId, l.id) || (single !== null && active(null, l.id))} onclick={() => go(single ? null : accountId, l.id)} style:padding-left="{10 + depth(l) * 14}px" title={l.path ?? l.name}>
          {#if hasChildren(l)}
            <span
              class="twisty"
              role="button"
              tabindex="-1"
              title={collapsed[`${accountId}|${l.id}`] ? "Expand" : "Collapse"}
              onclick={(e) => { e.stopPropagation(); collapsed[`${accountId}|${l.id}`] = !collapsed[`${accountId}|${l.id}`]; }}
              onkeydown={() => {}}
            >{#if collapsed[`${accountId}|${l.id}`]}<ChevronRight size={12} />{:else}<ChevronDown size={12} />{/if}</span>
          {:else}
            <Folder size={14} />
          {/if}
          <span class="name" dir="auto">{l.name.split(/[/.]/).pop()}</span>
          {#if l.unreadCount > 0}<span class="badge">{l.unreadCount}</span>{/if}
          {@render tuck(folderKey(accountId, l.id))}
        </button>
      {/if}
    {:else}
      {#if newFolder === null}<div class="hint">No folders yet</div>{/if}
    {/each}
  </div>
{/snippet}

<style>
  .hover-zone {
    position: fixed;
    left: 0;
    top: 0;
    bottom: 0;
    width: 10px;
    z-index: 900;
  }
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.3);
    z-index: 999;
    animation: fade 0.15s ease;
  }
  .sidenav {
    position: fixed;
    left: 0;
    top: 0;
    bottom: 0;
    width: var(--sidenav-width);
    background: var(--surface-nav);
    border-right: 0.5px solid var(--separator);
    z-index: 1000;
    transform: translateX(-100%);
    transition: transform 0.2s ease;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .sidenav.open {
    transform: none;
    box-shadow: 3px 0 16px #0000001f;
  }
  .sidenav.docked {
    box-shadow: none;
    z-index: 50;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 10px 12px 18px;
  }
  .logo {
    width: 22px;
    height: 22px;
    object-fit: contain;
  }
  .word {
    font-weight: 700;
    font-size: 15px;
    letter-spacing: -0.2px;
  }
  .dock {
    margin-left: auto;
  }
  .pages {
    flex: 1;
    display: flex;
    width: 200%;
    transition: transform 0.22s ease;
    min-height: 0;
  }
  .pages.sub {
    transform: translateX(-50%);
  }
  .page {
    width: 50%;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    padding-bottom: 10px;
  }
  .section {
    padding: 0 8px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .section.bottom {
    margin-top: auto;
    padding-top: 14px;
  }
  .heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.4px;
    color: var(--content-muted);
    padding: 4px 6px 4px 10px;
  }
  .heading .add {
    width: 22px;
    height: 22px;
  }
  .divider {
    height: 1px;
    background: var(--separator-subtle);
    margin: 8px 16px;
  }
  .item {
    position: relative;
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 6px 6px 6px 10px;
    border: none;
    background: none;
    border-radius: 8px;
    font-size: 13px;
    line-height: 17px;
    text-align: left;
    color: var(--content);
    width: 100%;
  }
  .item :global(svg) {
    color: var(--icon);
    flex: none;
  }
  .item:hover {
    background: var(--surface-nav-hover);
  }
  .item.active {
    background: var(--surface-nav-active);
  }
  .item.more {
    color: var(--content-subtle);
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badge {
    font-size: 11px;
    font-weight: 500;
    color: var(--content-subtle);
    background: var(--surface-nav-hover);
    border-radius: 12px;
    padding: 0 6px;
    min-width: 20px;
    text-align: center;
    line-height: 16px;
  }
  .item.active .badge {
    background: var(--surface-nav-hover);
  }
  .item:hover:not(.active) .badge {
    background: var(--surface-nav-active);
  }
  .tuck {
    display: none;
    position: absolute;
    right: 6px;
    width: 18px;
    height: 18px;
    align-items: center;
    justify-content: center;
    border-radius: 4px;
    background: var(--surface-nav-active);
  }
  .item:hover .tuck {
    display: flex;
    color: var(--icon);
  }
  .tuck:hover {
    color: var(--icon-hover) !important;
    background: var(--surface-active);
  }
  .item:hover .badge {
    visibility: hidden;
  }
  .resizer {
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    width: 5px;
    cursor: col-resize;
    z-index: 2;
  }
  .resizer:hover,
  .resizer:active {
    background: color-mix(in srgb, var(--accent) 35%, transparent);
  }
  .dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    flex: none;
    margin: 0 2px;
  }
  .warn :global(svg) {
    color: var(--indicator-warning);
  }
  .item :global(.chev) {
    opacity: 0.5;
  }
  .back {
    display: flex;
    align-items: center;
    gap: 8px;
    border: none;
    background: var(--surface-nav);
    padding: 8px 16px 10px;
    font-size: 12px;
    font-weight: 500;
    color: var(--content);
    text-align: left;
    position: sticky;
    top: 0;
  }
  .twisty {
    display: flex;
    width: 14px;
    justify-content: center;
  }
  .hint {
    font-size: 12px;
    color: var(--content-muted);
    padding: 4px 10px;
  }
  .folder-input {
    height: 28px;
    margin: 2px 0 4px;
  }
</style>
