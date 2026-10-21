<script lang="ts">
  import { onMount } from "svelte";
  import { api, errorText } from "../lib/api";
  import { L } from "../lib/types";
  import {
    act,
    closeThread,
    compose,
    initMail,
    key,
    mail,
    navigate,
    openThread,
    refreshMeta,
    selection,
    showCalendar,
    showMail,
    splitLayout,
    stepThread,
    targets,
    toggleAssistant,
    toggleLayout,
    toggleRead,
    toggleStar,
    trashOrRestore,
    undoLast,
  } from "../lib/mail.svelte";
  import { bindingCaps, bindings, eventKey, isModifierKey, matchKey, PREFIX, PREFIX_TIMEOUT, type MatchOptions } from "../lib/shortcuts.svelte";
  import { isTyping, toastError } from "../lib/ui.svelte";
  import FirstMailbox from "./auth/FirstMailbox.svelte";
  import type CalendarViewType from "./calendar/CalendarView.svelte";
  import CommandPalette from "./CommandPalette.svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import Header from "./Header.svelte";
  import ScheduledView from "./ScheduledView.svelte";
  import ShortcutsHelp from "./ShortcutsHelp.svelte";
  import Sidebar from "./Sidebar.svelte";
  import ThreadList from "./ThreadList.svelte";
  import Toasts from "./Toasts.svelte";
  import UnsubscribeView from "./UnsubscribeView.svelte";
  import ThreadView from "./thread/ThreadView.svelte";

  let header: Header | undefined = $state();
  let list: ThreadList | undefined = $state();
  let threadView: ThreadView | undefined = $state();
  let calView: CalendarViewType | undefined = $state();
  // The calendar and the assistant load the first time they open.
  let CalendarView: typeof import("./calendar/CalendarView.svelte").default | undefined = $state();
  let AssistantPanel: typeof import("./assistant/AssistantPanel.svelte").default | undefined = $state();
  $effect(() => {
    if (mail.space === "calendar" && !CalendarView)
      import("./calendar/CalendarView.svelte").then(
        (m) => (CalendarView = m.default),
        (e) => {
          toastError(e, "Couldn't open the calendar");
          showMail();
        },
      );
  });
  $effect(() => {
    if (mail.assistantOpen && !AssistantPanel)
      import("./assistant/AssistantPanel.svelte").then(
        (m) => (AssistantPanel = m.default),
        (e) => {
          toastError(e, "Couldn't open the assistant");
          mail.assistantOpen = false;
        },
      );
  });
  /** Mail stays on screen until the calendar's code has arrived. */
  const calendarShown = $derived(mail.space === "calendar" && !!CalendarView);
  let startupError = $state("");
  /** The first key of a two-key shortcut, waiting for the second. */
  let pending: string | null = null;
  let pendingTimer: ReturnType<typeof setTimeout> | undefined;
  let anchorIndex = -1;

  const split = $derived(splitLayout());
  const listPage = $derived(mail.page === "list" || mail.page === "search");
  let listWidth = $state(Number(localStorage.getItem("listWidth")) || 420);
  let mainEl: HTMLElement | undefined = $state();

  function startResize(e: PointerEvent) {
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const left = mainEl?.getBoundingClientRect().left ?? 0;
    const max = (mainEl?.clientWidth ?? 1200) * 0.6;
    const move = (ev: PointerEvent) => {
      listWidth = Math.round(Math.max(300, Math.min(max, ev.clientX - left)));
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      localStorage.setItem("listWidth", String(listWidth));
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  onMount(() => {
    initMail().catch((e) => (startupError = errorText(e)));
  });

  // Auto-clean leaves the open conversation alone until you move on.
  $effect(() => {
    const o = mail.open;
    void api.setViewing(o ? { accountId: o.accountId, threadId: o.threadId } : null).catch(() => {});
  });

  const GO_LABELS: Record<string, string> = {
    "go.inbox": L.INBOX,
    "go.starred": L.STARRED,
    "go.sent": L.SENT,
    "go.drafts": L.DRAFTS,
    "go.trash": L.TRASH,
    "go.spam": L.SPAM,
    "go.snoozed": L.SNOOZED,
    "go.archive": L.ARCHIVE,
    "go.unread": L.UNREAD,
  };

  function focusMove(dir: 1 | -1, extend: boolean) {
    if (mail.threads.length === 0) return;
    list?.setKeyboardNav();
    const from = mail.focusedIndex;
    const next = Math.max(0, Math.min(mail.threads.length - 1, (from < 0 ? -1 : from) + dir));
    if (extend) {
      if (anchorIndex < 0) anchorIndex = Math.max(0, from);
      selection.clear();
      const [a, b] = [Math.min(anchorIndex, next), Math.max(anchorIndex, next)];
      for (let i = a; i <= b; i++) selection.add(key(mail.threads[i]));
    } else {
      anchorIndex = -1;
    }
    mail.focusedIndex = next;
  }

  /** Next / previous: through open threads, or the list focus (Shift extends the selection). */
  function step(dir: 1 | -1, shift: boolean) {
    if (split && listPage && shift) focusMove(dir, true);
    else if (mail.open || (split && listPage)) stepThread(dir);
    else focusMove(dir, shift);
  }

  function toggleFocusedSelection() {
    const t = mail.threads[mail.focusedIndex];
    if (!t) return;
    const k = key(t);
    if (selection.has(k)) selection.delete(k);
    else selection.add(k);
  }

  /** Enter and Space belong to a focused button or link (list rows' bodies aren't focus stops). */
  function onControl(e: KeyboardEvent): boolean {
    const t = e.target as Element | null;
    return !!t?.closest?.("button, a[href], [role=button]:not([tabindex='-1'])");
  }

  function escape() {
    if (mail.sidebarOpen && !mail.sidebarDocked) mail.sidebarOpen = false;
    else if (selection.size > 0) selection.clear();
    else if (mail.open) closeThread();
    else if (mail.page === "search" || mail.page === "unsubscribe" || mail.page === "scheduled") navigate(mail.view.accountId, mail.view.label === L.SCHEDULED ? L.INBOX : mail.view.label || L.INBOX);
  }

  /**
   * The action a key press runs (see Settings › Shortcuts), "" when the key was
   * taken as part of a two-key shortcut, or null when it isn't a shortcut.
   */
  function shortcut(e: KeyboardEvent, opts: MatchOptions): string | null {
    const prev = pending;
    if (prev) {
      pending = null;
      clearTimeout(pendingTimer);
      const id = matchKey(e, prev, opts);
      if (id && id !== PREFIX) return id;
      // A key that doesn't finish the sequence is spent, unless it's a Ctrl combination.
      if (!(e.ctrlKey || e.metaKey)) return "";
    }
    const id = matchKey(e, null, opts);
    if (id !== PREFIX) return id;
    pending = eventKey(e);
    pendingTimer = setTimeout(() => (pending = null), PREFIX_TIMEOUT);
    return "";
  }

  /** While the calendar is shown: app-wide shortcuts here, the rest to the calendar. */
  function calendarKeys(e: KeyboardEvent) {
    if (calView?.busy()) return; // the event editor handles its own keys
    const mod = e.ctrlKey || e.metaKey;
    if (!mod && e.key === "Escape" && mail.sidebarOpen && !mail.sidebarDocked) {
      mail.sidebarOpen = false;
      return;
    }
    if (mod || !isTyping(e)) {
      const id = shortcut(e, { typing: mod && isTyping(e), only: (a) => !!a.calendar });
      if (id === "") return;
      if (id) {
        e.preventDefault();
        if (id === "settings") void api.openSettings("calendar");
        else run(id, e);
        return;
      }
    }
    calView?.handleKey(e);
  }

  /** Run a shortcut's action (with the same open-vs-list rules as the buttons). */
  function run(id: string, e: KeyboardEvent) {
    const open = !!mail.open;
    const listy = split && listPage;
    const hasTarget = targets().length > 0;
    // Shift on a letter key only gets here with Caps Lock on: it extends the selection as before.
    const shift = e.shiftKey && e.key.length === 1 && e.key === e.key.toLowerCase();
    if (GO_LABELS[id]) {
      navigate(mail.view.accountId, GO_LABELS[id]);
      return;
    }
    switch (id) {
      case "go.calendar":
        showCalendar();
        break;
      case "palette":
        mail.modal = "palette";
        break;
      case "help":
        mail.modal = "help";
        break;
      case "sidebar":
        mail.sidebarOpen = !mail.sidebarOpen;
        break;
      case "compose":
        void compose();
        break;
      case "search":
        header?.focusSearch();
        break;
      case "sync":
        void api.syncNow();
        break;
      case "assistant":
        toggleAssistant();
        break;
      case "layout":
        void toggleLayout();
        break;
      case "settings":
        void api.openSettings();
        break;
      case "next":
        step(1, shift);
        break;
      case "prev":
        step(-1, shift);
        break;
      case "extendDown":
      case "extendUp":
        if (!open || listy) focusMove(id === "extendDown" ? 1 : -1, true);
        break;
      case "openFirst":
        if (!open && mail.threads[0]) openThread(mail.threads[0]);
        break;
      case "archive":
        if (hasTarget) void act({ type: "archive" });
        break;
      case "trash":
        if (hasTarget) void trashOrRestore();
        break;
      case "spam":
        if (hasTarget) void act({ type: "spam" });
        break;
      case "star":
        if (hasTarget) void toggleStar();
        break;
      case "unread":
        if (open) void act({ type: "markUnread" });
        else if (hasTarget) void toggleRead();
        break;
      case "undo":
        void undoLast();
        break;
      case "reply":
        if (open) threadView?.shortcut(mail.settings?.defaultReply === "replyAll" ? "replyAll" : "reply");
        break;
      case "replyAll":
        if (open) threadView?.shortcut("replyAll");
        break;
      case "forward":
        if (open) threadView?.shortcut("forward");
        break;
      case "snooze":
        if (open) threadView?.shortcut("snooze");
        else if (hasTarget) list?.openMenu("snooze");
        break;
      case "move":
        if (open) threadView?.shortcut("move");
        else if (hasTarget) list?.openMenu("move");
        break;
      case "select":
        if (!open || listy) toggleFocusedSelection();
        break;
      case "selectAll":
        if (mail.threads.every((t) => selection.has(key(t)))) selection.clear();
        else for (const t of mail.threads) selection.add(key(t));
        break;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (mail.modal === "palette") return;
    if (mail.modal === "help") {
      if (e.key === "Escape" || matchKey(e, null) === "help") {
        e.preventDefault();
        mail.modal = null;
      }
      return;
    }
    if (document.querySelector(".overlay")) return; // a dialog owns the keyboard
    if (isModifierKey(e.key)) return; // wait for the key it modifies
    if (mail.space === "calendar") return calendarKeys(e);
    const mod = e.ctrlKey || e.metaKey;
    const k = e.key;

    if (!mod) {
      if (k === "Escape") {
        if (isTyping(e) && !(e.target as HTMLElement).closest("[data-inline-composer]")) (e.target as HTMLElement).blur();
        escape();
        return;
      }
      if (isTyping(e)) return;
    }

    const id = shortcut(e, { typing: mod && isTyping(e) });
    if (id === "") return;
    if (id) {
      e.preventDefault();
      run(id, e);
      return;
    }

    // Keys that can't be changed.
    if (mod) {
      if (/^[0-9]$/.test(k)) {
        e.preventDefault();
        const n = Number(k);
        if (n === 0) {
          mail.sidebarAccount = null;
          navigate(null, L.INBOX);
        } else {
          const a = mail.accounts[n - 1];
          if (a) {
            mail.sidebarAccount = mail.accounts.length > 1 ? a.id : null;
            navigate(mail.accounts.length > 1 ? a.id : null, L.INBOX);
          }
        }
      } else if (k.toLowerCase() === "a" && !isTyping(e) && (!mail.open || (split && listPage))) {
        e.preventDefault();
        for (const t of mail.threads) selection.add(key(t));
      }
      return;
    }
    if (e.altKey) return;

    const open = !!mail.open;
    switch (k) {
      case "ArrowDown":
      case "ArrowUp":
        e.preventDefault();
        step(k === "ArrowDown" ? 1 : -1, e.shiftKey);
        break;
      case "Enter":
        if (onControl(e)) break;
        if (!open && mail.threads[mail.focusedIndex]) openThread(mail.threads[mail.focusedIndex]);
        break;
      case "Delete":
      case "Backspace":
        if (targets().length > 0) {
          e.preventDefault();
          void trashOrRestore();
        }
        break;
      case " ":
        if (onControl(e)) break;
        if (!open || (split && listPage)) {
          e.preventDefault();
          toggleFocusedSelection();
        }
        break;
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#snippet keys(id: string)}
  {#each bindingCaps(bindings(id)[0] ?? "") as caps, s (s)}{#if s > 0}&nbsp;then&nbsp;{/if}{#each caps as cap, i (i)}{#if i > 0}+{/if}<kbd>{cap}</kbd>{/each}{/each}
{/snippet}

{#if startupError}
  <div class="center">
    <div class="err-card">
      <b>Remail couldn't start</b>
      <span class="selectable">{startupError}</span>
    </div>
  </div>
{:else if !mail.ready}
  <div class="center"><span class="spinner"></span></div>
{:else if mail.accounts.length === 0 && mail.space === "mail"}
  <FirstMailbox ondone={() => { void refreshMeta().then(() => navigate(null, L.INBOX)); }} oncalendar={showCalendar} />
{:else}
  <!-- The mail and calendar screens aren't translated yet (design.md, Q3), so they
       keep their left-to-right layout in Arabic; text inside still follows its own
       direction (lib/bidi.ts). -->
  <div class="app" dir="ltr" class:docked={mail.sidebarDocked} style:--sidenav-width="{mail.sidebarWidth}px">
    <Sidebar />
    {#if calendarShown && CalendarView}
      <CalendarView bind:this={calView} />
    {/if}
    {#if mail.accounts.length > 0}
    <!-- Mail stays mounted (hidden) under the calendar, so a reply being
         written, scroll positions and running AI requests survive a switch. -->
    <div class="mail-space" class:away={calendarShown}>
    <Header bind:this={header} />
    <div class="body">
      <main class="main" class:split={split && listPage} bind:this={mainEl}>
        {#if mail.page === "scheduled"}
          <ScheduledView />
        {:else if mail.page === "unsubscribe"}
          <UnsubscribeView />
        {:else if split}
          <div class="list-pane" style:width="{mail.assistantOpen ? Math.min(listWidth, 340) : listWidth}px">
            <ThreadList bind:this={list} compact />
          </div>
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="resizer" title="Drag to resize" onpointerdown={startResize}></div>
          <div class="reading-pane">
            {#if mail.open}
              <ThreadView bind:this={threadView} layout="pane" />
            {:else}
              <div class="placeholder">
                <img src="/brand/remail-mark.png" alt="" />
                <div class="ph-title">{mail.threads.length ? "Select an email to read" : "Nothing to read here"}</div>
                <div class="ph-hint">
                  {@render keys("next")} {@render keys("prev")} move between emails{#if bindings("layout")[0]} · {@render keys("layout")} full-width list{/if}
                </div>
              </div>
            {/if}
          </div>
        {:else}
          <ThreadList bind:this={list} />
        {/if}
        {#if mail.open && !(split && listPage)}
          <ThreadView bind:this={threadView} />
        {/if}
      </main>
      {#if mail.assistantOpen && AssistantPanel}
        <AssistantPanel />
      {/if}
    </div>
    </div>
    {/if}
  </div>
{/if}

<div class="contents" dir="ltr">
  {#if mail.modal === "palette"}
    <CommandPalette onsearch={() => { showMail(); header?.focusSearch(); }} />
  {:else if mail.modal === "help"}
    <ShortcutsHelp />
  {/if}
</div>
<ConfirmDialog />
<Toasts />

<style>
  .contents {
    display: contents;
  }
  .app {
    height: 100vh;
    display: flex;
    flex-direction: column;
  }
  .app.docked {
    padding-left: var(--sidenav-width);
  }
  .mail-space {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .mail-space.away {
    display: none;
  }
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .main {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    position: relative;
    padding: 0 20px;
  }
  .main.split {
    flex-direction: row;
    padding: 0 0 0 8px;
  }
  .list-pane {
    flex: none;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .resizer {
    flex: none;
    width: 6px;
    margin-left: 2px;
    cursor: col-resize;
    border-left: 0.5px solid var(--separator);
  }
  .resizer:hover {
    border-left: 2px solid var(--accent-soft);
  }
  .reading-pane {
    flex: 1;
    min-width: 0;
    position: relative;
    background: var(--surface-reading);
    container-type: inline-size;
  }
  .placeholder {
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    color: var(--content-subtle);
  }
  .placeholder img {
    width: 64px;
    opacity: 0.35;
    filter: grayscale(0.3);
    margin-bottom: 6px;
  }
  .ph-title {
    font-size: 15px;
    font-weight: 500;
    color: var(--content);
  }
  .ph-hint {
    font-size: 12px;
  }
  .center {
    height: 100vh;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .err-card {
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-width: 520px;
    padding: 20px;
    border-radius: 12px;
    background: var(--surface-elevated);
    border: 1px solid var(--separator);
    color: var(--content-subtle);
  }
  .err-card b {
    color: var(--content);
  }
</style>
