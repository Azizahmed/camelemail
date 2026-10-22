<script lang="ts">
  // The phone's home: the mail list with its header, the floating bar of
  // pinned views, the compose button and multi-select.
  import { Archive, ChevronsUpDown, Clock3, FolderInput, Mail, MailOpen, Menu, MoreVertical, Pencil, PenSquare, Search, ShieldAlert, Star, Trash2, X } from "@lucide/svelte";
  import { api } from "../lib/api";
  import { t } from "../lib/i18n.svelte";
  import {
    accountName,
    account,
    act,
    compose,
    key,
    loadMore,
    mail,
    refOf,
    selection,
    snooze,
    toggleRead,
    toggleStar,
    trashOrRestore,
    openThread,
  } from "../lib/mail.svelte";
  import { L, type ThreadRef, type ThreadSummary } from "../lib/types";
  import { toast } from "../lib/ui.svelte";
  import MoveSheet from "./MoveSheet.svelte";
  import { onLeave, push, back, type Screen } from "./nav.svelte";
  import Sheet from "./Sheet.svelte";
  import SnoozeSheet from "./SnoozeSheet.svelte";
  import ThreadRow from "./ThreadRow.svelte";
  import { currentView, openView, pins, viewIcon, viewName, type SwipeAction } from "./views.svelte";

  let { onsettings }: { onsettings: (section?: string) => void } = $props();

  const view = $derived(currentView());
  const TitleIcon = $derived(viewIcon(view));
  const selecting = $derived(selection.size > 0);
  const selectedRefs = $derived(mail.threads.filter((t) => selection.has(key(t))).map(refOf));
  const anyUnread = $derived(mail.threads.some((t) => selection.has(key(t)) && t.unreadCount > 0));
  const allStarred = $derived(selectedRefs.length > 0 && mail.threads.filter((t) => selection.has(key(t))).every((t) => t.isStarred));
  const inTrash = $derived(mail.view.label === L.TRASH);
  const subtitle = $derived(mail.accounts.length > 1 && mail.view.accountId ? accountName(account(mail.view.accountId)) : "");

  let pinMenu = $state(false);
  let pinSheet: Sheet | undefined = $state();
  let selSheet: Sheet | undefined = $state();
  let selMenu = $state(false);
  let snoozeOpen = $state(false);
  let moveOpen = $state(false);
  /** What the snooze/move sheets act on. */
  let target = $state.raw<ThreadRef[]>([]);
  let selEntry: Screen | null = null;
  let scroller: HTMLDivElement | undefined = $state();

  // Back clears a selection.
  $effect(() => {
    if (selecting && !selEntry) {
      const s: Screen = { name: "sheet", id: "selection" };
      selEntry = s;
      push(s);
      onLeave(s, () => {
        selEntry = null;
        selection.clear();
      });
    } else if (!selecting && selEntry) {
      void back();
    }
  });

  // A new view starts at the top.
  $effect(() => {
    void view;
    void mail.view.accountId;
    scroller?.scrollTo({ top: 0 });
  });

  function toggleSelect(t: ThreadSummary) {
    const k = key(t);
    if (selection.has(k)) selection.delete(k);
    else selection.add(k);
  }

  function swipe(t: ThreadSummary, a: SwipeAction) {
    const refs = [refOf(t)];
    switch (a) {
      case "star":
        void toggleStar(refs);
        break;
      case "trash":
        void (inTrash ? act({ type: "deleteForever" }, refs) : trashOrRestore(refs));
        break;
      case "archive":
        if (mail.view.label === L.ARCHIVE) void act({ type: "moveToInbox", from: L.ARCHIVE }, refs);
        else void act({ type: "archive" }, refs);
        break;
      case "read":
        void toggleRead(refs);
        break;
      case "snooze":
        target = refs;
        snoozeOpen = true;
        break;
    }
  }

  function onScroll() {
    if (!scroller) return;
    if (scroller.scrollTop + scroller.clientHeight > scroller.scrollHeight - 600) void loadMore();
  }

  // Pull down at the top to check for new mail.
  let pullStart: number | null = null;
  let pull = $state(0);
  function touchStart(e: TouchEvent) {
    pullStart = scroller && scroller.scrollTop <= 0 ? e.touches[0].clientY : null;
  }
  function touchMove(e: TouchEvent) {
    if (pullStart === null) return;
    pull = Math.max(0, Math.min(110, (e.touches[0].clientY - pullStart) * 0.5));
  }
  function touchEnd() {
    if (pull >= 70) {
      void api.syncNow(mail.view.accountId);
      toast(t("m.home.checking"));
    }
    pull = 0;
    pullStart = null;
  }

  async function pinPick(id: string) {
    await pinSheet?.close();
    go(id);
  }

  /** From the selection's menu to another sheet. */
  async function then(open: () => void) {
    target = selectedRefs;
    await selSheet?.close();
    open();
  }

  function go(id: string) {
    openView(id);
  }

  async function selAction(fn: () => unknown) {
    const refs = selectedRefs;
    selection.clear();
    if (refs.length) await fn();
  }
