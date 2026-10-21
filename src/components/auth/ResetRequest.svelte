<!-- Ask for a password reset link. -->
<script lang="ts">
  import { api } from "../../lib/api";
  import { autofocus } from "../../lib/actions";
  import { errMessage, t } from "../../lib/i18n.svelte";
  import AuthScreen from "./AuthScreen.svelte";

  let { email = $bindable(""), onhavelink, onback }: { email?: string; onhavelink: () => void; onback: () => void } = $props();

  let busy = $state(false);
  let error = $state("");
  let sentTo = $state("");

  async function submit(e: Event) {
    e.preventDefault();
    const to = email.trim();
    if (busy || !to) return;
    busy = true;
    error = "";
    try {
      await api.cloudRequestReset(to);
      sentTo = to;
    } catch (err) {
      error = errMessage(err);
    } finally {
      busy = false;
    }
  }
</script>

<AuthScreen title={t("reset.title")} lead={sentTo ? undefined : t("reset.body")} {onback}>
  {#if sentTo}
    <div class="au-info" role="status">{t("reset.sent", { email: sentTo })}</div>
    <button type="button" class="btn primary block" onclick={onhavelink}>{t("reset.haveLink")}</button>
  {:else}
    <form class="au-form" onsubmit={submit}>
      <div class="au-field">
        <label for="rr-email">{t("common.email")}</label>
        <input id="rr-email" class="input" type="email" dir="ltr" bind:value={email} placeholder={t("common.emailPlaceholder")} autocomplete="username" spellcheck="false" use:autofocus />
      </div>
      {#if error}<div class="au-error" role="alert">{error}</div>{/if}
      <button type="submit" class="btn primary block" disabled={busy || !email.trim()}>
        {#if busy}<span class="spinner"></span>{/if}
        {t("reset.submit")}
      </button>
      <button type="button" class="au-link alt" onclick={onhavelink}>{t("reset.haveLink")}</button>
    </form>
  {/if}
</AuthScreen>

<style>
  .alt {
    align-self: center;
    font-size: 12.5px;
  }
</style>
