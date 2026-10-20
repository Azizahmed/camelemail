<script lang="ts">
  import { dismissToast, toasts } from "../lib/ui.svelte";
  import { sending, undoSend } from "../lib/mail.svelte";

  let now = $state(Date.now());
  const counting = $derived(sending.items.some((i) => i.status === "pending"));
  $effect(() => {
    // Tick only while an undoable send counts down.
    if (!counting) return;
    now = Date.now();
    const t = setInterval(() => (now = Date.now()), 250);
    return () => clearInterval(t);
  });

  const pending = $derived(sending.items.find((i) => i.status === "pending" && i.sendAt > now - 1000));
  const sendingNow = $derived(sending.items.find((i) => i.status === "sending"));
</script>

{#if pending}
  {@const total = Math.max(1, pending.sendAt - pending.createdAt)}
  {@const left = Math.max(0, pending.sendAt - now)}
  <div class="send-pill" role="status">
    <div class="fill" style:transform="scaleX({1 - left / total})"></div>
    <span>Sending ({Math.ceil(left / 1000)}s)…</span>
    <button class="undo" onclick={() => undoSend(pending)}>Undo</button>
  </div>
{:else if sendingNow}
  <div class="send-pill" role="status"><span class="spinner"></span><span>Sending…</span></div>
{:else if toasts.current}
  {@const t = toasts.current}
  <div class="toast {t.kind}" role="status">
    <span class="msg selectable" dir="auto">{t.message}</span>
    {#if t.action}
      <button
        class="action"
        onclick={() => {
          const a = t.action!;
          dismissToast();
          void a.run();
        }}>{t.action.label}</button
      >
    {/if}
  </div>
{/if}

<style>
  .toast,
  .send-pill {
    position: fixed;
    bottom: 24px;
    left: 50%;
    transform: translateX(-50%);
    background: var(--surface-elevated);
    border: 1px solid var(--separator);
    box-shadow: var(--shadow-dropdown);
    z-index: 500;
    display: flex;
    align-items: center;
    gap: 12px;
    font-size: 13px;
    animation: fade 0.2s ease;
    max-width: min(560px, 90vw);
  }
  .toast {
    border-radius: 8px;
    padding: 10px 14px 10px 18px;
  }
  .toast.error .msg {
    color: var(--indicator-error);
  }
  .toast.success .msg {
    color: var(--content);
  }
  .action {
    border: none;
    background: var(--accent);
    color: #fff;
    border-radius: 4px;
    padding: 6px 12px;
    font-size: 12px;
    font-weight: 500;
    flex: none;
  }
  .send-pill {
    border-radius: 20px;
    padding: 8px 10px 8px 16px;
    min-width: 190px;
    overflow: hidden;
  }
  .fill {
    position: absolute;
    inset: 0;
    background: var(--accent);
    opacity: 0.22;
    transform-origin: left;
    transition: transform 0.25s linear;
  }
  .send-pill span {
    position: relative;
  }
  .undo {
    position: relative;
    margin-left: auto;
    border: 1px solid var(--separator);
    background: transparent;
    border-radius: 12px;
    padding: 3px 10px;
    font-size: 12px;
    font-weight: 500;
  }
  .undo:hover {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
</style>
