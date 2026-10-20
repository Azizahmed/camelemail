<script lang="ts">
  import { tick } from "svelte";
  import { subjectDir } from "../lib/bidi";
  import { Archive, Clock3, FolderInput, Paperclip, RotateCcw, Square, SquareCheck, Star, Trash2, Inbox } from "@lucide/svelte";
  import { api } from "../lib/api";
  import { titled } from "../lib/shortcuts.svelte";
  import { absoluteDate, listDate, wakeDate } from "../lib/format";
  import { BUCKET_NAMES, L, type ThreadSummary } from "../lib/types";
  import {
    account,
    act,
    key,
    labelName,
    loadMore,
    mail,
    openThread,
    refOf,
    refreshThreads,
    selection,
    smartActive,
    snooze,
    toggleStar,
    trashOrRestore,
    ownEmailSet,
    scheduledCount,
  } from "../lib/mail.svelte";
  import { confirmDialog, toast, toastError } from "../lib/ui.svelte";
  import Avatar from "./Avatar.svelte";
  import EmptyState from "./EmptyState.svelte";
  import MoveMenu from "./menus/MoveMenu.svelte";
  import SnoozeMenu from "./menus/SnoozeMenu.svelte";
  import SmartTabs from "./SmartTabs.svelte";

  /** Narrow two-line rows for the split (reading pane) layout. */
  let { compact = false }: { compact?: boolean } = $props();

  const showCategory = $derived(smartActive() && mail.smart.tab === "other" && mail.smart.other === null);
  const openKey = $derived(mail.open ? key(mail.open) : "");

  let listEl: HTMLDivElement | undefined = $state();
  let menu = $state<null | { kind: "move" | "snooze"; anchor: DOMRect; thread: ThreadSummary }>(null);
  let absolute = $state<Record<string, boolean>>({});
  let keyboardNav = $state(false);
  let lastClicked = -1;

  const multi = $derived(mail.accounts.length > 1);
  const label = $derived(mail.page === "search" ? "" : mail.view.label);
  const sentView = $derived(label === L.SENT || label === L.DRAFTS);

  export function openMenu(kind: "move" | "snooze") {
    const t = mail.threads[mail.focusedIndex];
    if (!t || !listEl) return;
    const row = listEl.querySelector(`[data-index="${mail.focusedIndex}"]`);
    const r = row?.getBoundingClientRect() ?? listEl.getBoundingClientRect();
    menu = { kind, anchor: new DOMRect(r.right - 40, r.top, 40, r.height), thread: t };
  }

  /** The card's action column fills the card's height. */
  const columnHeight = (rowHeight: number) => Math.max(rowHeight - 8, 56);

  export function setKeyboardNav() {
    keyboardNav = true;
  }

  // The list is hidden, not destroyed, while the calendar shows; put the
  // scroll position back in case hiding reset it.
  let savedTop = 0;
  $effect(() => {
    if (mail.space !== "mail") return;
    const top = savedTop;
    void tick().then(() => {
      if (listEl && top > 0 && listEl.scrollTop === 0) listEl.scrollTop = top;
    });
  });

  $effect(() => {
    // Keep the focused row visible during keyboard navigation (not while the mouse hovers rows).
    if (!keyboardNav) return;
    const i = mail.focusedIndex;
    if (!listEl || i < 0) return;
    listEl.querySelector(`[data-index="${i}"]`)?.scrollIntoView({ block: "nearest" });
  });

  // One set of hover actions, moved over whichever row the pointer is on
  // (rather than five hidden buttons in every row).
  let hover = $state<{ key: string; index: number; top: number; height: number } | null>(null);
  let hoverRow: HTMLElement | null = null;
  /** Where the pointer is over the list (null when it's elsewhere). */
  let pointer: { x: number; y: number } | null = null;
  const hoverThread = $derived.by(() => {
    const t = hover ? mail.threads[hover.index] : undefined;
    return t && key(t) === hover!.key ? t : null;
  });

  function setHover(row: HTMLElement | null) {
    if (row !== hoverRow) {
      hoverRow?.removeAttribute("data-hover");
      row?.setAttribute("data-hover", "");
      hoverRow = row;
    }
    const i = row ? Number(row.dataset.index) : -1;
    const t = mail.threads[i];
    hover = row && t ? { key: key(t), index: i, top: row.offsetTop, height: row.offsetHeight } : null;
  }

  $effect(() => {
    // Rows moved or went away (new mail, archive): the actions go to the row
    // now under the pointer, as hover would (WebKit sends no mouseover when
    // the page moves under a still pointer).
    void mail.threads;
    void tick().then(() => {
      const under = pointer && document.elementsFromPoint(pointer.x, pointer.y).find((el) => el.matches(".row") && listEl?.contains(el));
      setHover((under as HTMLElement | undefined) ?? null);
    });
  });

  function onOver(e: MouseEvent) {
    const target = e.target as Element;
    if (target.closest(".hover-actions")) return;
    const row = target.closest<HTMLElement>(".row");
    if (row !== hoverRow) setHover(row);
    if (!row) return;
    const i = Number(row.dataset.index);
    if (!keyboardNav && target.closest(".content, .card")) mail.focusedIndex = i;
    // The full date only when it is pointed at.
    const date = target.closest<HTMLElement>(".date");
    const t = mail.threads[i];
    if (date && t) date.title = absoluteDate(t.date);
  }

  function onScroll() {
    if (!listEl) return;
    if (mail.space === "mail") savedTop = listEl.scrollTop;
    if (listEl.scrollTop + listEl.clientHeight > listEl.scrollHeight - 400) void loadMore();
  }

  function click(e: MouseEvent, t: ThreadSummary, i: number) {
    if (e.shiftKey && lastClicked >= 0) {
      const [a, b] = [Math.min(lastClicked, i), Math.max(lastClicked, i)];
      for (let j = a; j <= b; j++) selection.add(key(mail.threads[j]));
      return;
    }
    if (e.ctrlKey || e.metaKey || selection.size > 0) {
      toggleSelect(t, i);
      return;
    }
    lastClicked = i;
    if (label === L.DRAFTS && t.hasDraft && t.messageCount === 1) {
      void openDraftThread(t);
      return;
    }
    openThread(t);
  }

  async function openDraftThread(t: ThreadSummary) {
    try {
      const d = await api.editDraftMessage(t.latestMessageId);
      await api.openCompose(d.id);
    } catch (e) {
      toastError(e);
    }
  }

  function toggleSelect(t: ThreadSummary, i: number) {
    const k = key(t);
    if (selection.has(k)) selection.delete(k);
    else selection.add(k);
    lastClicked = i;
  }

  function sender(t: ThreadSummary): string {
    if (sentView) {
      const others = t.participants.filter((p) => p !== "me");
      return others.length ? `To: ${others.join(", ")}` : "me";
    }
    if (t.participants.length <= 2) return t.participants.join(", ");
    return `${t.participants[0]} … ${t.participants.slice(-2).join(", ")}`;
  }

  function avatarName(t: ThreadSummary): string {
    return t.participants.find((p) => p !== "me") ?? t.participants[0] ?? "?";
  }

  const ownEmails = $derived(ownEmailSet());
  const scheduled = $derived(label === L.INBOX ? scheduledCount(mail.view.accountId) : 0);
  /** Colour avatars by address like the thread view, when one other person is in the thread. */
  const avatarEmail = (t: ThreadSummary) =>
    t.senderEmail && !ownEmails.has(t.senderEmail.toLowerCase()) && !sentView && t.participants.filter((p) => p !== "me").length === 1 ? t.senderEmail : "";

  async function emptyFolder() {
    const what = label === L.TRASH ? "Trash" : "Spam";
    const ok = await confirmDialog({
      title: `Empty ${what}?`,
      message: `All messages in ${what} will be permanently deleted. This cannot be undone.`,
      confirmLabel: `Empty ${what}`,
      danger: true,
    });
    if (ok !== "confirm") return;
    const accounts = mail.view.accountId ? [mail.view.accountId] : mail.accounts.map((a) => a.id);
    try {
      for (const a of accounts) await api.emptyFolder(a, label);
      toast(`${what} emptied`);
      await refreshThreads();
    } catch (e) {
      toastError(e);
    }
  }
