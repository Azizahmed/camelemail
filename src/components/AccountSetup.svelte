<script lang="ts">
  import { ChevronRight, KeyRound, ExternalLink, ShieldCheck } from "@lucide/svelte";
  import { api, errorText } from "../lib/api";
  import { toast } from "../lib/ui.svelte";
  import type { Account, NewAccount, ProviderPreset, Security, ServerConfig } from "../lib/types";

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
  const SECURITY: { value: Security; label: string }[] = [
    { value: "tls", label: "SSL/TLS" },
    { value: "starttls", label: "STARTTLS" },
    { value: "none", label: "None" },
  ];

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
  let presets = $state<ProviderPreset[]>([]);
  let guessSeq = 0;

  $effect(() => {
    api.providerPresets().then((p) => (presets = p)).catch(() => {});
  });

  const domain = $derived(email.trim().split("@")[1]?.toLowerCase() ?? "");
  const preset = $derived(domain ? presets.find((p) => p.domains.some((d) => d.toLowerCase() === domain)) : undefined);
  const validEmail = $derived(/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim()));

  // Microsoft (Outlook.com, Hotmail, Live, MSN, Microsoft 365) signs in in the browser.
  // Edit mode: Microsoft accounts (and Hotmail/Outlook ones added with a password, which switch over).
  const microsoftAccount = $derived(
    mode === "edit" &&
      !!account &&
      (account.auth === "microsoft" || preset?.oauth === "microsoft" || account.imap.host.toLowerCase() === "outlook.office365.com"),
  );
  const microsoftEmail = $derived(mode === "add" && preset?.oauth === "microsoft");
  let msState = $state<"idle" | "setup" | "waiting">("idle");
  let clientId = $state("");

  async function signInWithMicrosoft() {
    if (busy) return;
    error = "";
    try {
      if (!(await api.microsoftSignInReady())) {
        msState = "setup";
        return;
      }
    } catch (e) {
      error = errorText(e);
      return;
    }
    busy = true;
    msState = "waiting";
    try {
      if (mode === "edit" && account) {
        await api.reauthorizeAccount(account.id);
        toast("Signed in to " + account.email);
        ondone(account);
      } else {
        const added = await api.addMicrosoftAccount({ email: validEmail ? email.trim() : null, displayName: displayName.trim() || null });
        toast("Added " + added.account.email);
        if (added.warning) toast(added.warning, { kind: "error", duration: 12000 });
        ondone(added.account);
      }
    } catch (e) {
      const msg = errorText(e);
      if (!/cancelled/i.test(msg)) error = msg;
    } finally {
      busy = false;
      msState = "idle";
    }
  }

  async function saveClientId() {
    const id = clientId.trim();
    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id)) {
      error = "The Application (client) ID looks like 00000000-0000-0000-0000-000000000000.";
      return;
    }
    error = "";
    try {
      const s = await api.getSettings();
      await api.setSettings({ ...s, microsoftClientId: id });
      msState = "idle";
      await signInWithMicrosoft();
    } catch (e) {
      error = errorText(e);
    }
  }

  async function guess() {
    if (mode === "edit" || serversTouched || !validEmail) return;
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
    if (microsoftEmail || microsoftAccount) return signInWithMicrosoft();
    if (!imap.host.trim() && !serversTouched) await guess();
    if (!validEmail || !password) {
      error = !validEmail ? "Enter a valid email address." : "Enter your password.";
      return;
    }
    if (!imap.host.trim() || !smtp.host.trim()) {
      error = "Enter the IMAP and SMTP server hosts.";
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
        toast("Account re-authenticated");
        ondone(account);
      } else {
        const created = await api.addAccount(payload);
        toast("Added " + payload.email);
        ondone(created);
      }
    } catch (err) {
      error = errorText(err);
      showServers = true;
    } finally {
      busy = false;
    }
  }
</script>

