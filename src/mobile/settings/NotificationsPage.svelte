<script lang="ts">
  // Settings › Notifications: Android's permission, and which alerts to show
  // (the same AppSettings switches as the desktop's General section).
  import { BellOff, BellRing } from "@lucide/svelte";
  import { onMount } from "svelte";
  import Toggle from "../../components/Toggle.svelte";
  import { api } from "../../lib/api";
  import { t } from "../../lib/i18n.svelte";
  import type { AppSettings } from "../../lib/types";
  import { toastError } from "../../lib/ui.svelte";
  import { checkPermission, followPermission, notify } from "./notify.svelte";

  let settings = $state<AppSettings | null>(null);
  let asking = $state(false);

  onMount(() => {
    api.getSettings().then((s) => (settings = s), toastError);
    return followPermission();
  });

  let saveSeq = 0;
  async function update(patch: Partial<AppSettings>) {
    if (!settings) return;
    settings = { ...settings, ...patch };
    const seq = ++saveSeq;
    try {
      const saved = await api.setSettings($state.snapshot(settings) as AppSettings);
      // A slower reply must not undo a newer change.
      if (seq === saveSeq) settings = saved;
    } catch (e) {
      toastError(e);
    }
  }

  async function allow() {
    asking = true;
    try {
      await checkPermission(true);
    } finally {
      asking = false;
    }
  }

  function setNewMail(on: boolean) {
    void update({ notifications: on });
    // Turning alerts on is the moment to ask for the permission they need.
    if (on && notify.permission === "prompt") void allow();
  }
</script>

{#if notify.permission}
  <div class="m-section-title">{t("mset.permission")}</div>
  <div class="m-card permission">
    <div class="status">
      {#if notify.permission === "granted"}<BellRing size={21} strokeWidth={1.5} />{:else}<BellOff size={21} strokeWidth={1.5} />{/if}
      <p>
        {t(notify.permission === "granted" ? "mset.permGranted" : notify.permission === "denied" ? "mset.permDenied" : "mset.permPrompt")}
      </p>
    </div>
    {#if notify.permission === "prompt"}
      <button class="m-primary" disabled={asking} onclick={allow}>{t("mset.allow")}</button>
    {/if}
  </div>
{/if}

{#if settings}
  <div class="m-section-title">{t("mset.alerts")}</div>
  <div class="m-card">
    <div class="m-row">
      <span class="m-row-text">
        <span class="ms-row-title">{t("mset.newMail")}</span>
        <small>{t("mset.newMailDesc")}</small>
      </span>
      <Toggle label={t("mset.newMail")} checked={settings.notifications} onchange={setNewMail} />
    </div>
  </div>
  {#if settings.smartInbox}
    <div class="m-card">
      <div class="m-row">
        <span class="m-row-text"><span class="ms-row-title">{t("mset.importantOnly")}</span></span>
        <Toggle label={t("mset.importantOnly")} checked={settings.notifyImportantOnly} disabled={!settings.notifications} onchange={(v) => update({ notifyImportantOnly: v })} />
      </div>
    </div>
  {/if}
  <div class="m-card">
    <div class="m-row">
      <span class="m-row-text">
        <span class="ms-row-title">{t("mset.snoozeWake")}</span>
        <small>{t("mset.snoozeWakeDesc")}</small>
      </span>
      <Toggle label={t("mset.snoozeWake")} checked={settings.notifyOnSnoozeWake} onchange={(v) => update({ notifyOnSnoozeWake: v })} />
    </div>
  </div>
  <p class="ms-note">{t("mset.pushNote")}</p>
{:else}
  <div class="m-empty"><span class="spinner"></span></div>
{/if}
<div class="ms-end"></div>

<style>
  .permission {
    display: flex;
    flex-direction: column;
    gap: 13px;
    padding: 16px;
  }
  .status {
    display: flex;
    align-items: flex-start;
    gap: 13px;
  }
  .status :global(svg) {
    flex: none;
    color: var(--m-icon);
  }
  .status p {
    margin: 2px 0 0;
    font-size: 13px;
    line-height: 18px;
  }
  .permission .m-primary {
    align-self: flex-start;
  }
  .permission .m-primary:disabled {
    opacity: 0.6;
  }
  .m-row:active {
    background: transparent;
  }
</style>