</script>

<div class="list-wrap" class:compact>
  {#if smartActive()}
    <SmartTabs {compact} />
  {/if}
  {#if (label === L.TRASH || label === L.SPAM) && mail.threads.length > 0}
    <div class="banner">
      <span>Messages in {label === L.TRASH ? "Trash" : "Spam"} stay until you delete them.</span>
      <button class="btn" onclick={emptyFolder}>Empty {label === L.TRASH ? "Trash" : "Spam"}</button>
    </div>
  {/if}

  {#if scheduled > 0}
    <button class="entry" onclick={() => { mail.page = "scheduled"; mail.view = { ...mail.view, label: L.SCHEDULED }; }}>
      <Clock3 size={14} /> Scheduled <span class="muted">{scheduled} scheduled</span>
    </button>
  {/if}

  <!-- Hover only places the row actions; keyboard users have the shortcuts. -->
  <!-- svelte-ignore a11y_mouse_events_have_key_events -->
  <div
    class="list"
    bind:this={listEl}
    onscroll={onScroll}
    onmousemove={(e) => {
      keyboardNav = false;
      pointer = { x: e.clientX, y: e.clientY };
    }}
    onmouseover={onOver}
    onmouseleave={() => {
      pointer = null;
      setHover(null);
    }}
    role="list"
  >
    {#if mail.loading && mail.threads.length === 0}
      <div class="loading"><span class="spinner"></span> Loading…</div>
    {:else if mail.threads.length === 0}
      {#if mail.accounts.some((a) => mail.syncing[a.id]) && label === L.INBOX}
        <div class="loading"><span class="spinner"></span> Syncing emails… they'll appear as they arrive.</div>
      {:else}
        <EmptyState label={label} search={mail.page === "search" ? mail.searchQuery : ""} />
      {/if}
    {:else}
      {#each mail.threads as t, i (key(t))}
        {@const k = key(t)}
        {@const selected = selection.has(k)}
        {@const unread = t.unreadCount > 0}
        {@const acc = account(t.accountId)}
        <div
          class="row"
          class:unread
          class:selected
          class:open={k === openKey}
          class:focused={keyboardNav && i === mail.focusedIndex}
          class:removing={mail.removing.has(k)}
          data-index={i}
          role="listitem"
        >
          {#if compact}
            <!-- Selecting is in the hover column that takes this place. -->
            <span class="lead"><span class="dot" class:show={unread}></span></span>
          {:else}
          <button
            class="lead"
            title={selected ? "Deselect" : "Select"}
            onclick={(e) => {
              e.stopPropagation();
              toggleSelect(t, i);
            }}
          >
            <span class="dot" class:show={unread}></span>
            <span class="check" class:on={selected} class:visible={selection.size > 0}></span>
          </button>
          {/if}
          {#if compact}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <div class="card" role="button" tabindex="-1" onclick={(e) => click(e, t, i)}>
              <Avatar name={avatarName(t)} email={avatarEmail(t)} size={32} ring={multi ? acc?.color : null} />
              <div class="card-main">
                <div class="line1">
                  <span class="names" dir="auto">{sender(t)}</span>
                  {#if t.messageCount > 1}<span class="count">{t.messageCount}</span>{/if}
                  <span class="grow"></span>
                  {#if t.hasAttachments}<Paperclip size={12} class="clip" />{/if}
                  {#if t.isStarred}<Star size={12} class="star-mark" />{/if}
                  <span class="date">
                    {#if t.snoozedUntil}<span class="snoozed"><Clock3 size={11} /> {wakeDate(t.snoozedUntil, mail.clock)}</span>{:else}{listDate(t.date, mail.clock)}{/if}
                  </span>
                </div>
                <div class="line2">
                  {#if t.hasDraft}<span class="draft-tag">Draft</span>{/if}
                  {#if showCategory && t.category !== "primary" && BUCKET_NAMES[t.category]}<span class="label-chip">{BUCKET_NAMES[t.category]}</span>{/if}
                  <span class="subject" dir={subjectDir(t.subject)}>{t.subject || "(No subject)"}</span>
                </div>
                <div class="line3" dir="auto">{t.snippet}</div>
              </div>
            </div>
          {:else}
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <div class="content" role="button" tabindex="-1" onclick={(e) => click(e, t, i)}>
            <Avatar name={avatarName(t)} email={avatarEmail(t)} size={22} ring={multi ? acc?.color : null} />
            <span class="sender">
              <span class="names" dir="auto">{sender(t)}</span>
              {#if t.messageCount > 1}<span class="count">{t.messageCount}</span>{/if}
            </span>
            <span class="preview">
              {#if t.hasDraft}<span class="draft-tag">Draft</span>{/if}
              {#if showCategory && t.category !== "primary" && BUCKET_NAMES[t.category]}<span class="label-chip">{BUCKET_NAMES[t.category]}</span>{/if}
              {#if t.hasAttachments}<Paperclip size={12} class="clip" />{/if}
              {#if mail.page === "search" || label === L.STARRED || label === L.SNOOZED}
                {#each t.labels.filter((l) => l !== L.STARRED && l !== L.INBOX && l !== label) as l (l)}
                  <span class="label-chip">{labelName(l, t.accountId)}</span>
                {/each}
              {/if}
              <span class="subject" dir={subjectDir(t.subject)}>{t.subject || "(No subject)"}</span>
              <span class="snippet">— <bdi>{t.snippet}</bdi></span>
            </span>
            <span class="date">
              {#if t.snoozedUntil}
                <span class="snoozed"><Clock3 size={11} /> {wakeDate(t.snoozedUntil, mail.clock)}</span>
              {:else}
                <span
                  role="button"
                  tabindex="-1"
                  onclick={(e) => { e.stopPropagation(); absolute[k] = !absolute[k]; }}
                  onkeydown={() => {}}
                >{absolute[k] ? absoluteDate(t.date) : listDate(t.date, mail.clock)}</span>
              {/if}
            </span>
          </div>
          {/if}
        </div>
      {/each}
      {#if hover && hoverThread}
        {@const t = hoverThread}
        <div
          class="hover-actions"
          class:in-card={compact}
          role="toolbar"
          tabindex="-1"
          onmousedown={(e) => e.preventDefault()}
          aria-label="Actions for {t.subject || '(No subject)'}"
          style:top="{compact ? Math.max(0, hover.top + (hover.height - columnHeight(hover.height)) / 2) : hover.top}px"
          style:height="{compact ? columnHeight(hover.height) : hover.height}px"
        >
          {#if compact}
            <!-- One column down the card's left edge, where the unread dot is: select, star, snooze, delete. -->
            {@const picked = selection.has(key(t))}
            <button class="icon-btn" class:picked title={titled(picked ? "Deselect" : "Select", "select")} onclick={() => hover && toggleSelect(t, hover.index)}>
              {#if picked}<SquareCheck size={14} />{:else}<Square size={14} />{/if}
            </button>
            <button class="icon-btn" class:starred={t.isStarred} title={titled(t.isStarred ? "Unstar" : "Star", "star")} onclick={() => toggleStar([refOf(t)])}><Star size={14} /></button>
            <button class="icon-btn" title={titled("Snooze", "snooze")} onclick={(e) => (menu = { kind: "snooze", anchor: e.currentTarget.getBoundingClientRect(), thread: t })}><Clock3 size={14} /></button>
            {#if label === L.TRASH || label === L.SPAM}
              <button class="icon-btn" title="Restore to Inbox" onclick={() => act(label === L.SPAM ? { type: "notSpam" } : { type: "moveToInbox", from: L.TRASH }, [refOf(t)])}><RotateCcw size={14} /></button>
            {/if}
            {#if label !== L.TRASH}
              <button class="icon-btn" title={titled("Delete", "trash")} onclick={() => trashOrRestore([refOf(t)])}><Trash2 size={14} /></button>
            {/if}
          {:else}
            {#if label === L.TRASH || label === L.SPAM}
              <button class="icon-btn" title="Restore to Inbox" onclick={() => act(label === L.SPAM ? { type: "notSpam" } : { type: "moveToInbox", from: L.TRASH }, [refOf(t)])}><RotateCcw size={15} /></button>
            {:else if label === L.ARCHIVE}
              <button class="icon-btn" title="Move to Inbox" onclick={() => act({ type: "moveToInbox", from: L.ARCHIVE }, [refOf(t)])}><Inbox size={15} /></button>
            {:else}
              <button
                class="icon-btn"
                title={t.labels.includes(L.INBOX) ? titled("Archive", "archive") : "Already archived"}
                disabled={!t.labels.includes(L.INBOX)}
                onclick={() => act({ type: "archive" }, [refOf(t)])}><Archive size={15} /></button
              >
            {/if}
            <button class="icon-btn" title={titled("Move", "move")} onclick={(e) => (menu = { kind: "move", anchor: e.currentTarget.getBoundingClientRect(), thread: t })}><FolderInput size={15} /></button>
            <button class="icon-btn" title={titled("Snooze", "snooze")} onclick={(e) => (menu = { kind: "snooze", anchor: e.currentTarget.getBoundingClientRect(), thread: t })}><Clock3 size={15} /></button>
            {#if label !== L.TRASH}
              <button class="icon-btn" title={titled("Delete", "trash")} onclick={() => trashOrRestore([refOf(t)])}><Trash2 size={15} /></button>
            {/if}
            <button class="icon-btn" class:starred={t.isStarred} title={titled(t.isStarred ? "Unstar" : "Star", "star")} onclick={() => toggleStar([refOf(t)])}><Star size={15} /></button>
          {/if}
        </div>
      {/if}
      <div class="footer">
        {#if mail.loadingMore}
          <span class="spinner"></span> Loading older emails…
        {:else if mail.hasMore && mail.page === "list"}
          <button class="btn" onclick={() => loadMore()}>Load older emails</button>
        {:else if mail.page === "list"}
          <span class="muted">That's all your mail here</span>
        {/if}
      </div>
    {/if}
  </div>
</div>

{#if menu?.kind === "snooze"}
  {@const t = menu.thread}
  <SnoozeMenu anchor={menu.anchor} onclose={() => (menu = null)} onpick={(at) => { menu = null; snooze(at, [refOf(t)]); }} />
{:else if menu?.kind === "move"}
  {@const t = menu.thread}
  <MoveMenu
    anchor={menu.anchor}
    accountId={t.accountId}
    current={label || null}
    onclose={() => (menu = null)}
    onpick={(to) => { menu = null; act({ type: "move", from: label || null, to }, [refOf(t)]); }}
  />
{/if}

<style>
  .list-wrap {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0;
    width: 100%;
    max-width: 1196px;
    margin: 0 auto;
  }
  .list {
    flex: 1;
    overflow-y: auto;
    padding: 4px 0 40px;
    /* Positions the hover actions over a row. */
    position: relative;
  }
  .row {
    display: flex;
    align-items: center;
    height: 45px;
    gap: 4px;
    transition: opacity 0.15s ease, transform 0.15s ease;
    /* Rows off screen skip layout and paint (long lists after "load more"). */
    content-visibility: auto;
    contain-intrinsic-size: auto 45px;
  }
  .row.removing {
    opacity: 0;
    transform: translateY(-10px);
  }
  .lead {
    width: 24px;
    height: 24px;
    flex: none;
    border: none;
    background: none;
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--unread-indicator);
    opacity: 0;
  }
  .dot.show {
    opacity: 1;
  }
  .check {
    position: absolute;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    border: 1px solid var(--content-muted);
    background: var(--surface);
    opacity: 0;
  }
  .lead:hover .check,
  .check.visible {
    opacity: 1;
  }
  .check.on {
    background: var(--accent);
    border-color: var(--accent);
    opacity: 1;
  }
  .check.on::after {
    content: "";
    position: absolute;
    left: 6px;
    top: 3px;
    width: 4px;
    height: 8px;
    border: solid #fff;
    border-width: 0 2px 2px 0;
    transform: rotate(45deg);
  }
  .content {
    flex: 1;
    min-width: 0;
    height: 100%;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 12px;
    border-bottom: 0.5px solid var(--separator-subtle);
    position: relative;
    font-size: 12px;
    line-height: 16px;
    outline: none;
  }
  .row:global([data-hover]) .content,
  .row.focused .content {
    background: var(--row-hover);
  }
  .row.selected .content {
    background: var(--row-selected-tint);
  }
  .sender {
    width: 172px;
    flex: none;
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    font-weight: 500;
    color: var(--content-subtle);
  }
  .names {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .unread .sender {
    color: var(--content);
    font-weight: 600;
  }
  .count {
    font-size: 11px;
    font-weight: 500;
    background: var(--surface-nav-active);
    color: var(--content-subtle);
    border-radius: 12px;
    padding: 0 6px;
    flex: none;
  }
  .preview {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 6px;
    overflow: hidden;
    white-space: nowrap;
    font-weight: 500;
    color: var(--content-subtle);
  }
  .preview :global(.clip) {
    flex: none;
    color: var(--icon);
  }
  .subject {
    flex: none;
    max-width: 60%;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .unread .subject {
    color: var(--content);
    font-weight: 600;
  }
  .snippet {
    overflow: hidden;
    text-overflow: ellipsis;
    font-weight: 400;
  }
  .draft-tag {
    color: var(--indicator-error);
    font-weight: 600;
    flex: none;
  }
  .label-chip {
    flex: none;
    font-size: 10.5px;
    padding: 0 6px;
    border-radius: 4px;
    background: var(--surface-hover);
    color: var(--content-subtle);
  }
  .date {
    flex: none;
    min-width: 44px;
    text-align: right;
    color: var(--content-subtle);
    white-space: nowrap;
  }
  .unread .date {
    color: var(--content);
    font-weight: 500;
  }
  .snoozed {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--identity-gold);
  }
  .hover-actions {
    position: absolute;
    right: 8px;
    display: flex;
    align-items: center;
    gap: 2px;
    padding-left: 24px;
    background: linear-gradient(90deg, transparent, var(--surface) 22px);
  }
  .list-wrap:not(.compact) .row:global([data-hover]) .date {
    visibility: hidden;
  }
  .starred {
    color: var(--identity-gold) !important;
  }
  .starred :global(svg) {
    fill: currentColor;
  }
  .footer {
    display: flex;
    justify-content: center;
    align-items: center;
    gap: 8px;
    padding: 18px;
    color: var(--content-subtle);
    font-size: 12px;
  }
  .muted {
    color: var(--content-muted);
  }
  .loading {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 18vh 0;
    color: var(--content-subtle);
  }
  .banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin: 6px 12px 4px 36px;
    padding: 8px 12px;
    border-radius: 8px;
    background: var(--surface-hover);
    color: var(--content-subtle);
    font-size: 12px;
  }
  .entry {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 4px 0 0 28px;
    padding: 8px 12px;
    border: none;
    background: none;
    border-bottom: 0.5px solid var(--separator-subtle);
    color: var(--content);
    font-size: 12px;
    font-weight: 500;
    text-align: left;
  }
  .entry:hover {
    background: var(--row-hover);
  }
  .row.open .content {
    background: var(--row-selected-tint);
  }

  /* ---- compact rows (split layout) ---- */
  .list-wrap.compact {
    max-width: none;
  }
  .compact .row {
    height: auto;
    gap: 0;
    contain-intrinsic-size: auto 90px;
  }
  .compact .lead {
    width: 30px;
    align-self: stretch;
  }
  .card {
    flex: 1;
    min-width: 0;
    display: flex;
    gap: 10px;
    padding: 10px 10px 10px 6px;
    border-bottom: 0.5px solid var(--separator-subtle);
    position: relative;
    border-radius: 8px;
    outline: none;
  }
  .row:global([data-hover]) .card,
  .row.focused .card {
    background: var(--surface-hover);
  }
  .row.selected .card,
  .row.open .card {
    background: var(--row-selected-tint);
  }
  .card-main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .line1,
  .line2 {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .line1 .names {
    font-size: 13px;
    font-weight: 500;
    color: var(--content-subtle);
  }
  .unread .line1 .names {
    font-weight: 600;
    color: var(--content);
  }
  .line1 :global(.clip),
  .line1 :global(.star-mark) {
    flex: none;
    color: var(--icon);
  }
  .line1 :global(.star-mark) {
    color: var(--identity-gold);
    fill: currentColor;
  }
  .grow {
    flex: 1;
  }
  .line1 .date {
    min-width: 0;
    font-size: 11.5px;
  }
  .line2 .subject {
    max-width: none;
    font-size: 12.5px;
    color: var(--content-subtle);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .unread .line2 .subject {
    color: var(--content);
    font-weight: 600;
  }
  .line3 {
    font-size: 12px;
    line-height: 16px;
    color: var(--content-muted);
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .hover-actions.in-card {
    right: auto;
    left: 1px;
    width: 28px;
    flex-direction: column;
    justify-content: space-evenly;
    gap: 0;
    padding: 2px 0;
    border-radius: 9px;
    background: var(--surface-elevated);
    border: 0.5px solid var(--separator);
    box-shadow: var(--shadow-panel);
  }
  .hover-actions.in-card .icon-btn {
    width: 24px;
    height: auto;
    flex: 1 1 0;
    max-height: 26px;
    min-height: 18px;
    border-radius: 6px;
  }
  .picked {
    color: var(--accent) !important;
  }
</style>
