<script lang="ts">
  // Settings › Swipe actions: what each swipe on a list row does. A preview
  // shows the row slid aside over the action's colour, as in the list.
  import { Archive, Ban, Clock3, MailOpen, Star, Trash2 } from "@lucide/svelte";
  import { hasMessage, t } from "../../lib/i18n.svelte";
  import Sheet from "../Sheet.svelte";
  import { setSwipe, SWIPE_ACTIONS, SWIPE_GESTURES, swipeAction, type Icon, type SwipeAction, type SwipeGesture } from "../views.svelte";

  const ICONS: Record<SwipeAction, Icon> = { star: Star, trash: Trash2, archive: Archive, read: MailOpen, snooze: Clock3, none: Ban };

  const gestureName = (g: SwipeGesture) => {
    const k = `mset.gesture.${g}`;
    return hasMessage(k) ? t(k) : (SWIPE_GESTURES.find((x) => x.id === g)?.name ?? g);
  };
  const actionName = (a: SwipeAction) => {
    const k = `mset.action.${a}`;
    return hasMessage(k) ? t(k) : (SWIPE_ACTIONS.find((x) => x.id === a)?.name ?? a);
  };
  const colorOf = (a: SwipeAction) => SWIPE_ACTIONS.find((x) => x.id === a)?.color ?? "var(--m-surface-2)";

  let editing = $state<SwipeGesture | null>(null);
  let open = $state(false);
  let sheet: Sheet | undefined = $state();

  function edit(g: SwipeGesture) {
    editing = g;
    open = true;
  }

  async function pick(a: SwipeAction) {
    if (editing) setSwipe(editing, a);
    await sheet?.close();
  }
</script>

<p class="ms-intro">{t("mset.swipeIntro")}</p>

{#each SWIPE_GESTURES as g (g.id)}
  {@const a = swipeAction(g.id)}
  {@const ActionIcon = ICONS[a]}
  {@const right = g.id.endsWith("Right")}
  {@const long = g.id.startsWith("long")}
  <div class="m-card gesture">
    <button class="m-row" onclick={() => edit(g.id)}>
      <span class="m-row-text">
        <span class="ms-row-title">{gestureName(g.id)}</span>
        <small>{actionName(a)}</small>
      </span>
    </button>
    <!-- Physical directions: a swipe right uncovers the left edge. -->
    <div class="preview" dir="ltr" style:background={colorOf(a)} aria-hidden="true">
      <span class="pv-icon" class:right={!right} class:none={a === "none"}><ActionIcon size={16} strokeWidth={2} /></span>
      <span class="pv-row" style:transform="translateX({right ? '' : '-'}{long ? 62 : 30}%)">
        <span class="pv-line"></span>
        <span class="pv-line short"></span>
      </span>
    </div>
  </div>
{/each}
<div class="ms-end"></div>

<Sheet bind:this={sheet} bind:open title={editing ? gestureName(editing) : ""}>
  {#if editing}
    {@const current = swipeAction(editing)}
    {#each SWIPE_ACTIONS as x (x.id)}
      {@const ActionIcon = ICONS[x.id]}
      <button class="m-sheet-item" class:on={current === x.id} onclick={() => pick(x.id)}>
        <span class="chip" style:background={x.color} class:none={x.id === "none"}><ActionIcon size={15} strokeWidth={2} /></span>
        {actionName(x.id)}
      </button>
    {/each}
  {/if}
</Sheet>

<style>
  .gesture {
    padding-bottom: 13px;
  }
  .preview {
    position: relative;
    height: 43px;
    margin: 0 16px;
    border-radius: 11px;
    overflow: hidden;
  }
  .pv-icon {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 13px;
    display: flex;
    align-items: center;
    color: #fff;
  }
  .pv-icon.right {
    left: auto;
    right: 13px;
  }
  .pv-icon.none {
    color: var(--m-text-3);
  }
  .pv-row {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 7px;
    padding: 0 13px;
    border-radius: 11px;
    background: var(--m-surface);
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.12);
  }
  .pv-line {
    height: 7px;
    width: 70%;
    border-radius: 3px;
    background: var(--m-pill);
  }
  .pv-line.short {
    width: 45%;
  }
  .chip {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    color: #fff;
  }
  .chip.none {
    color: var(--m-text-2);
  }
</style>
