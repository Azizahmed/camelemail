<script lang="ts">
  import Select from "./Select.svelte";
  import { ChevronLeft, ChevronRight, KeyRound, ExternalLink, Mail, ShieldCheck, Server } from "@lucide/svelte";
  import { api } from "../lib/api";
  import { autofocus } from "../lib/actions";
  import { errMessage, t } from "../lib/i18n.svelte";
  import { toast } from "../lib/ui.svelte";
  import type { Account, NewAccount, OAuthAccountAdded, ProviderKind, ProviderOption, ProviderPreset, Security, ServerConfig, ServerPreset } from "../lib/types";

  let {
    mode,
    account,
    compact = false,
    ondone,
    oncancel,
  }: {
    mode: "add" | "edit";
    account?: Account;
    compact?: boolean;
    ondone: (a?: Account) => void;
    oncancel?: () => void;
  } = $props();

  const IMAP_PORTS: Record<Security, number> = { tls: 993, starttls: 143, none: 143 };
  const SMTP_PORTS: Record<Security, number> = { tls: 465, starttls: 587, none: 25 };
  const SECURITY = $derived<{ value: Security; label: string }[]>([
    { value: "tls", label: "SSL/TLS" },
    { value: "starttls", label: "STARTTLS" },
    { value: "none", label: t("setup.securityNone") },
  ]);

  const initial = (() => account)();
  let displayName = $state(initial?.displayName ?? "");
  let email = $state(initial?.email ?? "");
  let password = $state("");
  let username = $state(initial?.username ?? "");
  let imap = $state<ServerConfig>(initial ? { ...initial.imap } : { host: "", port: 993, security: "tls" });
  let smtp = $state<ServerConfig>(initial ? { ...initial.smtp } : { host: "", port: 465, security: "tls" });
  let allowInvalidCerts = $state(initial?.allowInvalidCerts ?? false);

  let showServers = $state(false);
  let busy = $state(false);
  let error = $state("");
  // svelte-ignore state_referenced_locally
  let serversTouched = $state(mode === "edit");
  /** Built-in knowledge of well-known providers (password help, Gmail / Microsoft domains). */
  let known = $state<ProviderPreset[]>([]);
  let guessSeq = 0;

  // What the organisation allows (cloud_providers); only asked for when adding.
  let options = $state<ProviderOption[] | null>(null);
  let optionsError = $state("");

  function loadOptions() {
    optionsError = "";
    api.cloudProviders().then((o) => (options = o)).catch((e) => (optionsError = errMessage(e)));
  }

  $effect(() => {
    api.providerPresets().then((p) => (known = p)).catch(() => {});
    if (mode === "add") loadOptions();
  });

  const enabled = $derived((options ?? []).filter((o) => o.enabled));
  const allows = (kind: ProviderKind) => enabled.some((o) => o.kind === kind);
  const imapOption = $derived(enabled.find((o) => o.kind === "imap"));
  /** The organisation's mail servers to pick from under "Other email". */
  const serverPresets = $derived(imapOption?.presets ?? []);
  const manualAllowed = $derived(!!imapOption?.manualServers);

  let picked = $state<ProviderKind | null>(null);
  /** The provider being set up; with a single one allowed there is nothing to choose. */
  const choice = $derived(mode === "add" ? (picked ?? (enabled.length === 1 ? enabled[0].kind : null)) : null);
  /** Under "Other email": the server preset in use, or "manual" for typed-in servers. */
  let server = $state<ServerPreset | "manual" | null>(null);
  const imapServer = $derived(choice === "imap" ? (server ?? (serverPresets.length === 0 && manualAllowed ? "manual" : null)) : null);

  function choose(k: ProviderKind) {
    picked = k;
    server = null;
    error = "";
    serversTouched = false;
  }

  function pickServer(p: ServerPreset | "manual") {
    server = p;
    error = "";
    if (p === "manual") {
      serversTouched = false;
      return;
    }
    imap = { host: p.imapHost, port: p.imapPort, security: p.imapSecurity };
    smtp = { host: p.smtpHost, port: p.smtpPort, security: p.smtpSecurity };
    serversTouched = true;
  }

  const domain = $derived(email.trim().split("@")[1]?.toLowerCase() ?? "");
  const preset = $derived(domain ? known.find((p) => p.domains.some((d) => d.toLowerCase() === domain)) : undefined);
  const validEmail = $derived(/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim()));

  // Accounts that sign in in the browser. Edit mode: Microsoft accounts (and
  // Hotmail/Outlook ones added with a password, which switch over), Google ones.
  const microsoftAccount = $derived(
    mode === "edit" &&
      !!account &&
      (account.auth === "microsoft" || preset?.oauth === "microsoft" || account.imap.host.toLowerCase() === "outlook.office365.com"),
  );
  const googleAccount = $derived(mode === "edit" && !!account && account.auth === "google");
  const oauthKind = $derived<"google" | "microsoft" | null>(
    mode === "edit" ? (googleAccount ? "google" : microsoftAccount ? "microsoft" : null) : choice === "google" || choice === "microsoft" ? choice : null,
  );
  /** An address that belongs under another choice (a Gmail address typed under "Other email"). */
  const otherFamily = $derived(choice === "imap" && preset ? (preset.id === "gmail" ? "google" : preset.oauth === "microsoft" ? "microsoft" : null) : null);
  /** The password form (adding under "Other email" once a server is chosen, or re-entering a password). */
  const passwordForm = $derived(mode === "edit" ? !oauthKind : choice === "imap" && imapServer !== null);
  let waiting = $state(false);

  async function oauthSignIn() {
    if (busy || !oauthKind) return;
    error = "";
    if (oauthKind === "microsoft") {
      try {
        if (!(await api.microsoftSignInReady())) {
          error = t("setup.msNotReady");
          return;
        }
      } catch (e) {
        error = errMessage(e);
        return;
      }
    }
    busy = true;
    waiting = true;
    try {
      if (mode === "edit" && account) {
        await api.reauthorizeAccount(account.id);
        toast(t("setup.signedInTo", { email: account.email }));
        ondone(account);
      } else {
        const name = displayName.trim() || null;
        const r: OAuthAccountAdded | Account =
          oauthKind === "google" ? await api.addGoogleAccount(name) : await api.addMicrosoftAccount({ email: null, displayName: name });
        const added = "account" in r ? r : { account: r, warning: null };
        toast(t("setup.added", { email: added.account.email }));
        if (added.warning) toast(added.warning, { kind: "error", duration: 12000 });
        ondone(added.account);
      }
    } catch (e) {
      const msg = errMessage(e);
      if (!/cancel/i.test(msg)) error = msg;
    } finally {
      busy = false;
      waiting = false;
    }
  }

  async function guess() {
    if (mode === "edit" || serversTouched || imapServer !== "manual" || !validEmail) return;
    const seq = ++guessSeq;
    try {
      const g = await api.guessServers(email.trim());
      if (seq !== guessSeq || serversTouched) return;
      imap = { ...g.imap };
      smtp = { ...g.smtp };
    } catch {
      /* keep manual entry */
    }
  }

  function touch() {
    serversTouched = true;
  }

  function setSecurity(which: "imap" | "smtp", s: Security) {
    touch();
    const cfg = which === "imap" ? imap : smtp;
    const defaults = which === "imap" ? IMAP_PORTS : SMTP_PORTS;
    const wasDefault = Object.values(defaults).includes(cfg.port);
    cfg.security = s;
    if (wasDefault) cfg.port = defaults[s];
  }

  async function submit(e: Event) {
    e.preventDefault();
    if (busy) return;
    if (oauthKind) return oauthSignIn();
    if (!passwordForm) return;
    if (otherFamily) {
      error = allows(otherFamily)
        ? t(otherFamily === "google" ? "setup.useGoogle" : "setup.useMicrosoft")
        : t(otherFamily === "google" ? "setup.googleNotAllowed" : "setup.microsoftNotAllowed");
      return;
    }
    if (!validEmail || !password) {
      error = !validEmail ? t("setup.invalidEmail") : t("setup.enterPassword");
      return;
    }
    if (mode === "add" && imapServer && imapServer !== "manual" && imapServer.domain && domain !== imapServer.domain.toLowerCase()) {
      error = t("setup.presetDomain", { domain: "@" + imapServer.domain });
      return;
    }
    if (!imap.host.trim() && !serversTouched) await guess();
    if (!imap.host.trim() || !smtp.host.trim()) {
      error = t("setup.enterHosts");
      showServers = true;
      return;
    }
    error = "";
    busy = true;
    const payload: NewAccount = {
      email: email.trim(),
      displayName: displayName.trim() || email.trim().split("@")[0],
      username: username.trim() || email.trim(),
      password,
      imap: { ...imap, host: imap.host.trim(), port: Number(imap.port) },
      smtp: { ...smtp, host: smtp.host.trim(), port: Number(smtp.port) },
      allowInvalidCerts,
    };
    try {
      if (mode === "edit" && account) {
        await api.updateAccount(account.id, payload);
        toast(t("setup.reauthed"));
        ondone(account);
      } else {
        const created = await api.addAccount(payload);
        toast(t("setup.added", { email: payload.email }));
        ondone(created);
      }
    } catch (err) {
      error = errMessage(err);
      if (imapServer === "manual" || mode === "edit") showServers = true;
    } finally {
      busy = false;
    }
  }

  /** Server fields: typed-in servers, or re-entering a password-account's details. */
  const serverFields = $derived(mode === "edit" ? !oauthKind : imapServer === "manual");
