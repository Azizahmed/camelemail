<!-- First sign-in on a computer that already has mailboxes: move them up, or start fresh. -->
<script lang="ts">
  import { onMount } from "svelte";
  import { CloudUpload, RotateCcw } from "@lucide/svelte";
  import { api } from "../../lib/api";
  import { refreshCloud, signOut } from "../../lib/cloud.svelte";
  import { errMessage, t } from "../../lib/i18n.svelte";
  import type { MigrationInfo } from "../../lib/types";
  import { confirmDialog } from "../../lib/ui.svelte";
  import AuthScreen from "./AuthScreen.svelte";

  let info = $state<MigrationInfo | null>(null);
  let upload = $state(true);
  let busy = $state(false);
  let error = $state("");

  onMount(() => {
    api.cloudMigration().then((i) => (info = i)).catch((e) => (error = errMessage(e)));
  });

  async function submit(e: Event) {
    e.preventDefault();
    if (busy) return;
    if (!upload) {
      const r = await confirmDialog({
        title: t("migrate.freshTitle"),
        message: t("migrate.freshConfirm"),
        confirmLabel: t("migrate.freshButton"),
        cancelLabel: t("common.cancel"),
        danger: true,
      });
      if (r !== "confirm") return;
    }
    busy = true;
    error = "";
    try {
      await api.cloudMigrate(upload);
      await refreshCloud();
    } catch (err) {
      error = errMessage(err);
    } finally {
      busy = false;
    }
  }
</script>

<AuthScreen title={t("migrate.title")} lead={t("migrate.body")} wide>
  {#if info}
    <ul class="counts">
      <li>{t("migrate.local", { n: info.localAccounts })}</li>
      {#if info.serverAccounts > 0}<li>{t("migrate.server", { n: info.serverAccounts })}</li>{/if}
    </ul>
  {/if}
  <form class="au-form" onsubmit={submit}>
    <div class="options" role="radiogroup" aria-label={t("migrate.title")}>
      <label class="option" class:on={upload}>
        <input type="radio" name="migrate" value="upload" checked={upload} onchange={() => (upload = true)} />
        <span class="icon"><CloudUpload size={18} /></span>
        <span class="text">
          <span class="name">{t("migrate.upload")} <span class="badge">{t("migrate.recommended")}</span></span>
          <span class="desc">{info && info.serverAccounts > 0 ? t("migrate.uploadMerge") : t("migrate.uploadDesc")}</span>
        </span>
      </label>
      <label class="option" class:on={!upload}>
        <input type="radio" name="migrate" value="fresh" checked={!upload} onchange={() => (upload = false)} />
        <span class="icon"><RotateCcw size={18} /></span>
        <span class="text">
          <span class="name">{t("migrate.fresh")}</span>
          <span class="desc">{t("migrate.freshDesc")}</span>
        </span>
      </label>
    </div>
    {#if error}<div class="au-error" role="alert">{error}</div>{/if}
    <button type="submit" class="btn primary block" disabled={busy}>
      {#if busy}<span class="spinner"></span>{/if}
      {t("common.continue")}
    </button>
  </form>

  {#snippet footer()}
    <button type="button" class="au-link" onclick={() => signOut().catch((e) => (error = errMessage(e)))}>{t("common.signOut")}</button>
  {/snippet}
</AuthScreen>

<style>
  .counts {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12.5px;
    color: var(--content-subtle);
  }
  .options {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .option {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 12px 14px;
    border: 1px solid var(--separator);
    border-radius: var(--radius-lg);
    background: var(--surface-overlay);
  }
  .option.on {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .option input {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }
  .option:focus-within {
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .icon {
    flex: none;
    margin-top: 1px;
    color: var(--accent);
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .name {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    font-weight: 600;
  }
  .badge {
    font-size: 10.5px;
    font-weight: 500;
    padding: 1px 7px;
    border-radius: 999px;
    background: var(--accent-soft);
    color: var(--accent);
  }
  .desc {
    font-size: 12px;
    line-height: 17px;
    color: var(--content-subtle);
  }
</style>
