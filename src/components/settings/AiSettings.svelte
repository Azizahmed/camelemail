<script lang="ts">
  import { onMount } from "svelte";
  import { CheckCircle2, Download, KeyRound } from "@lucide/svelte";
  import { api, errorText } from "../../lib/api";
  import type { AiSettings } from "../../lib/types";
  import { toast, toastError } from "../../lib/ui.svelte";
  import { t } from "../../lib/i18n.svelte";
  import Toggle from "../Toggle.svelte";

  const PRESETS: { id: string; name: string; provider: AiSettings["provider"]; baseUrl: string; model: string; keyUrl?: string }[] = [
    { id: "openai", name: "OpenAI", provider: "openai", baseUrl: "https://api.openai.com/v1", model: "", keyUrl: "https://platform.openai.com/api-keys" },
    { id: "anthropic", name: "Anthropic", provider: "anthropic", baseUrl: "https://api.anthropic.com", model: "claude-sonnet-5-5", keyUrl: "https://console.anthropic.com/settings/keys" },
    { id: "openrouter", name: "OpenRouter", provider: "openai", baseUrl: "https://openrouter.ai/api/v1", model: "", keyUrl: "https://openrouter.ai/keys" },
    { id: "gemini", name: "Google Gemini", provider: "openai", baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai", model: "", keyUrl: "https://aistudio.google.com/apikey" },
    { id: "groq", name: "Groq", provider: "openai", baseUrl: "https://api.groq.com/openai/v1", model: "", keyUrl: "https://console.groq.com/keys" },
    { id: "deepseek", name: "DeepSeek", provider: "openai", baseUrl: "https://api.deepseek.com/v1", model: "deepseek-chat", keyUrl: "https://platform.deepseek.com/api_keys" },
    { id: "ollama", name: "Ollama (local)", provider: "openai", baseUrl: "http://localhost:11434/v1", model: "" },
  ];

  let s = $state<AiSettings | null>(null);
  let saved = $state<string>("");
  let hasKey = $state(false);
  let key = $state("");
  let models = $state<string[]>([]);
  let loadingModels = $state(false);
  let testing = $state(false);
  let saving = $state(false);

  const dirty = $derived(!!s && (JSON.stringify(s) !== saved || key.trim() !== ""));
  const preset = $derived(s ? PRESETS.find((p) => p.baseUrl === s!.baseUrl.trim().replace(/\/$/, "")) : undefined);
  const local = $derived(!!s && /:\/\/(localhost|127\.|\[::1\]|0\.0\.0\.0)/.test(s.baseUrl));

  onMount(async () => {
    try {
      const v = await api.aiSettings();
      s = v.settings;
      saved = JSON.stringify(v.settings);
      hasKey = v.hasKey;
    } catch (e) {
      toastError(e);
    }
  });

  function usePreset(p: (typeof PRESETS)[number]) {
    if (!s) return;
    s.provider = p.provider;
    s.baseUrl = p.baseUrl;
    s.model = p.model;
    models = [];
  }

  async function save(enable?: boolean) {
    if (!s) return;
    if (enable !== undefined) s.enabled = enable;
    saving = true;
    try {
      const v = await api.setAiSettings($state.snapshot(s) as AiSettings, key.trim() ? key.trim() : null);
      s = v.settings;
      saved = JSON.stringify(v.settings);
      hasKey = v.hasKey;
      key = "";
      if (enable === undefined) toast("AI settings saved");
    } catch (e) {
      toastError(e);
    } finally {
      saving = false;
    }
  }

  async function removeKey() {
    if (!s) return;
    try {
      const v = await api.setAiSettings($state.snapshot(s) as AiSettings, "");
      hasKey = v.hasKey;
      toast("API key removed");
    } catch (e) {
      toastError(e);
    }
  }

  async function loadModels() {
    if (!s) return;
    loadingModels = true;
    try {
      models = await api.aiModels($state.snapshot(s) as AiSettings, key.trim() || null);
      toast(models.length ? `${models.length} models available` : "The provider didn't list any models");
    } catch (e) {
      toastError(e, "Couldn't load models");
    } finally {
      loadingModels = false;
    }
  }

  async function test() {
    if (!s) return;
    testing = true;
    try {
      const reply = await api.aiTest($state.snapshot(s) as AiSettings, key.trim() || null);
      toast(`Connected. ${s.model} replied “${reply}”`, { kind: "success", duration: 5000 });
    } catch (e) {
      toast(errorText(e), { kind: "error", duration: 8000 });
    } finally {
      testing = false;
    }
  }
</script>

{#if s}
  <h1>{t("settings.ai")}</h1>
  <p class="intro">
    Summaries, reply suggestions, rewriting and a chat assistant that can search and act on your mail. Bring your own key from any
    OpenAI-compatible or Anthropic-compatible provider, or run a local model.
  </p>

  <div class="row">
    <div class="row-text">
      <div class="row-label">Enable the AI assistant</div>
      <div class="row-desc">Only the emails you use it on are sent to the provider below.</div>
    </div>
    <Toggle label="Enable the AI assistant" checked={s.enabled} onchange={(v) => save(v)} />
  </div>

  <div class="group-title">Provider</div>
  <div class="presets">
    {#each PRESETS as p (p.id)}
      <button class="preset" class:on={preset?.id === p.id} onclick={() => usePreset(p)}>{p.name}</button>
    {/each}
  </div>

  <div class="field">
    <span class="lbl">API style</span>
    <div class="seg" role="radiogroup" aria-label="API style">
      <button role="radio" aria-checked={s.provider === "openai"} class:on={s.provider === "openai"} onclick={() => (s!.provider = "openai")}>OpenAI-compatible</button>
      <button role="radio" aria-checked={s.provider === "anthropic"} class:on={s.provider === "anthropic"} onclick={() => (s!.provider = "anthropic")}>Anthropic-compatible</button>
    </div>
  </div>
  <div class="field">
    <label for="ai-url">API URL</label>
    <input id="ai-url" class="input" bind:value={s.baseUrl} placeholder={s.provider === "anthropic" ? "https://api.anthropic.com" : "https://api.openai.com/v1"} spellcheck="false" />
  </div>
  <div class="field">
    <label for="ai-key">API key {#if local}<span class="opt">(optional for local servers)</span>{/if}</label>
    <div class="inline">
      <input id="ai-key" class="input" type="password" bind:value={key} placeholder={hasKey ? "Saved in the system keyring — type to replace" : "Paste your API key"} autocomplete="off" spellcheck="false" />
      {#if hasKey}<button class="btn" onclick={removeKey}>Remove</button>{/if}
    </div>
    {#if preset?.keyUrl}
      <button class="link" onclick={() => api.openUrl(preset!.keyUrl!)}><KeyRound size={12} /> Get a {preset.name} API key</button>
    {/if}
  </div>
  <div class="field">
    <label for="ai-model">Model</label>
    <div class="inline">
      <input id="ai-model" class="input" list="ai-models" bind:value={s.model} placeholder={s.provider === "anthropic" ? "claude-sonnet-5-5" : "Model id, e.g. from Load models"} spellcheck="false" />
      <datalist id="ai-models">
        {#each models as m (m)}<option value={m}></option>{/each}
      </datalist>
      <button class="btn" disabled={loadingModels} onclick={loadModels}>
        {#if loadingModels}<span class="spinner"></span>{:else}<Download size={13} />{/if} Load models
      </button>
    </div>
  </div>

  <div class="group-title">Behaviour</div>
  <div class="field">
    <label for="ai-instr">Personal instructions</label>
    <textarea id="ai-instr" class="input area" bind:value={s.instructions} placeholder="e.g. Keep replies short and friendly. Sign off with my first name. Answer in the language the email was written in."></textarea>
  </div>
  <div class="row">
    <div class="row-text">
      <div class="row-label">Use AI to sort the Smart Inbox</div>
      <div class="row-desc">Sends the sender, subject and a short preview of new Inbox mail to the provider to decide between Important and Other.</div>
    </div>
    <Toggle label="Use AI to sort the Smart Inbox" checked={s.smartSort} onchange={(v) => (s!.smartSort = v)} />
  </div>

  <div class="actions">
    <button class="btn" disabled={testing || !s.model.trim()} onclick={test}>
      {#if testing}<span class="spinner"></span>{:else}<CheckCircle2 size={14} />{/if} Test connection
    </button>
    <span class="grow"></span>
    <button class="btn primary" disabled={!dirty || saving} onclick={() => save()}>Save</button>
  </div>
  <p class="note">
    Camel Mail talks to the provider directly from this computer. Your API key is stored in the system keyring. In chat, the assistant
    asks before it sends, moves or snoozes email.
  </p>
{/if}

<style>
  /* Settings › AI assistance. */
  h1 {
    margin: 0 0 18px;
    font-size: 20px;
    line-height: 26px;
    font-weight: 600;
  }
  .intro {
    margin: -6px 0 12px;
    color: var(--content-subtle);
    font-size: 13px;
    line-height: 19px;
  }
  .group-title {
    margin: 24px 0 8px;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--content-muted);
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 24px;
    padding: 12px 0;
    border-bottom: 0.5px solid var(--separator-subtle);
  }
  .row-label {
    font-size: 13px;
    font-weight: 500;
  }
  .row-desc {
    margin-top: 2px;
    font-size: 12px;
    line-height: 16px;
    color: var(--content-subtle);
  }
  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 12px;
  }
  .preset {
    border: 0.5px solid var(--border-tertiary);
    background: var(--surface-elevated);
    border-radius: 999px;
    padding: 5px 12px;
    font-size: 12px;
  }
  .preset:hover {
    background: var(--surface-hover);
  }
  .preset.on {
    border-color: var(--accent);
    background: var(--accent-soft);
    color: var(--accent);
    font-weight: 500;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 12px;
  }
  .field label,
  .lbl {
    font-size: 12px;
    font-weight: 500;
    color: var(--content-subtle);
  }
  .opt {
    font-weight: 400;
    color: var(--content-muted);
  }
  .inline {
    display: flex;
    gap: 8px;
  }
  .inline .input {
    flex: 1;
    min-width: 0;
  }
  .area {
    min-height: 72px;
    resize: vertical;
    padding: 8px 10px;
    line-height: 18px;
    user-select: text;
  }
  .seg {
    align-self: flex-start;
    display: inline-flex;
    padding: 2px;
    border-radius: var(--radius-lg);
    background: var(--surface-hover);
  }
  .seg button {
    border: none;
    background: none;
    padding: 4px 12px;
    border-radius: 6px;
    font-size: 12px;
    font-weight: 500;
    color: var(--content-subtle);
  }
  .seg button.on {
    background: var(--surface-overlay);
    color: var(--content);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.12);
  }
  .link {
    align-self: flex-start;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    border: none;
    background: none;
    padding: 0;
    font-size: 12px;
    color: var(--accent);
  }
  .actions {
    display: flex;
    gap: 8px;
    margin-top: 18px;
  }
  .grow {
    flex: 1;
  }
  .note {
    margin-top: 18px;
    padding: 10px 12px;
    border-radius: var(--radius-lg);
    background: var(--surface-hover);
    color: var(--content-subtle);
    font-size: 12px;
    line-height: 17px;
  }
</style>
