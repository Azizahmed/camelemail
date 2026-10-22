<!--
  Settings › AI assistance: each person's own options (on/off, instructions,
  Smart Inbox sorting). The provider, model and key are the organisation's,
  set by an administrator in Settings › Admin › AI assistant.
-->
<script lang="ts">
  import "./admin/admin.css";
  import { onMount } from "svelte";
  import { ShieldCheck, Sparkles } from "@lucide/svelte";
  import { api, onEngineEvent } from "../../lib/api";
  import { cloud } from "../../lib/cloud.svelte";
  import { errMessage, t } from "../../lib/i18n.svelte";
  import type { AiPrefs, AiSettingsView } from "../../lib/types";
  import { toast, toastError } from "../../lib/ui.svelte";
  import Toggle from "../Toggle.svelte";

  let { onadmin }: { onadmin?: () => void } = $props();

  let v = $state<AiSettingsView | null>(null);
  let instructions = $state("");
  let savedInstructions = $state("");
  let saving = $state(false);

  const isAdmin = $derived(cloud.status?.me?.role === "owner" || cloud.status?.me?.role === "admin");
  const dirty = $derived(instructions !== savedInstructions);

  function take(next: AiSettingsView, keepDraft = false) {
    v = next;
    if (!keepDraft || !dirty) instructions = next.settings.instructions;
    savedInstructions = next.settings.instructions;
  }

  async function load() {
    try {
      take(await api.aiSettings(), true);
    } catch (e) {
      toastError(errMessage(e));
    }
  }

  onMount(() => {
    void load();
    const un = onEngineEvent((e) => {
      // The organisation changed its AI setup, or another device changed these options.
      if (e.type === "aiChanged" || e.type === "settingsChanged") void load();
    });
    return () => void un.then((u) => u());
  });

  async function save(patch: Partial<AiPrefs>, announce = false) {
    if (!v) return;
    const prefs: AiPrefs = { enabled: v.personalEnabled, instructions, smartSort: v.settings.smartSort, ...patch };
    saving = true;
    try {
      take(await api.setAiSettings(prefs));
      if (announce) toast(t("ai.saved"));
    } catch (e) {
      toastError(errMessage(e));
    } finally {
      saving = false;
    }
  }

  function openAdmin() {
    if (onadmin) onadmin();
    else void api.openSettings("admin:ai");
  }
</script>

<div class="adm">
  <h1>{t("settings.ai")}</h1>
  <p class="intro">{t("ai.intro")}</p>

  {#if v && !v.orgReady}
    <div class="blank">
      <span class="blank-icon"><Sparkles size={22} /></span>
      <div class="blank-title">{t("ai.notSetUp")}</div>
      <div class="blank-body">{isAdmin ? t("ai.notSetUpAdmin") : t("ai.notSetUpUser")}</div>
      {#if isAdmin}
        <button class="btn primary" onclick={openAdmin}><ShieldCheck size={14} /> {t("ai.setUp")}</button>
      {/if}
    </div>
  {:else if v}
    <div class="row">
      <div class="row-text">
        <div class="row-label">{t("ai.use")}</div>
        <div class="row-desc">{t("ai.useDesc", { model: v.settings.model })}</div>
      </div>
      <div class="row-control">
        <Toggle label={t("ai.use")} checked={v.personalEnabled} disabled={saving} onchange={(on) => save({ enabled: on })} />
      </div>
    </div>

    <div class="group-title">{t("ai.behaviour")}</div>
    <div class="field">
      <label for="ai-instr">{t("ai.instructions")}</label>
      <textarea id="ai-instr" class="input area" dir="auto" bind:value={instructions} placeholder={t("ai.instructionsPlaceholder")}></textarea>
    </div>
    <div class="row">
      <div class="row-text">
        <div class="row-label">{t("ai.smartSort")}</div>
        <div class="row-desc">{t("ai.smartSortDesc")}</div>
      </div>
      <div class="row-control">
        <Toggle label={t("ai.smartSort")} checked={v.settings.smartSort} disabled={saving} onchange={(on) => save({ smartSort: on })} />
      </div>
    </div>

    <div class="actions">
      {#if isAdmin}
        <button class="link" onclick={openAdmin}><ShieldCheck size={12} /> {t("ai.manage")}</button>
      {/if}
      <span class="grow"></span>
      <button class="btn primary" disabled={!dirty || saving} onclick={() => save({}, true)}>{t("common.save")}</button>
    </div>
    <p class="note">{t("ai.note")}</p>
  {/if}
</div>
