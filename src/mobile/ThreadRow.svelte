<script lang="ts">
  // One thread in the phone list. Swipe right or left (short or long) for the
  // actions chosen in Settings → Swipe actions; long-press to select.
  import { Archive, Check, Clock3, MailOpen, Paperclip, Star, Trash2 } from "@lucide/svelte";
  import { subjectDir } from "../lib/bidi";
  import { listDate } from "../lib/format";
  import { t as tr, type MessageKey } from "../lib/i18n.svelte";
  import { account, mail } from "../lib/mail.svelte";
  import type { ThreadSummary } from "../lib/types";
  import { swipeAction, SWIPE_ACTIONS, type SwipeAction } from "./views.svelte";

  let {
    thread: t,
    selecting,
    selected,
    onopen,
    onselect,
    onswipe,
  }: {
    thread: ThreadSummary;
    selecting: boolean;
    selected: boolean;
    onopen: () => void;
    onselect: () => void;
    onswipe: (a: SwipeAction) => void;
  } = $props();

  const ICONS = { star: Star, trash: Trash2, archive: Archive, read: MailOpen, snooze: Clock3, none: Check };

  const unread = $derived(t.unreadCount > 0);
  const sender = $derived(t.participants.length ? t.participants.slice(0, 3).join(", ") : t.senderEmail || tr("m.row.noSender"));
  const acc = $derived(mail.accounts.length > 1 && mail.view.accountId === null ? account(t.accountId) : undefined);

  let el: HTMLDivElement | undefined = $state();
  let dx = $state(0);
  let dragging = $state(false);
  let leaving = $state(false);
  let start: { x: number; y: number; id: number } | null = null;
  let horizontal: boolean | null = null;
  let pressTimer: ReturnType<typeof setTimeout> | undefined;
  let pressed = false;

  const width = () => el?.offsetWidth ?? 400;
  /** Past this share of the width a swipe is long. */
  const LONG = 0.55;
  const SHORT = 0.18;

  const gesture = $derived.by(() => {
    const f = dx / width();
    if (f >= LONG) return swipeAction("longRight");
    if (f >= SHORT) return swipeAction("shortRight");
    if (f <= -LONG) return swipeAction("longLeft");
    if (f <= -SHORT) return swipeAction("shortLeft");
    return null;
  });
  const shown = $derived(dx >= 0 ? (dx / width() >= LONG ? swipeAction("longRight") : swipeAction("shortRight")) : dx / width() <= -LONG ? swipeAction("longLeft") : swipeAction("shortLeft"));
  const meta = $derived(SWIPE_ACTIONS.find((a) => a.id === shown)!);
  const Icon = $derived(ICONS[shown]);
  const label = $derived(
    shown === "star" && t.isStarred ? tr("m.swipe.unstar") : shown === "read" ? tr(unread ? "m.swipe.read" : "m.swipe.unread") : tr(`mset.action.${shown}` as MessageKey),
  );

  function down(e: PointerEvent) {
    if (e.pointerType === "mouse" && e.button !== 0) return;
    start = { x: e.clientX, y: e.clientY, id: e.pointerId };
    horizontal = null;
    pressed = false;
    clearTimeout(pressTimer);
    pressTimer = setTimeout(() => {
      if (horizontal === null) {
        pressed = true;
        navigator.vibrate?.(15);
        onselect();
      }
    }, 450);
  }

  function move(e: PointerEvent) {
    if (!start || e.pointerId !== start.id) return;
    const mx = e.clientX - start.x;
    const my = e.clientY - start.y;
    if (horizontal === null) {
      if (Math.abs(mx) < 8 && Math.abs(my) < 8) return;
      clearTimeout(pressTimer);
      horizontal = !selecting && Math.abs(mx) > Math.abs(my) * 1.2;
      if (horizontal) {
        dragging = true;
        el?.setPointerCapture(e.pointerId);
      }
    }
    if (horizontal) dx = mx;
  }

  function up(e: PointerEvent) {
    clearTimeout(pressTimer);
    if (!start || e.pointerId !== start.id) return;
    start = null;
    if (!horizontal) {
      dragging = false;
      return;
    }
    dragging = false;
    const action = gesture;
    if (!action || action === "none") {
      dx = 0;
      return;
    }
    // Rows that leave the list slide out; the others spring back.
    const removes = action === "archive" || action === "trash" || action === "snooze";
    if (removes && action !== "snooze") {
      leaving = true;
      dx = dx > 0 ? width() : -width();
      setTimeout(() => onswipe(action), 160);
    } else {
      dx = 0;
      onswipe(action);
    }
  }

  function cancel() {
    clearTimeout(pressTimer);
    start = null;
    dragging = false;
    dx = 0;
  }

  function click() {
    if (pressed) {
      pressed = false;
      return;
    }
    if (dx !== 0) return;
    if (selecting) onselect();
    else onopen();
  }
