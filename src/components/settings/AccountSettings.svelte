<!--
  Settings › Account: the Camel Mail account itself (profile, password, two-step
  verification, signed-in devices, sign out). Mailboxes are under Settings ›
  Mailboxes.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { LogOut, Monitor, Smartphone } from "@lucide/svelte";
  import { api } from "../../lib/api";
  import { cloud, refreshCloud, setMe, signOut } from "../../lib/cloud.svelte";
  import { errMessage, intlLocale, t, type MessageKey } from "../../lib/i18n.svelte";
  import { MIN_LENGTH, personalWords } from "../../lib/password";
  import type { Device, Me } from "../../lib/types";
  import { confirmDialog, toast, toastError } from "../../lib/ui.svelte";
  import PasswordField from "../auth/PasswordField.svelte";
  import TwoFactorSetup from "../auth/TwoFactorSetup.svelte";

  const me = $derived<Me | null>(cloud.status?.me ?? null);

  // ---- profile
  let name = $state("");
  let nameFor = "";
  $effect(() => {
    // Follow the server's name until it's edited here.
    if (me && me.id + me.name !== nameFor) {
      nameFor = me.id + me.name;
      name = me.name;
    }
  });

  async function saveName() {
    const v = name.trim();
    if (!me || !v || v === me.name) {
      if (me && !v) name = me.name;
      return;
    }
    try {
      setMe(await api.cloudUpdateMe(v));
      toast(t("acct.nameSaved"));
    } catch (e) {
      toastError(errMessage(e));
    }
  }

  // ---- password
  let pwOpen = $state(false);
  let current = $state("");
  let next = $state("");
  let confirm = $state("");
  let pwBusy = $state(false);
  let pwError = $state("");

  function closePassword() {
    pwOpen = false;
    current = next = confirm = pwError = "";
  }

  async function changePassword(e: Event) {
    e.preventDefault();
    if (pwBusy) return;
    if ([...next].length < MIN_LENGTH) return void (pwError = t("pw.tooShort"));
    if (next !== confirm) return void (pwError = t("pw.mismatch"));
    pwBusy = true;
    pwError = "";
    try {
      await api.cloudChangePassword(current, next);
      closePassword();
      toast(t("acct.passwordChanged"), { kind: "success", duration: 5000 });
      void loadDevices();
    } catch (err) {
      pwError = errMessage(err);
    } finally {
      pwBusy = false;
    }
  }

  // ---- two-step verification
  let twofa = $state<"idle" | "setup" | "disable">("idle");
  let disPassword = $state("");
  let disCode = $state("");
  let disBusy = $state(false);
  let disError = $state("");

  async function refreshMe() {
    try {
      setMe(await api.cloudMe());
    } catch {
      await refreshCloud();
    }
  }

  async function disable2fa(e: Event) {
    e.preventDefault();
    if (disBusy) return;
    disBusy = true;
    disError = "";
    try {
      await api.cloudTotpDisable(disPassword, disCode.replace(/\s+/g, ""));
      twofa = "idle";
      disPassword = disCode = "";
      toast(t("acct.twofaDisabled"));
      await refreshMe();
    } catch (err) {
      disError = errMessage(err);
    } finally {
      disBusy = false;
    }
  }

  // ---- devices
  let devices = $state<Device[] | null>(null);
  let devicesError = $state("");

  async function loadDevices() {
    try {
      devices = await api.cloudDevices();
      devicesError = "";
    } catch (e) {
      devicesError = errMessage(e);
    }
  }

  const sortedDevices = $derived(
    [...(devices ?? [])].sort((a, b) => Number(b.current) - Number(a.current) || (b.lastSeenAt ?? 0) - (a.lastSeenAt ?? 0)),
  );

  const PLATFORMS: Record<string, string> = { linux: "Linux", windows: "Windows", macos: "macOS", ios: "iOS", android: "Android" };
  const mobile = (p: string) => p === "ios" || p === "android";

  function when(ms: number | null): string {
    if (!ms) return "";
    try {
      return new Intl.DateTimeFormat(intlLocale(), { dateStyle: "medium", timeStyle: "short" }).format(new Date(ms));
    } catch {
      return new Date(ms).toLocaleString();
    }
  }

  /** Platform, app version, last active: shown with separators. */
  function deviceMeta(d: Device): string[] {
    const parts = [PLATFORMS[d.platform] ?? d.platform];
    if (d.appVersion) parts.push(`Camel Mail ${d.appVersion}`);
    if (d.lastSeenAt && !d.current) parts.push(t("acct.lastSeen", { when: when(d.lastSeenAt) }));
    return parts;
  }

  async function signOutDevice(d: Device) {
    if (d.current) return signOutHere();
    const r = await confirmDialog({
      title: t("acct.signOutDeviceTitle", { name: d.name }),
      message: t("acct.signOutDeviceBody", { name: d.name }),
      confirmLabel: t("acct.signOutDevice"),
      cancelLabel: t("common.cancel"),
      danger: true,
    });
    if (r !== "confirm") return;
    try {
      await api.cloudRevokeDevice(d.id);
      toast(t("acct.deviceSignedOut"));
      await loadDevices();
    } catch (e) {
      toastError(errMessage(e));
    }
  }

  // ---- sign out
  async function signOutHere() {
    const r = await confirmDialog({
      title: t("acct.signOutTitle"),
      message: t("acct.signOutBody"),
      confirmLabel: t("common.signOut"),
      cancelLabel: t("common.cancel"),
      danger: true,
    });
    if (r !== "confirm") return;
    try {
      await signOut();
    } catch (e) {
      toastError(errMessage(e));
    }
  }

  onMount(() => {
    void refreshMe();
    void loadDevices();
  });
