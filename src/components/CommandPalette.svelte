<script lang="ts">
  import {
    Archive,
    FileText,
    Folder,
    Inbox,
    Keyboard,
    MailMinus,
    MailOpen,
    PenSquare,
    RefreshCw,
    Search,
    Send,
    Settings,
    ShieldAlert,
    Star,
    Trash2,
    Clock3,
    CalendarClock,
    PanelLeft,
    Columns2,
    Sparkles,
    Layers,
    Plug,
    CalendarDays,
    Eraser,
    MailPlus,
  } from "@lucide/svelte";
  import { api } from "../lib/api";
  import { L } from "../lib/types";
  import {
    accountName,
    act,
    compose,
    mail,
    navigate,
    openPage,
    setSmartTab,
    showCalendar,
    splitLayout,
    targets,
    toggleAssistant,
    toggleLayout,
    toggleStar,
  } from "../lib/mail.svelte";
  import { keyHint } from "../lib/shortcuts.svelte";
  import { toast, toastError } from "../lib/ui.svelte";

  let { onsearch }: { onsearch: () => void } = $props();

  interface Command {
    id: string;
    label: string;
    category: string;
    hint?: string;
    icon: typeof Inbox;
    color?: string;
    run: () => void | Promise<void>;
  }

  let query = $state("");
  let index = $state(0);
  let listEl: HTMLDivElement | undefined = $state();

  const close = () => (mail.modal = null);

  const commands = $derived.by((): Command[] => {
    const cmds: Command[] = [
      { id: "search", label: "Search emails", category: "Actions", hint: keyHint("search"), icon: Search, run: onsearch },
      { id: "compose", label: "Compose new email", category: "Actions", hint: keyHint("compose"), icon: PenSquare, run: () => compose() },
      { id: "sync", label: "Sync emails", category: "Actions", hint: keyHint("sync"), icon: RefreshCw, run: () => api.syncNow() },
      { id: "sidebar", label: "Toggle sidebar", category: "Actions", hint: keyHint("sidebar"), icon: PanelLeft, run: () => { mail.sidebarOpen = !mail.sidebarOpen; } },
      { id: "layout", label: splitLayout() ? "Switch to full-width list" : "Switch to split view (reading pane)", category: "Actions", hint: keyHint("layout"), icon: Columns2, run: () => toggleLayout() },
      { id: "assistant", label: "Ask the AI assistant", category: "Actions", hint: keyHint("assistant"), icon: Sparkles, run: () => { if (!mail.assistantOpen) toggleAssistant(); } },
      { id: "connections", label: "Connect AI agents (MCP)", category: "Actions", icon: Plug, run: () => api.openSettings("connections") },
      { id: "subscribe", label: "Subscribe to a calendar (iCal link)", category: "Actions", icon: CalendarDays, run: () => api.openSettings("calendar") },
      { id: "autoclean", label: "Auto-clean settings", category: "Actions", icon: Eraser, run: () => api.openSettings("autoclean") },
      { id: "add-account", label: "Add an email account", category: "Actions", icon: MailPlus, run: () => api.openSettings("accounts") },
      { id: "inbox", label: "Go to Inbox", category: "Navigation", hint: keyHint("go.inbox"), icon: Inbox, run: () => navigate(mail.view.accountId, L.INBOX) },
      { id: "calendar", label: "Go to Calendar", category: "Navigation", hint: keyHint("go.calendar"), icon: CalendarDays, run: showCalendar },
      ...(mail.settings?.smartInbox
        ? [
            { id: "important", label: "Go to Important", category: "Navigation", icon: Inbox, run: () => { if (mail.view.label !== L.INBOX || mail.page !== "list") navigate(mail.view.accountId, L.INBOX); setSmartTab("important"); } },
            { id: "other", label: "Go to Other (updates, promotions…)", category: "Navigation", icon: Layers, run: () => { if (mail.view.label !== L.INBOX || mail.page !== "list") navigate(mail.view.accountId, L.INBOX); setSmartTab("other"); } },
          ]
        : []),
      { id: "starred", label: "Go to Starred", category: "Navigation", hint: keyHint("go.starred"), icon: Star, run: () => navigate(mail.view.accountId, L.STARRED) },
      { id: "snoozed", label: "Go to Snoozed", category: "Navigation", hint: keyHint("go.snoozed"), icon: Clock3, run: () => navigate(mail.view.accountId, L.SNOOZED) },
      { id: "sent", label: "Go to Sent", category: "Navigation", hint: keyHint("go.sent"), icon: Send, run: () => navigate(mail.view.accountId, L.SENT) },
      { id: "drafts", label: "Go to Drafts", category: "Navigation", hint: keyHint("go.drafts"), icon: FileText, run: () => navigate(mail.view.accountId, L.DRAFTS) },
      { id: "scheduled", label: "Go to Scheduled", category: "Navigation", icon: CalendarClock, run: () => navigate(mail.view.accountId, L.SCHEDULED) },
      { id: "archive-view", label: "Go to Archive", category: "Navigation", hint: keyHint("go.archive"), icon: Archive, run: () => navigate(mail.view.accountId, L.ARCHIVE) },
      { id: "trash", label: "Go to Trash", category: "Navigation", hint: keyHint("go.trash"), icon: Trash2, run: () => navigate(mail.view.accountId, L.TRASH) },
      { id: "spam", label: "Go to Spam", category: "Navigation", hint: keyHint("go.spam"), icon: ShieldAlert, run: () => navigate(mail.view.accountId, L.SPAM) },
      { id: "subs", label: "Manage Subscriptions", category: "Navigation", icon: MailMinus, run: () => openPage("unsubscribe") },
    ];
    for (const [i, a] of mail.accounts.entries()) {
      cmds.push({
        id: `acc-${a.id}`,
        label: `Go to ${accountName(a)} inbox`,
        category: "Accounts",
        hint: i < 9 ? `Ctrl ${i + 1}` : undefined,
        icon: Inbox,
        color: a.color,
        run: () => {
          mail.sidebarAccount = mail.accounts.length > 1 ? a.id : null;
          navigate(a.id, L.INBOX);
        },
      });
    }
    for (const [accId, labels] of Object.entries(mail.labels)) {
      for (const l of labels.filter((x) => x.kind === "user")) {
        const acc = mail.accounts.find((a) => a.id === accId);
        cmds.push({
          id: `label-${accId}-${l.id}`,
          label: `Go to ${l.name}`,
          category: mail.accounts.length > 1 ? `Folders · ${accountName(acc)}` : "Folders",
          icon: Folder,
          color: acc?.color,
          run: () => navigate(mail.accounts.length > 1 ? accId : null, l.id),
        });
      }
    }
    // Thread actions only apply to what you can see (not from the calendar).
    const t = mail.space === "mail" ? targets() : [];
    if (t.length > 0) {
      cmds.push(
        { id: "t-archive", label: "Archive thread", category: "Thread actions", hint: keyHint("archive"), icon: Archive, run: () => act({ type: "archive" }, t) },
        { id: "t-trash", label: "Move to Trash", category: "Thread actions", hint: keyHint("trash"), icon: Trash2, run: () => act({ type: "trash" }, t) },
        { id: "t-unread", label: "Mark as unread", category: "Thread actions", hint: keyHint("unread"), icon: MailOpen, run: () => act({ type: "markUnread" }, t) },
        { id: "t-star", label: "Star / unstar thread", category: "Thread actions", hint: keyHint("star"), icon: Star, run: () => toggleStar(t) },
        { id: "t-spam", label: "Mark as spam", category: "Thread actions", hint: keyHint("spam"), icon: ShieldAlert, run: () => act({ type: "spam" }, t) },
      );
      const thread = mail.threads.find((x) => x.accountId === t[0].accountId && x.threadId === t[0].threadId);
      if (thread?.unsubscribe && t.length === 1) {
        cmds.push({
          id: "t-unsub",
          label: "Unsubscribe from this sender",
          category: "Thread actions",
          icon: MailMinus,
          run: async () => {
            try {
              const r = await api.unsubscribe(thread.latestMessageId);
              toast(r.method === "none" ? (r.error ?? "No unsubscribe method available") : "Unsubscribe request sent");
            } catch (e) {
              toastError(e);
            }
          },
        });
      }
      if (t.length === 1) {
        for (const l of (mail.labels[t[0].accountId] ?? []).filter((x) => x.kind === "user")) {
          cmds.push({
            id: `move-${l.id}`,
            label: `Move to ${l.name}`,
            category: "Move to",
            icon: Folder,
            run: () => act({ type: "move", from: mail.page === "list" ? mail.view.label : null, to: l.id }, t),
          });
        }
      }
    }
    cmds.push(
      { id: "help", label: "Keyboard shortcuts", category: "Help", hint: keyHint("help"), icon: Keyboard, run: () => { mail.modal = "help"; } },
      { id: "settings", label: "Settings", category: "Help", hint: keyHint("settings"), icon: Settings, run: () => api.openSettings() },
      { id: "shortcuts", label: "Customize keyboard shortcuts", category: "Help", icon: Keyboard, run: () => api.openSettings("shortcuts") },
    );
    return cmds;
  });

  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (!q) return commands;
    return commands.filter((c) => c.label.toLowerCase().includes(q) || c.category.toLowerCase().includes(q));
  });

  $effect(() => {
    void query;
    index = 0;
  });

  $effect(() => {
    const i = index;
    listEl?.querySelector(`[data-i="${i}"]`)?.scrollIntoView({ block: "nearest" });
  });

  function run(c: Command | undefined) {
    if (!c) return;
    close();
    void c.run();
  }

  function keydown(e: KeyboardEvent) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      index = Math.min(filtered.length - 1, index + 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      index = Math.max(0, index - 1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      run(filtered[index]);
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      close();
    }
  }
