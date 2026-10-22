<!-- Create a Camel Mail account. -->
<script lang="ts">
  import { api } from "../../lib/api";
  import { autofocus } from "../../lib/actions";
  import { errMessage, i18n, t } from "../../lib/i18n.svelte";
  import { extractToken } from "../../lib/gate";
  import { MIN_LENGTH, personalWords } from "../../lib/password";
  import type { Meta } from "../../lib/types";
  import AuthScreen from "./AuthScreen.svelte";
  import PasswordField from "./PasswordField.svelte";

  let {
    meta,
    email = $bindable(""),
    invite: givenInvite = "",
    oncreated,
    onsignin,
    onback,
  }: {
    meta: Meta | null;
    email?: string;
    /** An invitation code to start with (pasted on the confirmation screen). */
    invite?: string;
    /** `verify`: what the server asks for next. The password is passed on so signing in can follow. */
    oncreated: (verify: "email" | "none", password: string) => void;
    onsignin: () => void;
    onback: () => void;
  } = $props();

  let name = $state("");
  let password = $state("");
  // svelte-ignore state_referenced_locally
  let invite = $state(givenInvite);
  // Open sign-up still takes invitations (an invited account needs no e-mail check).
  // svelte-ignore state_referenced_locally
  let showInvite = $state(!!givenInvite);
  let busy = $state(false);
  let error = $state("");

  const needsInvite = $derived(meta?.signup === "invite");
  const personal = $derived(personalWords(name, email));

  async function submit(e: Event) {
    e.preventDefault();
    if (busy) return;
    if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim())) {
      error = t("setup.invalidEmail");
      return;
    }
    if ([...password].length < MIN_LENGTH) {
      error = t("pw.tooShort");
      return;
    }
    busy = true;
    error = "";
    try {
      const r = await api.cloudSignup({
        name: name.trim(),
        email: email.trim(),
        password,
        invite: needsInvite || invite.trim() ? extractToken(invite) || null : null,
        locale: i18n.locale,
      });
      email = email.trim();
      oncreated(r.verify === "none" ? "none" : "email", password);
    } catch (err) {
      error = errMessage(err);
    } finally {
      busy = false;
    }
  }
</script>

<AuthScreen title={t("signup.title")} lead={t("signup.body")} {onback}>
  <form class="au-form" onsubmit={submit}>
    {#if needsInvite || showInvite}
      <div class="au-field">
        <label for="su-invite">{t("signup.invite")}</label>
        <input id="su-invite" class="input" dir="ltr" bind:value={invite} placeholder={t("signup.invitePlaceholder")} spellcheck="false" autocapitalize="off" required={needsInvite} use:autofocus />
      </div>
    {:else}
      <button type="button" class="au-link small invite-toggle" onclick={() => (showInvite = true)}>{t("signup.haveInvite")}</button>
    {/if}
    <div class="au-field">
      <label for="su-name">{t("common.name")}</label>
      <input id="su-name" class="input" dir="auto" bind:value={name} placeholder={t("common.namePlaceholder")} autocomplete="name" required use:autofocus={!needsInvite && !showInvite} />
    </div>
    <div class="au-field">
      <label for="su-email">{t("common.email")}</label>
      <input id="su-email" class="input" type="email" dir="ltr" bind:value={email} placeholder={t("common.emailPlaceholder")} autocomplete="email" spellcheck="false" required />
    </div>
    <PasswordField id="su-password" label={t("common.password")} bind:value={password} autocomplete="new-password" meter {personal} />
    {#if !password}<p class="au-help">{t("signup.passwordHint")}</p>{/if}
    {#if error}<div class="au-error" role="alert">{error}</div>{/if}
    <button type="submit" class="btn primary block" disabled={busy || !name.trim() || !email.trim() || !password}>
      {#if busy}<span class="spinner"></span>{/if}
      {t("signup.submit")}
    </button>
  </form>

  {#snippet footer()}
    <span>{t("signup.haveAccount")}</span>
    <button type="button" class="au-link" onclick={onsignin}>{t("welcome.signIn")}</button>
  {/snippet}
</AuthScreen>

<style>
  .invite-toggle {
    align-self: flex-start;
  }
</style>
