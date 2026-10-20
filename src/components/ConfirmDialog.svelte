<script lang="ts">
  import { dialogs } from "../lib/ui.svelte";

  const req = $derived(dialogs.confirm);
  let modal: HTMLElement | undefined = $state();

  function onkeydown(e: KeyboardEvent) {
    if (!dialogs.confirm) return;
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      dialogs.confirm.resolve("cancel");
    } else if (e.key === "Enter") {
      const t = e.target as HTMLElement | null;
      if (t?.tagName === "BUTTON" && t.dataset.role !== "confirm") return;
      e.preventDefault();
      e.stopPropagation();
      dialogs.confirm.resolve("confirm");
    } else if (e.key === "Tab" && modal) {
      // Keep Tab inside the dialog.
      const items = [...modal.querySelectorAll<HTMLElement>("button:not(:disabled)")];
      if (!items.length) return;
      const at = items.indexOf(document.activeElement as HTMLElement);
      e.preventDefault();
      e.stopPropagation();
      const next = at < 0 ? 0 : (at + (e.shiftKey ? -1 : 1) + items.length) % items.length;
      items[next].focus();
    }
  }

  function focusConfirm(node: HTMLElement) {
    // After restoreFocus has noted what had focus.
    queueMicrotask(() => node.focus());
  }

  /** Give focus back to what had it once the dialog (or a run of dialogs) closes. */
  function restoreFocus(_node: HTMLElement) {
    const before = document.activeElement as HTMLElement | null;
    return {
      destroy() {
        if (before?.isConnected) before.focus();
      },
    };
  }
</script>

<svelte:window onkeydowncapture={onkeydown} />

{#if req}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="overlay confirm-overlay" onclick={(e) => e.target === e.currentTarget && req.resolve("cancel")}>
    <div class="modal confirm" bind:this={modal} use:restoreFocus role="alertdialog" aria-modal="true" aria-labelledby="confirm-title" aria-describedby="confirm-msg">
      <div class="modal-header" id="confirm-title">{req.title}</div>
      <div class="modal-body selectable" id="confirm-msg">{req.message}</div>
      <div class="modal-actions">
        <button class="btn" onclick={() => req.resolve("cancel")}>{req.cancelLabel}</button>
        {#if req.altLabel}
          <button class="btn" onclick={() => req.resolve("alt")}>{req.altLabel}</button>
        {/if}
        <button
          class="btn"
          class:primary={!req.danger}
          class:danger={req.danger}
          data-role="confirm"
          use:focusConfirm
          onclick={() => req.resolve("confirm")}
        >
          {req.confirmLabel}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .confirm-overlay {
    z-index: 600;
  }
  .confirm {
    width: min(92vw, 400px);
  }
  .btn.danger {
    background: var(--indicator-error);
    border-color: transparent;
    color: #fff;
  }
  .btn.danger:hover:not(:disabled) {
    background: var(--indicator-error);
    filter: brightness(1.08);
  }
</style>