</script>

<div class="overlay" role="presentation" onclick={(e) => { if (e.target === e.currentTarget) close(); }}>
  <div class="palette" role="dialog" aria-label="Command palette">
    <div class="input-row">
      <Search size={16} />
      <!-- svelte-ignore a11y_autofocus -->
      <input bind:value={query} onkeydown={keydown} placeholder="Type a command or search…" autofocus spellcheck="false" dir="auto" />
      <kbd>ESC</kbd>
    </div>
    <div class="list" bind:this={listEl}>
      {#each filtered as c, i (c.id)}
        {#if i === 0 || filtered[i - 1].category !== c.category}
          <div class="group">{c.category}</div>
        {/if}
        <button class="item" class:selected={i === index} data-i={i} onmouseenter={() => (index = i)} onclick={() => run(c)}>
          <span class="icon" style:color={c.color}><c.icon size={15} /></span>
          <span class="label">{c.label}</span>
          {#if c.hint}<kbd>{c.hint}</kbd>{/if}
        </button>
      {:else}
        <div class="none">No commands found</div>
      {/each}
    </div>
  </div>
</div>

<style>
  .palette {
    width: min(620px, 92vw);
    background: var(--surface-elevated);
    border: 1px solid var(--separator);
    border-radius: 14px;
    box-shadow: var(--shadow-dropdown);
    overflow: hidden;
    animation: slideUp 0.18s ease;
  }
  .input-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 14px 18px;
    border-bottom: 0.5px solid var(--separator-subtle);
    color: var(--content-subtle);
  }
  .input-row input {
    flex: 1;
    border: none;
    outline: none;
    background: none;
    font-size: 14px;
    line-height: 18px;
    user-select: text;
  }
  .list {
    max-height: 52vh;
    overflow-y: auto;
    padding: 6px;
  }
  .group {
    font-size: 11px;
    font-weight: 600;
    color: var(--content-muted);
    text-transform: uppercase;
    letter-spacing: 0.4px;
    padding: 10px 10px 4px;
  }
  .item {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border: none;
    background: none;
    border-radius: 8px;
    text-align: left;
    font-size: 13px;
  }
  .item.selected {
    background: var(--surface-elevated-hover);
  }
  .icon {
    display: flex;
    color: var(--icon);
  }
  .label {
    flex: 1;
  }
  .none {
    padding: 18px;
    text-align: center;
    color: var(--content-subtle);
  }
</style>
