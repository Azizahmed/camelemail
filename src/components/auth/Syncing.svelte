<!-- "Setting up your mailboxes": the first sync after signing in. -->
<script lang="ts">
  import { cloud, signOut } from "../../lib/cloud.svelte";
  import { t } from "../../lib/i18n.svelte";
  import AuthScreen from "./AuthScreen.svelte";

  const progress = $derived(cloud.status?.progress ?? null);
  const fraction = $derived(progress && progress.total > 0 ? Math.min(1, progress.done / progress.total) : null);
</script>

<AuthScreen title={t("sync.title")} lead={t("sync.body")}>
  <div
    class="bar"
    class:indeterminate={fraction === null}
    role="progressbar"
    aria-label={t("sync.title")}
    aria-valuemin={0}
    aria-valuemax={progress?.total ?? undefined}
    aria-valuenow={fraction === null ? undefined : progress!.done}
  >
    <div class="fill" style:width={fraction === null ? null : `${fraction * 100}%`}></div>
  </div>
  {#if progress}
    <div class="detail">
      <span class="label" dir="auto">{progress.label}</span>
      {#if progress.total > 0}<span class="count">{t("sync.progress", { done: progress.done, total: progress.total })}</span>{/if}
    </div>
  {/if}
  {#if cloud.status?.offline}<div class="au-info" role="status">{t("sync.offline")}</div>{/if}

  {#snippet footer()}
    <button type="button" class="au-link" onclick={() => void signOut().catch(() => {})}>{t("common.signOut")}</button>
  {/snippet}
</AuthScreen>

<style>
  .bar {
    position: relative;
    height: 6px;
    border-radius: 3px;
    background: var(--surface-active);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    border-radius: 3px;
    background: var(--accent);
    transition: width var(--slow);
  }
  .indeterminate .fill {
    position: absolute;
    inset-inline-start: 0;
    width: 35%;
    animation: slide 1.3s ease-in-out infinite;
  }
  :global([dir="rtl"]) .indeterminate .fill {
    animation-name: slide-rtl;
  }
  @keyframes slide {
    from {
      transform: translateX(-100%);
    }
    to {
      transform: translateX(300%);
    }
  }
  @keyframes slide-rtl {
    from {
      transform: translateX(100%);
    }
    to {
      transform: translateX(-300%);
    }
  }
  .detail {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    flex: none;
  }
  @media (prefers-reduced-motion: reduce) {
    .indeterminate .fill {
      animation: none;
      width: 100%;
      opacity: 0.4;
    }
  }
</style>