</script>

{#snippet googleLogo(size: number)}
  <svg width={size} height={size} viewBox="0 0 48 48" aria-hidden="true"><path fill="#4285F4" d="M6 40h7V22L4 15v22a3 3 0 0 0 2 3z"/><path fill="#34A853" d="M35 40h7a3 3 0 0 0 2-3V15l-9 7z"/><path fill="#FBBC04" d="M35 10v12l9-7v-3c0-3-3.4-4.6-5.8-2.8z"/><path fill="#EA4335" d="M13 22V10l11 8.3L35 10v12l-11 8.3z"/><path fill="#C5221F" d="M4 12v3l9 7V10l-3.2-2.8C7.4 5.4 4 7 4 12z"/></svg>
{/snippet}
{#snippet msLogo(size: number)}
  <svg width={size} height={size} viewBox="0 0 16 16" aria-hidden="true"><rect width="7.5" height="7.5" fill="#f25022" /><rect x="8.5" width="7.5" height="7.5" fill="#7fba00" /><rect y="8.5" width="7.5" height="7.5" fill="#00a4ef" /><rect x="8.5" y="8.5" width="7.5" height="7.5" fill="#ffb900" /></svg>
{/snippet}

<form class="setup" class:compact onsubmit={submit} autocomplete="off">
  {#if !compact}
    <h2 class="title">{mode === "add" ? t("setup.titleAdd") : t("setup.titleEdit")}</h2>
  {/if}

  {#if mode === "add" && !choice}
    {#if optionsError}
      <div class="error" role="alert">{optionsError} <button type="button" class="link" onclick={loadOptions}>{t("common.retry")}</button></div>
    {:else if options === null}
      <div class="loading"><span class="spinner"></span></div>
    {:else if enabled.length === 0}
      <div class="error" role="alert">{t("setup.noProviders")}</div>
    {:else}
      <div class="choices">
        {#if allows("google")}
          <button type="button" class="choice" onclick={() => choose("google")}>
            {@render googleLogo(18)}
            <span class="choice-name">{t("setup.google")}</span>
          </button>
        {/if}
        {#if allows("microsoft")}
          <button type="button" class="choice" onclick={() => choose("microsoft")}>
            {@render msLogo(16)}
            <span class="choice-name">{t("setup.microsoft")}</span>
          </button>
        {/if}
        {#if allows("imap")}
          <button type="button" class="choice" onclick={() => choose("imap")}>
            <Mail size={17} />
            <span class="choice-name">{t("setup.other")}</span>
          </button>
        {/if}
      </div>
    {/if}
    {#if oncancel}
      <div class="actions"><button type="button" class="btn" onclick={oncancel}>{t("common.cancel")}</button></div>
    {/if}
  {:else}

  {#if mode === "add" && enabled.length > 1}
    <button type="button" class="back" onclick={() => { picked = null; server = null; error = ""; }}><ChevronLeft size={14} class="rtl-flip" /> {t("setup.allProviders")}</button>
  {:else if mode === "add" && choice === "imap" && server !== null && serverPresets.length > 0}
    <button type="button" class="back" onclick={() => { server = null; error = ""; }}><ChevronLeft size={14} class="rtl-flip" /> {t("setup.pickPreset")}</button>
  {/if}

  {#if oauthKind}
    <div class="ms">
      <button type="button" class="ms-btn" disabled={busy} onclick={oauthSignIn} use:autofocus={mode === "add"}>
        {#if oauthKind === "google"}{@render googleLogo(17)}{:else}{@render msLogo(16)}{/if}
        {#if oauthKind === "google"}
          {mode === "edit" ? t("setup.googleAgain") : t("setup.googleSignIn")}
        {:else}
          {mode === "edit" ? t("setup.msAgain") : t("setup.msSignIn")}
        {/if}
      </button>
      <div class="ms-note">
        {#if mode === "edit" && account}
          {t("setup.oauthAccount", { email: account.email, provider: oauthKind === "google" ? "Google" : "Microsoft" })}
        {:else}
          {oauthKind === "google" ? t("setup.googleNote") : t("setup.msNote")}
        {/if}
      </div>
      {#if waiting}
        <div class="ms-wait" role="status">
          <span class="spinner"></span> {t("setup.waiting")}
          <button type="button" class="link" onclick={() => api.cancelSignIn()}>{t("common.cancel")}</button>
        </div>
      {/if}
    </div>
  {:else if mode === "add" && choice === "imap" && imapServer === null}
    {#if serverPresets.length === 0}
      <div class="error" role="alert">{t("setup.noServers")}</div>
    {:else}
      <div class="au-label">{t("setup.pickPreset")}</div>
      <div class="choices">
        {#each serverPresets as p (p.name + p.imapHost)}
          <button type="button" class="choice" onclick={() => pickServer(p)}>
            <Server size={16} />
            <span class="choice-text">
              <span class="choice-name" dir="auto">{p.name || p.imapHost}</span>
              {#if p.domain}<span class="choice-sub" dir="ltr">@{p.domain}</span>{/if}
            </span>
          </button>
        {/each}
        {#if manualAllowed}
          <button type="button" class="choice" onclick={() => pickServer("manual")}>
            <Mail size={16} />
            <span class="choice-name">{t("setup.otherServer")}</span>
          </button>
        {/if}
      </div>
    {/if}
  {/if}

  {#if passwordForm}
  {#if mode === "add" && imapServer && imapServer !== "manual"}
    <div class="preset-name"><Server size={14} /> <span dir="auto">{imapServer.name || imapServer.imapHost}</span></div>
  {/if}
  <div class="field">
    <label for="as-name">{t("common.name")}</label>
    <input id="as-name" class="input" dir="auto" bind:value={displayName} placeholder={t("common.namePlaceholder")} autocomplete="name" />
  </div>
  <div class="field">
    <label for="as-email">{t("common.email")}</label>
    <input
      id="as-email"
      class="input"
      type="email"
      dir="ltr"
      bind:value={email}
      onblur={guess}
      placeholder={imapServer && imapServer !== "manual" && imapServer.domain ? `you@${imapServer.domain}` : t("common.emailPlaceholder")}
      readonly={mode === "edit"}
      autocomplete="email"
      use:autofocus={mode === "add"}
    />
  </div>
  <div class="field">
    <label for="as-pass">{t("common.password")}</label>
    <input id="as-pass" class="input" type="password" dir="ltr" bind:value={password} placeholder={mode === "edit" ? t("setup.passwordAgain") : t("setup.passwordPlaceholder")} autocomplete="current-password" />
    {#if preset && !otherFamily}
      {@const help = preset}
      <div class="help">
        <KeyRound size={14} />
        <div>
          <span>{help.passwordHelp}</span>
          {#if help.passwordHelpUrl}
            <button type="button" class="link" onclick={() => api.openUrl(help.passwordHelpUrl!)}>
              {t("setup.appPassword")} <ExternalLink size={11} />
            </button>
          {/if}
        </div>
      </div>
    {/if}
  </div>

  {#if serverFields}
  <button type="button" class="disclosure" aria-expanded={showServers} onclick={() => (showServers = !showServers)}>
    <span class="chev" class:open={showServers}><ChevronRight size={14} class="rtl-flip" /></span>
    {t("setup.servers")}
  </button>

  {#if showServers}
    <div class="servers">
      <div class="srv-title">{t("setup.imap")}</div>
      <div class="srv-row">
        <div class="field grow">
          <label for="as-imap-host">{t("setup.host")}</label>
          <input id="as-imap-host" class="input" dir="ltr" bind:value={imap.host} oninput={touch} placeholder="imap.example.com" />
        </div>
        <div class="field port">
          <label for="as-imap-port">{t("setup.port")}</label>
          <input id="as-imap-port" class="input" dir="ltr" type="number" min="1" max="65535" bind:value={imap.port} oninput={touch} />
        </div>
        <div class="field sec">
          <label for="as-imap-sec">{t("setup.security")}</label>
          <Select id="as-imap-sec" value={imap.security} options={SECURITY} onchange={(v) => setSecurity("imap", v)} />
        </div>
      </div>

      <div class="srv-title">{t("setup.smtp")}</div>
      <div class="srv-row">
        <div class="field grow">
          <label for="as-smtp-host">{t("setup.host")}</label>
          <input id="as-smtp-host" class="input" dir="ltr" bind:value={smtp.host} oninput={touch} placeholder="smtp.example.com" />
        </div>
        <div class="field port">
          <label for="as-smtp-port">{t("setup.port")}</label>
          <input id="as-smtp-port" class="input" dir="ltr" type="number" min="1" max="65535" bind:value={smtp.port} oninput={touch} />
        </div>
        <div class="field sec">
          <label for="as-smtp-sec">{t("setup.security")}</label>
          <Select id="as-smtp-sec" value={smtp.security} options={SECURITY} onchange={(v) => setSecurity("smtp", v)} />
        </div>
      </div>

      <div class="field">
        <label for="as-user">{t("setup.username")}</label>
        <input id="as-user" class="input" dir="ltr" bind:value={username} placeholder={email || t("setup.usernamePlaceholder")} autocomplete="username" />
      </div>
      <label class="check">
        <input type="checkbox" bind:checked={allowInvalidCerts} />
        {t("setup.selfSigned")}
      </label>
    </div>
  {/if}
  {/if}
  {/if}

  {#if error}
    <div class="error selectable" role="alert">{error}</div>
  {/if}

  {#if oncancel || passwordForm}
  <div class="actions">
    {#if oncancel}
      <button type="button" class="btn" onclick={oncancel} disabled={busy}>{t("common.cancel")}</button>
    {/if}
    {#if passwordForm}
      <button type="submit" class="btn primary" disabled={busy || !email.trim() || !password}>
        {#if busy}
          <span class="spinner"></span> {t("setup.checking")}
        {:else}
          {mode === "add" ? t("setup.connect") : t("common.save")}
        {/if}
      </button>
    {/if}
  </div>
  {/if}

  {/if}

  <p class="privacy"><ShieldCheck size={13} /> {t("setup.privacy")}</p>
</form>

<style>
  .setup {
    display: flex;
    flex-direction: column;
    gap: 14px;
    width: 100%;
    max-width: 420px;
    padding: 24px;
    background: var(--surface-elevated);
    border: 0.5px solid var(--separator);
    border-radius: var(--radius-xl);
    box-shadow: var(--shadow-dropdown);
  }
  .setup.compact {
    max-width: none;
    padding: 0;
    background: none;
    border: none;
    box-shadow: none;
  }
  .title {
    margin: 0 0 2px;
    font-size: 18px;
    font-weight: 600;
    line-height: 24px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-width: 0;
  }
  .field label {
    font-size: 12px;
    font-weight: 500;
    color: var(--content-subtle);
  }
  .help {
    display: flex;
    gap: 8px;
    margin-top: 4px;
    padding: 9px 10px;
    border-radius: var(--radius-lg);
    background: var(--accent-soft);
    font-size: 12px;
    line-height: 17px;
    color: var(--content);
  }
  .help :global(svg) {
    flex: none;
    margin-top: 2px;
    color: var(--accent);
  }
  .link {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    border: none;
    background: none;
    padding: 0;
    margin-inline-start: 4px;
    color: var(--accent);
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
  }
  .link:hover {
    text-decoration: underline;
  }
  .disclosure {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    align-self: flex-start;
    border: none;
    background: none;
    padding: 2px 0;
    font-size: 12px;
    font-weight: 500;
    color: var(--content-subtle);
  }
  .disclosure:hover {
    color: var(--content);
  }
  .chev {
    display: inline-flex;
    transition: transform var(--normal);
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .servers {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 12px;
    border: 0.5px solid var(--separator);
    border-radius: var(--radius-lg);
    background: var(--surface);
  }
  .srv-title {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.4px;
    color: var(--content-muted);
  }
  .srv-row {
    display: flex;
    gap: 8px;
  }
  .grow {
    flex: 1;
  }
  .port {
    width: 76px;
  }
  .sec {
    width: 108px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
  }
  .check input {
    accent-color: var(--accent);
  }
  .error {
    padding: 9px 10px;
    border-radius: var(--radius-lg);
    background: color-mix(in srgb, var(--indicator-error) 12%, transparent);
    color: var(--indicator-error);
    font-size: 12px;
    line-height: 17px;
    word-break: break-word;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .ms {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .ms-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 10px;
    height: 38px;
    border: 0.5px solid var(--border-tertiary);
    border-radius: var(--radius-lg);
    background: var(--surface-overlay);
    color: var(--content);
    font-size: 14px;
    font-weight: 600;
  }
  .ms-btn:hover:not(:disabled) {
    background: var(--surface-hover);
  }
  .ms-note {
    font-size: 12px;
    text-align: center;
    color: var(--content-subtle);
  }
  .ms-wait {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    font-size: 13px;
  }
  .choices {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .choice {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 44px;
    padding: 0 14px;
    border-radius: var(--radius-lg);
    border: 0.5px solid var(--separator);
    background: var(--surface-overlay);
    color: var(--content);
    font-size: 13.5px;
    font-weight: 500;
    text-align: start;
  }
  .choice:hover {
    border-color: var(--content-muted);
    background: var(--surface-hover);
  }
  .choice-text {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .choice-sub {
    font-size: 11.5px;
    font-weight: 400;
    color: var(--content-subtle);
  }
  .preset-name {
    display: flex;
    align-items: center;
    gap: 6px;
    font-weight: 600;
  }
  .preset-name :global(svg) {
    color: var(--icon);
  }
  .loading {
    display: flex;
    justify-content: center;
    padding: 16px;
  }
  .choice :global(svg) {
    flex: none;
    color: var(--icon);
  }
  .back {
    align-self: flex-start;
    display: inline-flex;
    align-items: center;
    gap: 2px;
    border: none;
    background: none;
    padding: 0;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .back:hover {
    color: var(--content);
  }
  .privacy {
    display: flex;
    gap: 6px;
    margin: 0;
    font-size: 11px;
    line-height: 15px;
    color: var(--content-muted);
  }
  .privacy :global(svg) {
    flex: none;
    margin-top: 1px;
  }
</style>