</script>

{#snippet row(label: string, desc: string | null, control: import("svelte").Snippet)}
  <div class="row">
    <div class="row-text">
      <div class="row-label">{label}</div>
      {#if desc}<div class="row-desc">{desc}</div>{/if}
    </div>
    <div class="row-control">{@render control()}</div>
  </div>
{/snippet}

<h1>{t("settings.account")}</h1>

{#if me}
  <div class="group-title">{t("acct.profile")}</div>
  {@render row(t("common.name"), null, nameCtl)}
  {#snippet nameCtl()}
    <input class="input narrow" dir="auto" aria-label={t("common.name")} bind:value={name} onblur={saveName} onkeydown={(e) => e.key === "Enter" && e.currentTarget.blur()} />
  {/snippet}
  {@render row(t("common.email"), null, emailCtl)}
  {#snippet emailCtl()}<span class="value selectable" dir="ltr">{me!.email}</span>{/snippet}
  {#if me.orgName}
    {@render row(t("acct.organisation"), null, orgCtl)}
    {#snippet orgCtl()}
      <span class="value"><span dir="auto">{me!.orgName}</span> · {t(`acct.role.${me!.role}` as MessageKey)}</span>
    {/snippet}
  {/if}

  <div class="group-title">{t("acct.passwordTitle")}</div>
  {#if pwOpen}
    <form class="card au-form" onsubmit={changePassword}>
      <PasswordField id="ac-current" label={t("acct.current")} bind:value={current} focus />
      <PasswordField id="ac-new" label={t("acct.new")} bind:value={next} autocomplete="new-password" meter personal={personalWords(me.name, me.email)} />
      <PasswordField id="ac-confirm" label={t("acct.confirm")} bind:value={confirm} autocomplete="new-password" />
      <p class="au-help">{t("acct.passwordDesc")}</p>
      {#if pwError}<div class="au-error" role="alert">{pwError}</div>{/if}
      <div class="au-actions">
        <button type="button" class="btn" onclick={closePassword} disabled={pwBusy}>{t("common.cancel")}</button>
        <button type="submit" class="btn primary" disabled={pwBusy || !current || !next || !confirm}>
          {#if pwBusy}<span class="spinner"></span>{/if}
          {t("acct.changePassword")}
        </button>
      </div>
    </form>
  {:else}
    {@render row(t("acct.changePassword"), t("acct.passwordDesc"), pwCtl)}
    {#snippet pwCtl()}<button class="btn" onclick={() => (pwOpen = true)}>{t("acct.changePassword")}</button>{/snippet}
  {/if}

  <div class="group-title">{t("acct.twofa")}</div>
  {#if twofa === "setup"}
    <div class="card">
      <TwoFactorSetup
        ondone={() => {
          twofa = "idle";
          void refreshMe();
        }}
        oncancel={() => (twofa = "idle")}
      />
    </div>
  {:else if twofa === "disable"}
    <form class="card au-form" onsubmit={disable2fa}>
      <p class="au-help">{t("acct.twofaDisableBody")}</p>
      <PasswordField id="ac-dis-pw" label={t("common.password")} bind:value={disPassword} focus />
      <div class="au-field">
        <label for="ac-dis-code">{t("twofa.code")}</label>
        <input id="ac-dis-code" class="input code" dir="ltr" inputmode="numeric" autocomplete="one-time-code" maxlength="9" placeholder="123456" bind:value={disCode} />
      </div>
      {#if disError}<div class="au-error" role="alert">{disError}</div>{/if}
      <div class="au-actions">
        <button type="button" class="btn" onclick={() => { twofa = "idle"; disError = ""; }} disabled={disBusy}>{t("common.cancel")}</button>
        <button type="submit" class="btn danger" disabled={disBusy || !disPassword || disCode.replace(/\s+/g, "").length < 6}>
          {#if disBusy}<span class="spinner"></span>{/if}
          {t("acct.twofaDisable")}
        </button>
      </div>
    </form>
  {:else}
    {@render row(t("acct.twofa"), me.totp && me.policies?.require2fa ? t("acct.twofaRequired") : t("acct.twofaDesc"), tfCtl)}
    {#snippet tfCtl()}
      <span class="badge" class:on={me!.totp}>{me!.totp ? t("acct.twofaOn") : t("acct.twofaOff")}</span>
      {#if !me!.totp}
        <button class="btn" onclick={() => (twofa = "setup")}>{t("acct.twofaEnable")}</button>
      {:else if !me!.policies?.require2fa}
        <button class="btn" onclick={() => (twofa = "disable")}>{t("acct.twofaDisable")}</button>
      {/if}
    {/snippet}
  {/if}

  <div class="group-title">{t("acct.devices")}</div>
  <p class="row-desc devices-desc">{t("acct.devicesDesc")}</p>
  {#if devicesError}
    <div class="au-error" role="alert">{devicesError} <button class="au-link" onclick={loadDevices}>{t("common.retry")}</button></div>
  {:else if devices === null}
    <div class="loading"><span class="spinner"></span></div>
  {:else}
    <div class="list">
      {#each sortedDevices as d (d.id)}
        <div class="device">
          <span class="dev-icon">{#if mobile(d.platform)}<Smartphone size={17} />{:else}<Monitor size={17} />{/if}</span>
          <span class="dev-text">
            <span class="dev-name">
              <span dir="auto">{d.name}</span>
              {#if d.current}<span class="badge on">{t("acct.thisDevice")}</span>{/if}
            </span>
            <span class="dev-meta">
              {#each deviceMeta(d) as part, i (i)}
                {#if i > 0}<span aria-hidden="true">·</span>{/if}
                <bdi>{part}</bdi>
              {/each}
            </span>
          </span>
          <button class="btn" onclick={() => signOutDevice(d)}>{t("acct.signOutDevice")}</button>
        </div>
      {/each}
    </div>
    {#if sortedDevices.length <= 1}<p class="row-desc">{t("acct.noDevices")}</p>{/if}
  {/if}

  <div class="group-title">{t("common.signOut")}</div>
  {@render row(t("common.signOut"), t("acct.signOutDesc"), outCtl)}
  {#snippet outCtl()}
    <button class="btn danger" onclick={signOutHere}><LogOut size={14} class="rtl-flip" /> {t("common.signOut")}</button>
  {/snippet}
{:else}
  <div class="loading"><span class="spinner"></span></div>
{/if}

<style>
  h1 {
    margin: 0 0 18px;
    font-size: 20px;
    line-height: 26px;
    font-weight: 600;
  }
  .group-title {
    margin: 24px 0 2px;
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
  .row-text {
    min-width: 0;
  }
  .row-label {
    font-size: 13px;
    font-weight: 500;
  }
  .row-desc {
    margin: 2px 0 0;
    font-size: 12px;
    line-height: 16px;
    color: var(--content-subtle);
  }
  .row-control {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .narrow {
    width: 240px;
  }
  .value {
    font-size: 13px;
    color: var(--content-subtle);
  }
  .card {
    margin-top: 10px;
    padding: 16px;
    border: 0.5px solid var(--separator);
    border-radius: var(--radius-xl);
    background: var(--surface-elevated);
  }
  .badge {
    font-size: 11px;
    font-weight: 500;
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--surface-hover);
    color: var(--content-subtle);
  }
  .badge.on {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .code {
    font-family: var(--font-mono);
    letter-spacing: 2px;
  }
  .devices-desc {
    margin-bottom: 10px;
  }
  .list {
    border: 0.5px solid var(--separator);
    border-radius: var(--radius-xl);
    background: var(--surface-elevated);
    overflow: hidden;
    margin-bottom: 8px;
  }
  .device {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 11px 14px;
  }
  .device + .device {
    border-top: 0.5px solid var(--separator-subtle);
  }
  .dev-icon {
    flex: none;
    display: inline-flex;
    color: var(--icon);
  }
  .dev-text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .dev-name {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    font-weight: 500;
  }
  .dev-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0 6px;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .loading {
    display: flex;
    justify-content: center;
    padding: 24px;
  }
  @media (max-width: 560px) {
    .row {
      flex-direction: column;
      align-items: stretch;
      gap: 8px;
    }
    .narrow {
      width: 100%;
    }
  }
</style>
