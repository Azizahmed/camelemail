<!-- "Using Camel Mail at work?": a server address or an organisation code. -->
<script lang="ts">
  import { api } from "../../lib/api";
  import { autofocus } from "../../lib/actions";
  import { errMessage, t } from "../../lib/i18n.svelte";
  import type { Meta } from "../../lib/types";
  import AuthScreen from "./AuthScreen.svelte";

  let {
    current,
    defaultServer,
    onconnected,
    onback,
  }: {
    current: string | null;
    defaultServer: string | null;
    onconnected: (meta: Meta) => void;
    onback?: () => void;
  } = $props();

  // svelte-ignore state_referenced_locally
  let address = $state(current && current !== defaultServer ? current.replace(/^https?:\/\//, "").replace(/\/$/, "") : "");
  let busy = $state(false);
  let error = $state("");

  async function connect(to: string) {
    if (busy) return;
    busy = true;
    error = "";
    try {
      onconnected(await api.cloudSetServer(to));
    } catch (e) {
      error = errMessage(e);
    } finally {
      busy = false;
    }
  }

  function submit(e: Event) {
    e.preventDefault();
    const a = address.trim();
    if (!a) {
      error = t("server.required");
      return;
    }
    void connect(a);
  }
</script>

<AuthScreen title={t("server.title")} lead={t("server.body")} {onback}>
  <form class="au-form" onsubmit={submit}>
    <div class="au-field">
      <label for="srv-address">{t("server.label")}</label>
      <input
        id="srv-address"
        class="input"
        dir="ltr"
        bind:value={address}
        placeholder={t("server.placeholder")}
        autocomplete="url"
        autocapitalize="off"
        spellcheck="false"
        use:autofocus
      />
    </div>
    {#if error}<div class="au-error" role="alert">{error}</div>{/if}
    <button type="submit" class="btn primary block" disabled={busy}>
      {#if busy}<span class="spinner"></span>{/if}
      {t("server.submit")}
    </button>
    {#if defaultServer && current && current !== defaultServer}
      <button type="button" class="au-link center" disabled={busy} onclick={() => connect(defaultServer)}>{t("server.useDefault")}</button>
    {/if}
  </form>
</AuthScreen>

<style>
  .center {
    align-self: center;
    font-size: 12.5px;
  }
</style>