</script>

<div class="home">
  {#if selecting}
    <header class="m-topbar sel">
      <button class="m-icon-btn" aria-label={t("m.home.clearSelection")} onclick={() => selection.clear()}><X size={20} strokeWidth={1.6} /></button>
      <h1>{selection.size}</h1>
      <button class="m-icon-btn" aria-label={t("m.home.archive")} onclick={() => selAction(() => act({ type: "archive" }, selectedRefs))}><Archive size={18} strokeWidth={1.6} /></button>
      <button class="m-icon-btn" aria-label={inTrash ? t("m.home.deleteForever") : t("m.home.delete")} onclick={() => { const r = selectedRefs; void selAction(() => (inTrash ? act({ type: "deleteForever" }, r) : trashOrRestore(r))); }}><Trash2 size={18} strokeWidth={1.6} /></button>
      <button class="m-icon-btn" aria-label={anyUnread ? t("m.home.markRead") : t("m.home.markUnread")} onclick={() => { const r = selectedRefs; const u = anyUnread; void selAction(() => act({ type: u ? "markRead" : "markUnread" }, r)); }}>
        {#if anyUnread}<MailOpen size={18} strokeWidth={1.6} />{:else}<Mail size={18} strokeWidth={1.6} />{/if}
      </button>
      <button class="m-icon-btn" aria-label={t("m.home.more")} onclick={() => (selMenu = true)}><MoreVertical size={18} strokeWidth={1.6} /></button>
    </header>
  {:else}
    <header class="m-topbar main">
      <button class="m-icon-btn" aria-label={t("m.home.menu")} onclick={() => push({ name: "drawer" })}><Menu size={21} strokeWidth={1.5} /></button>
      <div class="title">
        <span class="name"><TitleIcon size={18} strokeWidth={1.6} /> {viewName(view)}</span>
        {#if subtitle}<small>{subtitle}</small>{/if}
      </div>
      <button class="m-icon-btn" aria-label={t("m.home.search")} onclick={() => push({ name: "search" })}><Search size={20} strokeWidth={1.6} /></button>
    </header>
  {/if}

  {#if !mail.online}
    <div class="offline">{t("m.home.offline")}</div>
  {/if}

  <div
    class="list"
    role="list"
    bind:this={scroller}
    onscroll={onScroll}
    ontouchstart={touchStart}
    ontouchmove={touchMove}
    ontouchend={touchEnd}
  >
    {#if pull > 0}
      <div class="pull" style:height="{pull}px"><span class="spinner" class:ready={pull >= 70}></span></div>
    {/if}
    {#if mail.accounts.length === 0}
      <div class="m-empty">
        <p>{t("m.home.noMailbox")}</p>
        <button class="m-primary" onclick={() => onsettings("accounts")}>{t("m.home.addMailbox")}</button>
      </div>
    {:else if mail.loading && mail.threads.length === 0}
      <div class="m-empty"><span class="spinner"></span></div>
    {:else if mail.threads.length === 0}
      <div class="m-empty">{mail.view.label === L.INBOX ? t("m.home.caughtUp") : t("m.home.nothing")}</div>
    {:else}
      {#each mail.threads as t (key(t))}
        <ThreadRow
          thread={t}
          {selecting}
          selected={selection.has(key(t))}
          onopen={() => { openThread(t); push({ name: "thread" }); }}
          onselect={() => toggleSelect(t)}
          onswipe={(a) => swipe(t, a)}
        />
      {/each}
      {#if mail.loadingMore}
        <div class="more"><span class="spinner"></span></div>
      {/if}
      <div class="tail"></div>
    {/if}
  </div>

  {#if !selecting}
    <nav class="bar" aria-label={t("m.home.pinned")}>
      {#each pins() as id (id)}
        {@const Icon = viewIcon(id)}
        <button class="pin" class:on={view === id} aria-label={viewName(id)} onclick={() => go(id)}><Icon size={19} strokeWidth={1.5} /></button>
      {/each}
      <button class="pin" aria-label={t("m.home.moreViews")} onclick={() => (pinMenu = true)}><ChevronsUpDown size={19} strokeWidth={1.5} /></button>
    </nav>
    <button class="fab" aria-label={t("m.home.compose")} onclick={() => compose()}><PenSquare size={21} strokeWidth={1.6} /></button>
  {/if}
</div>

<Sheet bind:this={pinSheet} bind:open={pinMenu} menu>
  {#each pins() as id (id)}
    {@const Icon = viewIcon(id)}
    <button class="m-sheet-item" class:on={view === id} onclick={() => pinPick(id)}><Icon size={20} strokeWidth={1.5} /> {viewName(id)}</button>
  {/each}
  <div class="m-sheet-sep"></div>
  <button class="m-sheet-item" onclick={async () => { await pinSheet?.close(); onsettings("pins"); }}><Pencil size={18} strokeWidth={1.5} /> {t("m.home.editPins")}</button>
</Sheet>

<Sheet bind:this={selSheet} bind:open={selMenu}>
  <button class="m-sheet-item" onclick={() => { const r = selectedRefs; selMenu = false; void selAction(() => toggleStar(r)); }}><Star size={18} strokeWidth={1.6} /> {allStarred ? t("m.home.unstar") : t("mset.action.star")}</button>
  <button class="m-sheet-item" onclick={() => then(() => (snoozeOpen = true))}><Clock3 size={18} strokeWidth={1.6} /> {t("mset.action.snooze")}</button>
  {#if new Set(selectedRefs.map((r) => r.accountId)).size === 1}
    <button class="m-sheet-item" onclick={() => then(() => (moveOpen = true))}><FolderInput size={18} strokeWidth={1.6} /> {t("m.home.moveTo")}</button>
  {/if}
  <button class="m-sheet-item" onclick={() => { const r = selectedRefs; selMenu = false; void selAction(() => act({ type: mail.view.label === L.SPAM ? "notSpam" : "spam" }, r)); }}><ShieldAlert size={18} strokeWidth={1.6} /> {mail.view.label === L.SPAM ? t("m.home.notSpam") : t("m.home.markSpam")}</button>
</Sheet>

<SnoozeSheet bind:open={snoozeOpen} onpick={(at) => { selection.clear(); void snooze(at, target); }} />
{#if target[0]}
  <MoveSheet
    bind:open={moveOpen}
    accountId={target[0].accountId}
    current={mail.view.label}
    onpick={(to) => { selection.clear(); void act({ type: "move", from: mail.view.label, to }, target); }}
  />
{/if}

<style>
  .home {
    position: fixed;
    inset: 0;
    display: flex;
    flex-direction: column;
    background: var(--m-bg);
    color: var(--m-text);
  }
  .main .title {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
  }
  .main .name {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    max-width: 100%;
    font-size: 19px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .main small {
    font-size: 11px;
    color: var(--m-text-2);
  }
  .sel h1 {
    font-size: 17px;
  }
  .offline {
    padding: 7px var(--m-pad);
    font-size: 11px;
    background: var(--m-surface-2);
    color: var(--m-text-2);
  }
  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    border-top: 1px solid var(--m-separator);
  }
  .pull {
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: hidden;
  }
  .pull .spinner:not(.ready) {
    animation-play-state: paused;
    opacity: 0.5;
  }
  .more {
    display: flex;
    justify-content: center;
    padding: 15px;
  }
  .tail {
    height: 90px;
  }
  .bar {
    position: fixed;
    inset-inline-start: 13px;
    bottom: 15px;
    display: flex;
    align-items: center;
    gap: 2px;
    max-width: calc(100vw - 84px);
    padding: 4px;
    border-radius: 28px;
    background: var(--m-surface);
    box-shadow: var(--m-shadow);
  }
  .pin {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 1 1 50px;
    width: 50px;
    min-width: 34px;
    height: 36px;
    border: 0;
    border-radius: 18px;
    background: transparent;
    color: var(--m-icon);
  }
  .pin.on {
    background: var(--m-pill);
  }
  .fab {
    position: fixed;
    inset-inline-end: 13px;
    bottom: 13px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 48px;
    height: 48px;
    border: 0;
    border-radius: 50%;
    background: var(--m-fab);
    color: #fff;
    box-shadow: var(--m-shadow);
  }
  .fab:active {
    filter: brightness(1.15);
  }
</style>
