<!-- The second step of signing in: an authenticator code or a backup code. -->
<script lang="ts">
  import { autofocus } from "../../lib/actions";
  import { isCloudError } from "../../lib/cloud-error";
  import { errMessage, t } from "../../lib/i18n.svelte";
  import AuthScreen from "./AuthScreen.svelte";

  let {
    onverify,
    onexpired,
    onback,
  }: {
    /** Rejects with the server error. */
    onverify: (code: string) => Promise<void>;
    onexpired: () => void;
    onback: () => void;
  } = $props();

  let backup = $state(false);
  let code = $state("");
  let busy = $state(false);
  let error = $state("");

  const clean = $derived(backup ? code.trim() : code.replace(/\s+/g, ""));
  const ready = $derived(backup ? clean.length >= 8 : /^\d{6}$/.test(clean));

  async function submit(e: Event) {
    e.preventDefault();
    if (busy || !ready) return;
    busy = true;
    error = "";
    try {
      await onverify(clean);
    } catch (err) {
      if (isCloudError(err, "invalid_token") || isCloudError(err, "unauthorized")) return onexpired();
      error = errMessage(err);
      code = "";
    } finally {
      busy = false;
    }
  }

  function toggle() {
    backup = !backup;
    code = "";
    error = "";
  }
</script>

<AuthScreen title={t("twofa.title")} lead={backup ? t("twofa.backupBody") : t("twofa.body")} {onback}>
  <form class="au-form" onsubmit={submit}>
    {#key backup}
      <div class="au-field">
        <label for="tf-code">{backup ? t("twofa.backupLabel") : t("twofa.code")}</label>
        {#if backup}
          <input id="tf-code" class="input code" dir="ltr" bind:value={code} placeholder="xxxx-xxxx" autocomplete="off" spellcheck="false" autocapitalize="off" use:autofocus />
        {:else}
          <input id="tf-code" class="input code" dir="ltr" bind:value={code} inputmode="numeric" autocomplete="one-time-code" maxlength="7" placeholder="123456" use:autofocus />
        {/if}
      </div>
    {/key}
    {#if error}<div class="au-error" role="alert">{error}</div>{/if}
    <button type="submit" class="btn primary block" disabled={busy || !ready}>
      {#if busy}<span class="spinner"></span>{/if}
      {t("twofa.submit")}
    </button>
    <button type="button" class="au-link alt" onclick={toggle}>{backup ? t("twofa.useApp") : t("twofa.useBackup")}</button>
  </form>
</AuthScreen>

<style>
  .code {
    height: 40px;
    font-family: var(--font-mono);
    font-size: 17px;
    letter-spacing: 3px;
    text-align: center;
  }
  :global([dir="rtl"]) .code.input {
    text-align: center;
  }
  .alt {
    align-self: center;
    font-size: 12.5px;
  }
</style>
