<!-- Choose a new password with the link or code from the reset e-mail. -->
<script lang="ts">
  import { api } from "../../lib/api";
  import { autofocus } from "../../lib/actions";
  import { errMessage, t } from "../../lib/i18n.svelte";
  import { extractToken } from "../../lib/gate";
  import { MIN_LENGTH, personalWords } from "../../lib/password";
  import AuthScreen from "./AuthScreen.svelte";
  import PasswordField from "./PasswordField.svelte";

  let { email = "", ondone, onback }: { email?: string; ondone: () => void; onback: () => void } = $props();

  let link = $state("");
  let password = $state("");
  let confirm = $state("");
  let busy = $state(false);
  let error = $state("");

  async function submit(e: Event) {
    e.preventDefault();
    const token = extractToken(link);
    if (busy || !token) return;
    if ([...password].length < MIN_LENGTH) {
      error = t("pw.tooShort");
      return;
    }
    if (password !== confirm) {
      error = t("pw.mismatch");
      return;
    }
    busy = true;
    error = "";
    try {
      await api.cloudResetPassword(token, password);
      ondone();
    } catch (err) {
      error = errMessage(err);
    } finally {
      busy = false;
    }
  }
</script>

<AuthScreen title={t("reset.newTitle")} lead={t("reset.signsOut")} {onback}>
  <form class="au-form" onsubmit={submit}>
    <div class="au-field">
      <label for="rp-token">{t("reset.tokenLabel")}</label>
      <input id="rp-token" class="input" dir="ltr" bind:value={link} placeholder={t("reset.tokenPlaceholder")} spellcheck="false" autocapitalize="off" autocomplete="off" use:autofocus />
    </div>
    <PasswordField id="rp-new" label={t("reset.newPassword")} bind:value={password} autocomplete="new-password" meter personal={personalWords(email)} />
    <PasswordField id="rp-confirm" label={t("reset.confirm")} bind:value={confirm} autocomplete="new-password" />
    {#if error}<div class="au-error" role="alert">{error}</div>{/if}
    <button type="submit" class="btn primary block" disabled={busy || !extractToken(link) || !password || !confirm}>
      {#if busy}<span class="spinner"></span>{/if}
      {t("reset.submitNew")}
    </button>
  </form>
</AuthScreen>
