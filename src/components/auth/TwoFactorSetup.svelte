<!--
  Turn on two-step verification: show the key (as a QR code and as text),
  check a code, then show the backup codes until the person confirms they saved
  them. With `challenge` it's the setup a required-2FA sign-in asks for, and
  enabling also finishes signing in.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { Copy } from "@lucide/svelte";
  import { api } from "../../lib/api";
  import { autofocus } from "../../lib/actions";
  import { cloud } from "../../lib/cloud.svelte";
  import { isCloudError } from "../../lib/cloud-error";
  import { errMessage, t } from "../../lib/i18n.svelte";
  import { toast } from "../../lib/ui.svelte";
  import BackupCodes from "./BackupCodes.svelte";
  import QrCode from "./QrCode.svelte";

  let {
    challenge = null,
    ondone,
    oncancel,
    onexpired,
  }: {
    challenge?: string | null;
    /** The backup codes were saved. */
    ondone: () => void;
    oncancel?: () => void;
    /** The sign-in challenge ran out. */
    onexpired?: () => void;
  } = $props();

  let setup = $state<{ secret: string; otpauthUrl: string } | null>(null);
  let code = $state("");
  let codes = $state<string[] | null>(null);
  let busy = $state(false);
  let error = $state("");

  /** The key in groups of four, easier to type. */
  const grouped = $derived(setup ? setup.secret.replace(/\s+/g, "").replace(/(.{4})(?=.)/g, "$1 ") : "");

  async function load() {
    error = "";
    try {
      setup = await api.cloudTotpSetup(challenge);
    } catch (e) {
      if (challenge && isCloudError(e, "invalid_token")) return onexpired?.();
      error = errMessage(e);
    }
  }
  onMount(load);

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      toast(t("common.copied"));
    } catch {
      /* selectable anyway */
    }
  }

  async function enable(e: Event) {
    e.preventDefault();
    const c = code.replace(/\s+/g, "");
    if (busy || c.length < 6) return;
    busy = true;
    error = "";
    // A sign-in that finishes here must stay on these screens until the codes are saved.
    if (challenge) cloud.holdAuth = true;
    try {
      const r = await api.cloudTotpEnable(c, challenge);
      codes = r.backupCodes;
    } catch (err) {
      cloud.holdAuth = false;
      if (challenge && isCloudError(err, "invalid_token")) return onexpired?.();
      error = errMessage(err);
    } finally {
      busy = false;
    }
  }
</script>

{#if codes}
  <BackupCodes {codes} ondone={() => { toast(t("totp.enabled"), { kind: "success" }); ondone(); }} />
{:else}
  <form class="au-form" onsubmit={enable}>
    {#if challenge}<p class="au-info">{t("totp.required")}</p>{:else}<p class="au-help">{t("totp.intro")}</p>{/if}
    {#if setup}
      <p class="au-help">{t("totp.scan")}</p>
      <div class="qr"><QrCode text={setup.otpauthUrl} label={t("totp.qrAlt")} /></div>
      <div class="au-field">
        <span class="au-label">{t("totp.manual")}</span>
        <div class="key">
          <code class="selectable" dir="ltr">{grouped}</code>
          <button type="button" class="icon-btn" title={t("totp.copyKey")} aria-label={t("totp.copyKey")} onclick={() => copy(setup!.secret)}><Copy size={14} /></button>
        </div>
        <button type="button" class="au-link small" onclick={() => copy(setup!.otpauthUrl)}>{t("totp.copyLink")}</button>
      </div>
      <div class="au-field">
        <label for="totp-code">{t("totp.enterCode")}</label>
        <input
          id="totp-code"
          class="input code"
          dir="ltr"
          inputmode="numeric"
          autocomplete="one-time-code"
          maxlength="7"
          placeholder="123456"
          bind:value={code}
          use:autofocus
        />
      </div>
    {:else if !error}
      <div class="loading"><span class="spinner"></span></div>
    {/if}
    {#if error}<div class="au-error" role="alert">{error}</div>{/if}
    <div class="au-actions">
      {#if oncancel}<button type="button" class="btn" onclick={oncancel} disabled={busy}>{t("common.cancel")}</button>{/if}
      {#if !setup && error}<button type="button" class="btn" onclick={load}>{t("common.retry")}</button>{/if}
      <button type="submit" class="btn primary" disabled={!setup || busy || code.replace(/\s+/g, "").length < 6}>
        {#if busy}<span class="spinner"></span>{/if}
        {t("totp.submit")}
      </button>
    </div>
  </form>
{/if}

<style>
  .qr {
    display: flex;
    justify-content: center;
  }
  .key {
    display: flex;
    align-items: center;
    gap: 6px;
    padding-block: 6px;
    padding-inline: 10px 6px;
    border-radius: var(--radius-lg);
    background: var(--surface-hover);
  }
  .key code {
    flex: 1;
    min-width: 0;
    font-family: var(--font-mono);
    font-size: 13px;
    letter-spacing: 0.5px;
    overflow-wrap: anywhere;
  }
  .small {
    align-self: flex-start;
    font-size: 12px;
  }
  .code {
    font-family: var(--font-mono);
    font-size: 16px;
    letter-spacing: 3px;
    height: 38px;
  }
  .loading {
    display: flex;
    justify-content: center;
    padding: 24px;
  }
</style>
