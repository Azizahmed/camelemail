<!-- Sign in with e-mail and password. -->
<script lang="ts">
  import { autofocus } from "../../lib/actions";
  import { errMessage, t } from "../../lib/i18n.svelte";
  import type { Meta } from "../../lib/types";
  import AuthScreen from "./AuthScreen.svelte";
  import PasswordField from "./PasswordField.svelte";

  let {
    meta,
    email = $bindable(""),
    notice = "",
    onlogin,
    oncreate,
    onforgot,
    onback,
  }: {
    meta: Meta | null;
    email?: string;
    /** Shown above the form ("Email confirmed…", "Password changed…"). */
    notice?: string;
    /** Signs in; rejects with the server error. */
    onlogin: (email: string, password: string) => Promise<void>;
    oncreate: () => void;
    onforgot: () => void;
    onback: () => void;
  } = $props();

  let password = $state("");
  let busy = $state(false);
  let error = $state("");
  // On-prem servers reset passwords through the administrator.
  const selfReset = $derived(meta?.mode !== "onprem");

  async function submit(e: Event) {
    e.preventDefault();
    if (busy || !email.trim() || !password) return;
    busy = true;
    error = "";
    try {
      await onlogin(email.trim(), password);
    } catch (err) {
      error = errMessage(err);
    } finally {
      busy = false;
    }
  }
</script>

<AuthScreen title={meta?.orgName ? t("welcome.org", { org: meta.orgName }) : t("signin.title")} {onback}>
  {#if notice}<div class="au-info" role="status">{notice}</div>{/if}
  <form class="au-form" onsubmit={submit}>
    <div class="au-field">
      <label for="si-email">{t("common.email")}</label>
      <input id="si-email" class="input" type="email" dir="ltr" bind:value={email} placeholder={t("common.emailPlaceholder")} autocomplete="username" spellcheck="false" use:autofocus={!email} />
    </div>
    <PasswordField id="si-password" label={t("common.password")} bind:value={password} focus={!!email} />
    {#if selfReset}
      <button type="button" class="au-link forgot" onclick={onforgot}>{t("signin.forgot")}</button>
    {:else}
      <p class="au-help">{t("signin.askAdmin")}</p>
    {/if}
    {#if error}<div class="au-error" role="alert">{error}</div>{/if}
    <button type="submit" class="btn primary block" disabled={busy || !email.trim() || !password}>
      {#if busy}<span class="spinner"></span>{/if}
      {t("signin.submit")}
    </button>
  </form>

  {#snippet footer()}
    <span>{t("signin.noAccount")}</span>
    <button type="button" class="au-link" onclick={oncreate}>{t("signin.create")}</button>
  {/snippet}
</AuthScreen>

<style>
  .forgot {
    align-self: flex-start;
    margin-top: -6px;
    font-size: 12px;
  }
</style>