</script>

<div class="wrap" class:leaving>
  {#if dx !== 0}
    <div class="action" class:left={dx < 0} style:background={meta.color}>
      <span class="what"><Icon size={20} strokeWidth={1.6} /> {label}</span>
    </div>
  {/if}
  <div
    bind:this={el}
    class="row"
    class:unread
    class:selected
    class:dragging
    style:transform={dx ? `translateX(${dx}px)` : undefined}
    onpointerdown={down}
    onpointermove={move}
    onpointerup={up}
    onpointercancel={cancel}
    onclick={click}
    oncontextmenu={(e) => e.preventDefault()}
    role="button"
    tabindex="0"
    onkeydown={(e) => e.key === "Enter" && onopen()}
  >
    {#if selecting}
      <span class="check" class:on={selected}>{#if selected}<Check size={13} strokeWidth={2.4} />{/if}</span>
    {/if}
    <div class="body">
      <div class="line">
        {#if unread}<span class="dot"></span>{/if}
        {#if acc}<span class="acc" style:background={acc.color} title={acc.email}></span>{/if}
        <span class="from" dir="auto">{sender}</span>
        {#if t.messageCount > 1}<span class="count">({t.messageCount})</span>{/if}
        {#if t.hasDraft}<span class="draft">{tr("m.row.draft")}</span>{/if}
        <span class="date">{listDate(t.date, mail.clock)}</span>
      </div>
      <div class="line">
        <span class="subject" dir={subjectDir(t.subject)}>{t.subject || tr("m.row.noSubject")}</span>
        {#if t.isStarred}<Star size={13} class="star" fill="currentColor" />{/if}
      </div>
      <div class="line">
        <span class="snippet" dir="auto">{t.snippet}</span>
        {#if t.hasAttachments}<Paperclip size={15} class="clip" />{/if}
      </div>
    </div>
  </div>
</div>

<style>
  .wrap {
    position: relative;
    overflow: hidden;
    border-bottom: 1px solid var(--m-separator);
    transition: max-height 0.2s ease;
    max-height: 164px;
  }
  .wrap.leaving {
    max-height: 0;
    transition-delay: 0.12s;
  }
  .action {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    padding: 0 21px;
    color: #fff;
    font-size: 16px;
  }
  .action.left {
    justify-content: flex-end;
  }
  .what {
    display: inline-flex;
    align-items: center;
    gap: 11px;
  }
  .row {
    position: relative;
    display: flex;
    align-items: center;
    gap: 11px;
    padding: 11px var(--m-pad) 12px;
    background: var(--m-bg);
    color: var(--m-text-read);
    touch-action: pan-y;
    transition: transform 0.18s ease;
  }
  .row.dragging {
    transition: none;
  }
  .row:active:not(.dragging) {
    background: var(--m-pill);
  }
  .row.unread {
    color: var(--m-text);
  }
  .row.selected {
    background: var(--m-accent-soft);
  }
  .body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .line {
    display: flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
  }
  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    margin-inline-end: 3px;
    border-radius: 50%;
    background: var(--m-unread-dot);
  }
  .acc {
    flex: none;
    width: 3px;
    height: 13px;
    border-radius: 2px;
  }
  .from {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 16px;
    line-height: 20px;
  }
  .count {
    flex: none;
    font-size: 13px;
    color: var(--m-text-2);
  }
  .draft {
    flex: none;
    font-size: 11px;
    color: var(--draft-text);
  }
  .date {
    flex: none;
    margin-inline-start: auto;
    padding-inline-start: 8px;
    font-size: 12px;
    color: var(--m-text-2);
  }
  .unread .date {
    color: var(--m-text);
  }
  .subject {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 16px;
    line-height: 20px;
  }
  .snippet {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 13px;
    line-height: 18px;
    color: var(--m-text-3);
  }
  .unread .snippet {
    color: var(--m-text-2);
  }
  .row :global(.star) {
    flex: none;
    color: var(--m-swipe-star);
  }
  .row :global(.clip) {
    flex: none;
    color: var(--m-text-2);
  }
  .check {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 21px;
    height: 21px;
    border-radius: 50%;
    border: 2px solid var(--m-text-3);
    color: #fff;
  }
  .check.on {
    border-color: var(--m-accent);
    background: var(--m-accent);
  }
</style>
