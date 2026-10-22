<!--
  Settings › Admin › AI assistant: the provider everyone in the organisation
  uses. Devices call it directly with the organisation's key, which they get
  from the server while AI is on. The key is write-only here.
-->
<script lang="ts">
  import { CheckCircle2, Download, KeyRound } from "@lucide/svelte";
  import { api } from "../../../lib/api";
  import { aiProblems, isLocalUrl } from "../../../lib/admin";
  import { errMessage, t } from "../../../lib/i18n.svelte";
  import type { AdminConfig, OrgAi } from "../../../lib/types";
  import { toast, toastError } from "../../../lib/ui.svelte";
  import Select from "../../Select.svelte";
  import Toggle from "../../Toggle.svelte";

  let { config, onsaved }: { config: AdminConfig; onsaved: (c: AdminConfig) => void } = $props();

  const PRESETS: { id: string; name: string; provider: OrgAi["provider"]; baseUrl: string; model: string; keyUrl?: string }[] = [
    { id: "openai", name: "OpenAI", provider: "openai", baseUrl: "https://api.openai.com/v1", model: "", keyUrl: "https://platform.openai.com/api-keys" },
    { id: "anthropic", name: "Anthropic", provider: "anthropic", baseUrl: "https://api.anthropic.com", model: "claude-sonnet-5-5", keyUrl: "https://console.anthropic.com/settings/keys" },
    { id: "openrouter", name: "OpenRouter", provider: "openai", baseUrl: "https://openrouter.ai/api/v1", model: "", keyUrl: "https://openrouter.ai/keys" },
    { id: "gemini", name: "Google Gemini", provider: "openai", baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai", model: "", keyUrl: "https://aistudio.google.com/apikey" },
    { id: "groq", name: "Groq", provider: "openai", baseUrl: "https://api.groq.com/openai/v1", model: "", keyUrl: "https://console.groq.com/keys" },
    { id: "deepseek", name: "DeepSeek", provider: "openai", baseUrl: "https://api.deepseek.com/v1", model: "deepseek-chat", keyUrl: "https://platform.deepseek.com/api_keys" },
  ];

  const initial = (() => config)();
  let ai = $state<OrgAi>({ ...initial.ai });
  let saved = $state(JSON.stringify(initial.ai));
  let keySet = $state(initial.aiKeySet);
  /** A new key typed here (never the saved one: that isn't sent back). */
  let key = $state("");
  let clearKey = $state(false);
  let showErrors = $state(false);
  let saving = $state(false);
  let models = $state<string[]>([]);
  let loadingModels = $state(false);
  let testing = $state(false);

  /** null keeps the saved key, "" clears it. */
  const keyArg = $derived(clearKey ? "" : key.trim() ? key.trim() : null);
  const dirty = $derived(JSON.stringify(ai) !== saved || keyArg !== null);
  const problems = $derived(aiProblems(ai, keyArg));
  const shown = $derived(showErrors ? problems : {});
  const preset = $derived(PRESETS.find((p) => p.baseUrl === ai.baseUrl.trim().replace(/\/$/, "")));
  const local = $derived(isLocalUrl(ai.baseUrl));
  const noKey = $derived(ai.enabled && !local && (clearKey || (!keySet && !key.trim())));

  function usePreset(p: (typeof PRESETS)[number]) {
    ai.provider = p.provider;
    ai.baseUrl = p.baseUrl;
    ai.model = p.model;
    models = [];
  }

  async function save() {
    showErrors = true;
    if (Object.keys(problems).length) {
      toast(t("admin.prov.fix"), { kind: "error" });
      return;
    }
    saving = true;
    try {
      const c = await api.adminSetAi($state.snapshot(ai) as OrgAi, keyArg);
      ai = { ...c.ai };
      saved = JSON.stringify(c.ai);
      keySet = c.aiKeySet;
      key = "";
      clearKey = false;
      showErrors = false;
      onsaved(c);
      toast(t("admin.saved"), { kind: "success" });
    } catch (e) {
      toastError(errMessage(e));
    } finally {
      saving = false;
    }
  }

  function discard() {
    ai = JSON.parse(saved);
    key = "";
    clearKey = false;
    showErrors = false;
  }

  async function loadModels() {
    loadingModels = true;
    try {
      models = await api.adminAiModels($state.snapshot(ai) as OrgAi, key.trim() || null);
      toast(models.length ? t("admin.ai.models", { n: models.length }) : t("admin.ai.noModels"));
    } catch (e) {
      toastError(errMessage(e), t("admin.ai.modelsFailed"));
    } finally {
      loadingModels = false;
    }
  }

  async function test() {
    testing = true;
    try {
      const reply = await api.adminAiTest($state.snapshot(ai) as OrgAi, key.trim() || null);
      toast(t("admin.ai.testOk", { model: ai.model, reply }), { kind: "success", duration: 6000 });
    } catch (e) {
      toast(errMessage(e), { kind: "error", duration: 8000 });
    } finally {
      testing = false;
    }
  }
</script>

<p class="intro">{t("admin.ai.desc")}</p>

<div class="row">
  <div class="row-text">
    <div class="row-label">{t("admin.ai.enable")}</div>
    <div class="row-desc">{t("admin.ai.enableDesc")}</div>
  </div>
  <div class="row-control"><Toggle label={t("admin.ai.enable")} checked={ai.enabled} onchange={(v) => (ai.enabled = v)} /></div>
</div>

<div class="group-title">{t("admin.ai.provider")}</div>
<div class="presets" dir="ltr">
  {#each PRESETS as p (p.id)}
    <button class="preset" class:on={preset?.id === p.id} onclick={() => usePreset(p)}>{p.name}</button>
  {/each}
</div>

<div class="field">
  <span class="lbl">{t("admin.ai.style")}</span>
  <div class="seg" role="radiogroup" aria-label={t("admin.ai.style")}>
    <button role="radio" aria-checked={ai.provider === "openai"} class:on={ai.provider === "openai"} onclick={() => (ai.provider = "openai")}>{t("admin.ai.openai")}</button>
    <button role="radio" aria-checked={ai.provider === "anthropic"} class:on={ai.provider === "anthropic"} onclick={() => (ai.provider = "anthropic")}>{t("admin.ai.anthropic")}</button>
  </div>
</div>

<div class="field">
  <label for="adm-ai-url">{t("admin.ai.url")}</label>
  <input
    id="adm-ai-url"
    class="input"
    class:bad={shown.baseUrl}
    dir="ltr"
    bind:value={ai.baseUrl}
    placeholder={ai.provider === "anthropic" ? "https://api.anthropic.com" : "https://api.openai.com/v1"}
    spellcheck="false"
    autocomplete="off"
  />
  {#if shown.baseUrl}<div class="err">{t(shown.baseUrl)}</div>{/if}
</div>

<div class="field">
  <label for="adm-ai-key">
    {t("admin.ai.key")}
    <span class="state" class:set={keySet && !clearKey}>{keySet && !clearKey ? t("admin.set") : t("admin.notSet")}</span>
  </label>
  <div class="inline">
    <input
      id="adm-ai-key"
      class="input"
      class:bad={shown.key}
      type="password"
      dir={key ? "ltr" : undefined}
      bind:value={key}
      oninput={() => (clearKey = false)}
      placeholder={keySet && !clearKey ? t("admin.keepSecret") : t("admin.ai.keyPlaceholder")}
      autocomplete="off"
      spellcheck="false"
    />
    {#if keySet && !clearKey && !key}<button class="btn" onclick={() => (clearKey = true)}>{t("admin.clear")}</button>{/if}
    {#if clearKey}<button class="btn" onclick={() => (clearKey = false)}>{t("admin.keep")}</button>{/if}
  </div>
  {#if shown.key}<div class="err">{t(shown.key)}</div>{/if}
  <div class="hint">{clearKey ? t("admin.willClear") : t("admin.writeOnly")}</div>
  {#if noKey}<div class="hint">{t("admin.ai.noKey")}</div>{/if}
  {#if preset?.keyUrl}
    <button class="link" onclick={() => api.openUrl(preset!.keyUrl!)}><KeyRound size={12} /> {t("admin.ai.getKey", { name: preset.name })}</button>
  {/if}
</div>

<div class="field">
  <label for="adm-ai-model">{t("admin.ai.model")}</label>
  <div class="inline">
    <input id="adm-ai-model" class="input" class:bad={shown.model} dir={ai.model ? "ltr" : undefined} bind:value={ai.model} placeholder={t("admin.ai.modelPlaceholder")} spellcheck="false" autocomplete="off" />
    <button class="btn" disabled={loadingModels || !ai.baseUrl.trim()} onclick={loadModels}>
      {#if loadingModels}<span class="spinner"></span>{:else}<Download size={13} />{/if}
      {t("admin.ai.loadModels")}
    </button>
  </div>
  {#if models.length}
    <Select label={t("admin.ai.model")} value={models.includes(ai.model) ? ai.model : ""} options={[...(models.includes(ai.model) ? [] : [{ value: "", label: "—" }]), ...models.map((m) => ({ value: m, label: m }))]} onchange={(v) => v && (ai.model = v)} />
  {/if}
  {#if shown.model}<div class="err">{t(shown.model)}</div>{/if}
</div>

<div class="row">
  <div class="row-text">
    <div class="row-label">{t("admin.ai.maxTokens")}</div>
    <div class="row-desc">{t("admin.ai.maxTokensDesc")}</div>
    {#if shown.maxTokens}<div class="err">{t(shown.maxTokens)}</div>{/if}
  </div>
  <div class="row-control">
    <input class="input tokens" class:bad={shown.maxTokens} type="number" min="256" max="1000000" step="256" dir="ltr" aria-label={t("admin.ai.maxTokens")} bind:value={ai.maxTokens} />
  </div>
</div>

<div class="actions">
  <button class="btn" disabled={testing || !ai.model.trim() || !ai.baseUrl.trim()} onclick={test}>
    {#if testing}<span class="spinner"></span>{:else}<CheckCircle2 size={14} />{/if}
    {t("admin.ai.test")}
  </button>
  <span class="grow"></span>
  {#if dirty}<button class="btn" onclick={discard}>{t("admin.discard")}</button>{/if}
  <button class="btn primary" disabled={!dirty || saving} onclick={save}>{t("common.save")}</button>
</div>

<style>
  .tokens {
    width: 120px;
  }
</style>
