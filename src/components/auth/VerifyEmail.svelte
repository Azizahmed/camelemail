<!-- "Check your inbox": paste the confirmation link or code. -->
<script lang="ts">
  import { MailCheck } from "@lucide/svelte";
  import { api } from "../../lib/api";
  import { autofocus } from "../../lib/actions";
  import { errMessage, t } from "../../lib/i18n.svelte";
  import { extractToken, isInviteToken } from "../../lib/gate";
  import AuthScreen from "./AuthScreen.svelte";

  let {
    email,
    onverified,
    onsignin,
    oninvite,
  }: {
    email: string;
    onverified: () => Promise<void> | void;
    onsignin: () => void;
    /** An invitation code was pasted here: it belongs on the sign-up screen. */
    oninvite: (code: string) => void;
  } = $props();

  let link = $state("");
  let busy = $state(false);
  let error = $state("");

  async function submit(e: Event) {
    e.preventDefault();
    const token = extractToken(link);
    if (busy || !token) return;
    if (isInviteToken(token)) {
      oninvite(token);
      return;
    }
    busy = true;
    error = "";
    try {
      await api.cloudVerifyEmail(token);
      await onverified();
    } catch (err) {
      error = errMessage(err);
    } finally {
      busy = false;
    }
  }
</script>

<AuthScreen title={t("verify.title")} onback={onsignin}>
  <div class="icon"><MailCheck size={26} strokeWidth={1.6} /></div>
  <p class="lead">{email ? t("verify.body", { email }) : t("verify.bodyNoEmail")}</p>
  <form class="au-form" onsubmit={submit}>
    <div class="au-field">
      <label for="vf-token">{t("verify.label")}</label>
      <input id="vf-token" class="input" dir="ltr" bind:value={link} placeholder={t("verify.placeholder")} spellcheck="false" autocapitalize="off" autocomplete="off" use:autofocus />
    </div>
    {#if error}<div class="au-error" role="alert">{error}</div>{/if}
    <button type="submit" class="btn primary block" disabled={busy || !extractToken(link)}>
      {#if busy}<span class="spinner"></span>{/if}
      {t("verify.submit")}
    </button>
    <p class="au-help">{t("verify.notArrived")}</p>
  </form>

  {#snippet footer()}
    <button type="button" class="au-link" onclick={onsignin}>{t("verify.backToSignIn")}</button>
  {/snippet}
</AuthScreen>

<style>
  .icon {
    align-self: center;
    width: 56px;
    height: 56px;
    border-radius: 18px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--accent-soft);
    color: var(--accent);
  }
  .lead {
    margin: 0;
    font-size: 13px;
    line-height: 20px;
    color: var(--content-subtle);
    overflow-wrap: anywhere;
  }
</style>