<form class="setup" class:compact onsubmit={submit} autocomplete="off">
  {#if !compact}
    <h2 class="title">{mode === "add" ? "Add an account" : "Re-authenticate"}</h2>
  {/if}

  {#if mode === "add" || microsoftAccount}
    <div class="ms">
      <button type="button" class="ms-btn" disabled={busy} onclick={signInWithMicrosoft}>
        <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true"><rect width="7.5" height="7.5" fill="#f25022" /><rect x="8.5" width="7.5" height="7.5" fill="#7fba00" /><rect y="8.5" width="7.5" height="7.5" fill="#00a4ef" /><rect x="8.5" y="8.5" width="7.5" height="7.5" fill="#ffb900" /></svg>
        {microsoftAccount ? "Sign in again with Microsoft" : "Sign in with Microsoft"}
      </button>
      <div class="ms-note">
        {microsoftAccount ? `${account?.email} signs in with Microsoft.` : "Outlook.com, Hotmail, Live, MSN and Microsoft 365 (Office 365): mail and calendars"}
      </div>
      {#if msState === "waiting"}
        <div class="ms-wait" role="status">
          <span class="spinner"></span> Finish signing in in your browser…
          <button type="button" class="link" onclick={() => api.cancelSignIn()}>Cancel</button>
        </div>
      {:else if msState === "setup"}
        <div class="ms-setup">
          <b>One-time setup: Remail's Microsoft app ID</b>
          <p>Microsoft only lets registered apps sign in to mail and calendars. Register Remail once with your Microsoft account (free, a few minutes):</p>
          <ol>
            <li>
              Open
              <button type="button" class="link" onclick={() => api.openUrl("https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade")}>
                App registrations <ExternalLink size={11} />
              </button>
              in the Microsoft Entra admin center and choose <b>New registration</b>.
            </li>
            <li>Name it <b>Remail</b>. Under “Supported account types” choose <b>Accounts in any organizational directory and personal Microsoft accounts</b>.</li>
            <li>Under “Redirect URI” pick <b>Public client/native (mobile &amp; desktop)</b> and enter <code>http://localhost</code>.</li>
            <li>Select <b>Register</b>, then copy the <b>Application (client) ID</b> here.</li>
          </ol>
          <div class="field">
            <label for="as-msid">Application (client) ID</label>
            <input id="as-msid" class="input" bind:value={clientId} placeholder="00000000-0000-0000-0000-000000000000" spellcheck="false" />
          </div>
          <div class="actions">
            <button type="button" class="btn" onclick={() => (msState = "idle")}>Cancel</button>
            <button type="button" class="btn primary" onclick={saveClientId}>Save and sign in</button>
          </div>
        </div>
      {/if}
    </div>
    {#if mode === "add"}<div class="or"><span>or use a password</span></div>{/if}
  {/if}

  {#if !microsoftAccount}
  <div class="field">
    <label for="as-name">Name</label>
    <input id="as-name" class="input" bind:value={displayName} placeholder="Your name" autocomplete="name" />
  </div>
  <div class="field">
    <label for="as-email">Email</label>
    <input
      id="as-email"
      class="input"
      type="email"
      bind:value={email}
      onblur={guess}
      placeholder="you@example.com"
      readonly={mode === "edit"}
      autocomplete="email"
    />
  </div>
  {#if microsoftEmail}
    <div class="help">
      <KeyRound size={14} />
      <div>{preset?.passwordHelp}</div>
    </div>
  {:else}
  <div class="field">
    <label for="as-pass">Password</label>
    <input id="as-pass" class="input" type="password" bind:value={password} placeholder={mode === "edit" ? "Re-enter your password" : "Password or app password"} autocomplete="current-password" />
    {#if preset}
      <div class="help">
        <KeyRound size={14} />
        <div>
          <span>{preset.passwordHelp}</span>
          {#if preset.passwordHelpUrl}
            <button type="button" class="link" onclick={() => api.openUrl(preset.passwordHelpUrl!)}>
              Create an app password <ExternalLink size={11} />
            </button>
          {/if}
        </div>
      </div>
    {/if}
  </div>
  {/if}

  {#if !microsoftEmail}
  <button type="button" class="disclosure" aria-expanded={showServers} onclick={() => (showServers = !showServers)}>
    <span class="chev" class:open={showServers}><ChevronRight size={14} /></span>
    Server settings
  </button>

  {#if showServers}
    <div class="servers">
      <div class="srv-title">IMAP (incoming)</div>
      <div class="srv-row">
        <div class="field grow">
          <label for="as-imap-host">Host</label>
          <input id="as-imap-host" class="input" bind:value={imap.host} oninput={touch} placeholder="imap.example.com" />
        </div>
        <div class="field port">
          <label for="as-imap-port">Port</label>
          <input id="as-imap-port" class="input" type="number" min="1" max="65535" bind:value={imap.port} oninput={touch} />
        </div>
        <div class="field sec">
          <label for="as-imap-sec">Security</label>
          <select id="as-imap-sec" class="input" value={imap.security} onchange={(e) => setSecurity("imap", e.currentTarget.value as Security)}>
            {#each SECURITY as s}<option value={s.value}>{s.label}</option>{/each}
          </select>
        </div>
      </div>

      <div class="srv-title">SMTP (outgoing)</div>
      <div class="srv-row">
        <div class="field grow">
          <label for="as-smtp-host">Host</label>
          <input id="as-smtp-host" class="input" bind:value={smtp.host} oninput={touch} placeholder="smtp.example.com" />
        </div>
        <div class="field port">
          <label for="as-smtp-port">Port</label>
          <input id="as-smtp-port" class="input" type="number" min="1" max="65535" bind:value={smtp.port} oninput={touch} />
        </div>
        <div class="field sec">
          <label for="as-smtp-sec">Security</label>
          <select id="as-smtp-sec" class="input" value={smtp.security} onchange={(e) => setSecurity("smtp", e.currentTarget.value as Security)}>
            {#each SECURITY as s}<option value={s.value}>{s.label}</option>{/each}
          </select>
        </div>
      </div>

      <div class="field">
        <label for="as-user">Username</label>
        <input id="as-user" class="input" bind:value={username} placeholder={email || "Same as email"} autocomplete="username" />
      </div>
      <label class="check">
        <input type="checkbox" bind:checked={allowInvalidCerts} />
        Allow self-signed certificates
      </label>
    </div>
  {/if}
  {/if}
  {/if}

  {#if error}
    <div class="error selectable" role="alert">{error}</div>
  {/if}

  <div class="actions">
    {#if oncancel}
      <button type="button" class="btn" onclick={oncancel} disabled={busy}>Cancel</button>
    {/if}
    {#if !microsoftAccount}
      <button type="submit" class="btn primary" disabled={busy || !email.trim() || (!password && !microsoftEmail)}>
        {#if busy && msState !== "waiting"}
          <span class="spinner"></span> Checking connection…
        {:else if microsoftEmail}
          Sign in with Microsoft
        {:else}
          {mode === "add" ? "Connect" : "Save"}
        {/if}
      </button>
    {/if}
  </div>

  <p class="privacy"><ShieldCheck size={13} /> {PRIVACY_NOTE}</p>
</form>

<script lang="ts" module>
  export const PRIVACY_NOTE =
    "Your password is stored in the system keyring. Remail talks to your mail server directly — no third-party cloud.";
</script>

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
    margin-left: 4px;
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
  .ms-setup {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    border: 0.5px solid var(--separator);
    border-radius: var(--radius-lg);
    background: var(--surface);
    font-size: 12.5px;
    line-height: 18px;
  }
  .ms-setup p,
  .ms-setup ol {
    margin: 0;
  }
  .ms-setup ol {
    padding-inline-start: 18px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .ms-setup code {
    font-size: 11.5px;
    user-select: text;
  }
  .or {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 11.5px;
    color: var(--content-muted);
  }
  .or::before,
  .or::after {
    content: "";
    flex: 1;
    border-top: 0.5px solid var(--separator);
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
