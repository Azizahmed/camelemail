<!--
  Settings › Admin › Email providers: which mailboxes people can add (Google,
  Microsoft, other IMAP/SMTP), the OAuth apps Camel Mail signs in with, and
  the company's own mail servers. Saving bumps the org config version and
  every device picks the change up.
-->
<script lang="ts">
  import { ChevronRight, ExternalLink, Mail, Plus, Server, Trash2, TriangleAlert } from "@lucide/svelte";
  import { api } from "../../../lib/api";
  import { presetProblems, providerProblems } from "../../../lib/admin";
  import { errMessage, t } from "../../../lib/i18n.svelte";
  import type { AdminConfig, OrgProviders, Security, ServerPreset } from "../../../lib/types";
  import { toast, toastError } from "../../../lib/ui.svelte";
  import Select from "../../Select.svelte";
  import Toggle from "../../Toggle.svelte";

  let { config, onsaved }: { config: AdminConfig; onsaved: (c: AdminConfig) => void } = $props();

  const GOOGLE_CONSOLE = "https://console.cloud.google.com/apis/credentials";
  const ENTRA_APPS = "https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade";
  const FIXED_TENANTS = ["common", "organizations", "consumers"] as const;
  type TenantMode = (typeof FIXED_TENANTS)[number] | "one";

  const SECURITY = $derived<{ value: Security; label: string }[]>([
    { value: "tls", label: "SSL/TLS" },
    { value: "starttls", label: "STARTTLS" },
    { value: "none", label: t("setup.securityNone") },
  ]);
  const IMAP_PORTS: Record<Security, number> = { tls: 993, starttls: 143, none: 143 };
  const SMTP_PORTS: Record<Security, number> = { tls: 465, starttls: 587, none: 25 };

  /** The saved state, without the (write-only) Google secret. */
  function clean(p: OrgProviders): OrgProviders {
    return {
      ...p,
      googleApp: { clientId: p.googleApp.clientId, tenant: "" },
      microsoftApp: { clientId: p.microsoftApp.clientId, tenant: p.microsoftApp.tenant || "common" },
      presets: p.presets.map((x) => ({ ...x })),
    };
  }

  const initial = (() => config)();
  let p = $state<OrgProviders>(clean(initial.providers));
  let saved = $state(JSON.stringify(clean(initial.providers)));
  let secretSet = $state(initial.googleSecretSet);
  let secret = $state("");
  let clearSecret = $state(false);
  let tenantMode = $state<TenantMode>(modeOf(initial.providers.microsoftApp.tenant));
  let editing = $state<number | null>(null);
  let showErrors = $state(false);
  let saving = $state(false);

  function modeOf(tenant: string): TenantMode {
    const t = (tenant || "common").toLowerCase();
    return (FIXED_TENANTS as readonly string[]).includes(t) ? (t as TenantMode) : "one";
  }

  /** null keeps the saved secret, "" clears it. */
  const secretArg = $derived(clearSecret ? "" : secret.trim() ? secret.trim() : null);
  const dirty = $derived(JSON.stringify(p) !== saved || secretArg !== null);
  const problems = $derived.by(() => {
    const out = providerProblems(p, secretArg);
    if (tenantMode === "one" && !p.microsoftApp.tenant.trim()) out.tenant = "admin.prov.badTenant";
    return out;
  });
  const shown = $derived(showErrors ? problems : {});
  const editProblems = $derived(editing !== null && p.presets[editing] && showErrors ? presetProblems(p.presets[editing]) : {});
  const allOff = $derived(!p.google && !p.microsoft && !p.imap);

  const TENANTS = $derived<{ value: TenantMode; label: string }[]>([
    { value: "common", label: t("admin.prov.tenant.common") },
    { value: "organizations", label: t("admin.prov.tenant.organizations") },
    { value: "consumers", label: t("admin.prov.tenant.consumers") },
    { value: "one", label: t("admin.prov.tenant.one") },
  ]);

  function setTenantMode(m: TenantMode) {
    tenantMode = m;
    p.microsoftApp.tenant = m === "one" ? "" : m;
  }

  function addPreset() {
    p.presets.push({ name: "", domain: "", imapHost: "", imapPort: 993, imapSecurity: "tls", smtpHost: "", smtpPort: 465, smtpSecurity: "tls", caldavUrl: "" });
    editing = p.presets.length - 1;
  }

  function removePreset(i: number) {
    p.presets.splice(i, 1);
    editing = null;
  }

  function doneEditing() {
    if (editing !== null && Object.keys(presetProblems(p.presets[editing])).length) {
      showErrors = true;
      return;
    }
    editing = null;
  }

  function setSecurity(x: ServerPreset, which: "imap" | "smtp", s: Security) {
    if (which === "imap") {
      if (x.imapPort === IMAP_PORTS[x.imapSecurity]) x.imapPort = IMAP_PORTS[s];
      x.imapSecurity = s;
    } else {
      if (x.smtpPort === SMTP_PORTS[x.smtpSecurity]) x.smtpPort = SMTP_PORTS[s];
      x.smtpSecurity = s;
    }
  }

  async function save() {
    showErrors = true;
    if (Object.keys(problems).length) {
      // Open the first server with a problem.
      const bad = Object.keys(problems).find((k) => k.startsWith("preset"));
      if (bad && editing === null) editing = Number(bad.slice(6));
      toast(t("admin.prov.fix"), { kind: "error" });
      return;
    }
    saving = true;
    try {
      const c = await api.adminSetProviders($state.snapshot(p) as OrgProviders, secretArg);
      p = clean(c.providers);
      saved = JSON.stringify(p);
      tenantMode = modeOf(c.providers.microsoftApp.tenant);
      secretSet = c.googleSecretSet;
      secret = "";
      clearSecret = false;
      showErrors = false;
      editing = null;
      onsaved(c);
      toast(t("admin.saved"), { kind: "success" });
    } catch (e) {
      toastError(errMessage(e));
    } finally {
      saving = false;
    }
  }

  function discard() {
    p = JSON.parse(saved);
    tenantMode = modeOf(p.microsoftApp.tenant);
    secret = "";
    clearSecret = false;
    showErrors = false;
    editing = null;
  }
</script>

{#snippet logo(letter: string)}
  <span class="logo" aria-hidden="true">{letter}</span>
{/snippet}

<p class="intro">{t("admin.prov.desc")}</p>
{#if allOff}
  <div class="warn"><TriangleAlert size={14} /> {t("admin.prov.allOff")}</div>
{/if}

<!-- Google -->
<div class="card" class:off={!p.google}>
  <div class="card-head">
    {@render logo("G")}
    <div class="row-text">
      <div class="row-label">Google</div>
      <div class="row-desc">{t("admin.prov.googleDesc")}</div>
    </div>
    <Toggle label="Google" checked={p.google} onchange={(v) => (p.google = v)} />
  </div>
  <div class="card-body">
    {#if p.google && !p.googleApp.clientId.trim()}
      <div class="warn"><TriangleAlert size={14} /> {t("admin.prov.needsId")}</div>
    {/if}
    <div class="field">
      <label for="adm-g-id">{t("admin.prov.clientId")}</label>
      <input id="adm-g-id" class="input" class:bad={shown.googleId} dir="ltr" bind:value={p.googleApp.clientId} placeholder="1234567890-abc.apps.googleusercontent.com" spellcheck="false" autocomplete="off" />
      {#if shown.googleId}<div class="err">{t(shown.googleId)}</div>{/if}
    </div>
    <div class="field">
      <label for="adm-g-secret">
        {t("admin.prov.clientSecret")}
        <span class="state" class:set={secretSet && !clearSecret}>{secretSet && !clearSecret ? t("admin.set") : t("admin.notSet")}</span>
      </label>
      <div class="inline">
        <input
          id="adm-g-secret"
          class="input"
          class:bad={shown.googleSecret}
          type="password"
          dir={secret ? "ltr" : undefined}
          bind:value={secret}
          oninput={() => (clearSecret = false)}
          placeholder={secretSet && !clearSecret ? t("admin.keepSecret") : "GOCSPX-…"}
          autocomplete="off"
          spellcheck="false"
        />
        {#if secretSet && !clearSecret && !secret}<button class="btn" onclick={() => (clearSecret = true)}>{t("admin.clear")}</button>{/if}
        {#if clearSecret}<button class="btn" onclick={() => (clearSecret = false)}>{t("admin.keep")}</button>{/if}
      </div>
      {#if shown.googleSecret}<div class="err">{t(shown.googleSecret)}</div>{/if}
      <div class="hint">{clearSecret ? t("admin.willClear") : t("admin.prov.googleSecretHint")}</div>
    </div>
    <div class="field">
      <div class="hint">{t("admin.prov.googleHint")}</div>
      <button class="link" onclick={() => api.openUrl(GOOGLE_CONSOLE)}><ExternalLink size={12} /> console.cloud.google.com</button>
    </div>
  </div>
</div>

<!-- Microsoft -->
<div class="card" class:off={!p.microsoft}>
  <div class="card-head">
    {@render logo("M")}
    <div class="row-text">
      <div class="row-label">Microsoft</div>
      <div class="row-desc">{t("admin.prov.microsoftDesc")}</div>
    </div>
    <Toggle label="Microsoft" checked={p.microsoft} onchange={(v) => (p.microsoft = v)} />
  </div>
  <div class="card-body">
    {#if p.microsoft && !p.microsoftApp.clientId.trim()}
      <div class="warn"><TriangleAlert size={14} /> {t("admin.prov.needsId")}</div>
    {/if}
    <div class="field">
      <label for="adm-ms-id">{t("admin.prov.appId")}</label>
      <input id="adm-ms-id" class="input" class:bad={shown.msId} dir="ltr" bind:value={p.microsoftApp.clientId} placeholder="00000000-0000-0000-0000-000000000000" spellcheck="false" autocomplete="off" />
      {#if shown.msId}<div class="err">{t(shown.msId)}</div>{/if}
    </div>
    <div class="field">
      <label for="adm-ms-tenant">{t("admin.prov.accounts")}</label>
      <Select id="adm-ms-tenant" value={tenantMode} options={TENANTS} onchange={setTenantMode} />
    </div>
    {#if tenantMode === "one"}
      <div class="field">
        <label for="adm-ms-tid">{t("admin.prov.tenantId")}</label>
        <input id="adm-ms-tid" class="input" class:bad={shown.tenant} dir="ltr" bind:value={p.microsoftApp.tenant} placeholder="contoso.onmicrosoft.com" spellcheck="false" autocomplete="off" />
        {#if shown.tenant}<div class="err">{t(shown.tenant)}</div>{/if}
      </div>
    {/if}
    <div class="field">
      <div class="hint">{t("admin.prov.microsoftHint")}</div>
      <button class="link" onclick={() => api.openUrl(ENTRA_APPS)}><ExternalLink size={12} /> entra.microsoft.com</button>
    </div>
  </div>
</div>

<!-- Other email -->
<div class="card" class:off={!p.imap}>
  <div class="card-head">
    <span class="logo" aria-hidden="true"><Mail size={15} /></span>
    <div class="row-text">
      <div class="row-label">{t("admin.prov.imap")}</div>
      <div class="row-desc">{t("admin.prov.imapDesc")}</div>
    </div>
    <Toggle label={t("admin.prov.imap")} checked={p.imap} onchange={(v) => (p.imap = v)} />
  </div>
  <div class="card-body">
    <div class="row last">
      <div class="row-text">
        <div class="row-label">{t("admin.prov.manual")}</div>
        <div class="row-desc">{t("admin.prov.manualDesc")}</div>
      </div>
      <div class="row-control"><Toggle label={t("admin.prov.manual")} checked={p.manualServers} disabled={!p.imap} onchange={(v) => (p.manualServers = v)} /></div>
    </div>
  </div>
</div>

<div class="group-title">{t("admin.prov.presets")}</div>
<p class="hint presets-desc">{t("admin.prov.presetsDesc")}</p>
<div class="list">
  {#each p.presets as x, i (i)}
    {#if editing === i}
      {@const bad = editProblems}
      <div class="editor">
        <div class="field">
          <label for="adm-p-name">{t("admin.prov.presetName")}</label>
          <input id="adm-p-name" class="input" class:bad={bad.name} dir="auto" bind:value={x.name} placeholder={t("admin.prov.presetNamePlaceholder")} />
          {#if bad.name}<div class="err">{t(bad.name)}</div>{/if}
        </div>
        <div class="field">
          <label for="adm-p-domain">{t("admin.prov.domain")} <span class="opt">{t("admin.prov.optional")}</span></label>
          <input id="adm-p-domain" class="input" class:bad={bad.domain} dir="ltr" bind:value={x.domain} placeholder="example.com" spellcheck="false" autocomplete="off" />
          {#if bad.domain}<div class="err">{t(bad.domain)}</div>{:else}<div class="hint">{t("admin.prov.domainHint")}</div>{/if}
        </div>

        <div class="srv-title">{t("setup.imap")}</div>
        <div class="srv-row">
          <div class="field grow">
            <label for="adm-p-ih">{t("setup.host")}</label>
            <input id="adm-p-ih" class="input" class:bad={bad.imapHost} dir="ltr" bind:value={x.imapHost} placeholder="imap.example.com" spellcheck="false" autocomplete="off" />
          </div>
          <div class="field port">
            <label for="adm-p-ip">{t("setup.port")}</label>
            <input id="adm-p-ip" class="input" class:bad={bad.imapPort} dir="ltr" type="number" min="1" max="65535" bind:value={x.imapPort} />
          </div>
          <div class="field sec">
            <label for="adm-p-is">{t("setup.security")}</label>
            <Select id="adm-p-is" value={x.imapSecurity} options={SECURITY} onchange={(v) => setSecurity(x, "imap", v)} />
          </div>
        </div>
        {#if bad.imapHost || bad.imapPort}<div class="err srv-err">{t((bad.imapHost || bad.imapPort)!)}</div>{/if}

        <div class="srv-title">{t("setup.smtp")}</div>
        <div class="srv-row">
          <div class="field grow">
            <label for="adm-p-sh">{t("setup.host")}</label>
            <input id="adm-p-sh" class="input" class:bad={bad.smtpHost} dir="ltr" bind:value={x.smtpHost} placeholder="smtp.example.com" spellcheck="false" autocomplete="off" />
          </div>
          <div class="field port">
            <label for="adm-p-sp">{t("setup.port")}</label>
            <input id="adm-p-sp" class="input" class:bad={bad.smtpPort} dir="ltr" type="number" min="1" max="65535" bind:value={x.smtpPort} />
          </div>
          <div class="field sec">
            <label for="adm-p-ss">{t("setup.security")}</label>
            <Select id="adm-p-ss" value={x.smtpSecurity} options={SECURITY} onchange={(v) => setSecurity(x, "smtp", v)} />
          </div>
        </div>
        {#if bad.smtpHost || bad.smtpPort}<div class="err srv-err">{t((bad.smtpHost || bad.smtpPort)!)}</div>{/if}

        <div class="field">
          <label for="adm-p-dav">{t("admin.prov.caldav")} <span class="opt">{t("admin.prov.optional")}</span></label>
          <input id="adm-p-dav" class="input" class:bad={bad.caldavUrl} dir="ltr" bind:value={x.caldavUrl} placeholder="https://dav.example.com/" spellcheck="false" autocomplete="off" />
          {#if bad.caldavUrl}<div class="err">{t(bad.caldavUrl)}</div>{/if}
        </div>
        <div class="form-actions">
          <button class="btn danger" onclick={() => removePreset(i)}><Trash2 size={14} /> {t("admin.prov.remove")}</button>
          <span class="grow"></span>
          <button class="btn" onclick={doneEditing}>{t("admin.prov.done")}</button>
        </div>
      </div>
    {:else}
      <button class="item" onclick={() => (editing = i)}>
        <span class="logo" aria-hidden="true"><Server size={15} /></span>
        <span class="item-main">
          <span class="name" dir="auto">{x.name || "—"}</span>
          <span class="sub"><bdi>{x.imapHost}:{x.imapPort}</bdi> · <bdi>{x.smtpHost}:{x.smtpPort}</bdi></span>
        </span>
        {#if shown[`preset${i}`]}<span class="badge bad-badge">!</span>{/if}
        <span class="badge" dir={x.domain ? "ltr" : undefined}>{x.domain ? `@${x.domain.replace(/^@/, "")}` : t("admin.prov.anyDomain")}</span>
        <span class="chev"><ChevronRight size={15} class="rtl-flip" /></span>
      </button>
    {/if}
  {/each}
  {#if p.presets.length === 0}<div class="empty">{t("admin.prov.noPresets")}</div>{/if}
</div>
<button class="btn" onclick={addPreset}><Plus size={15} /> {t("admin.prov.addPreset")}</button>

<div class="actions">
  <span class="grow"></span>
  {#if dirty}<button class="btn" onclick={discard}>{t("admin.discard")}</button>{/if}
  <button class="btn primary" disabled={!dirty || saving} onclick={save}>{t("common.save")}</button>
</div>

<style>
  .last {
    border-bottom: none;
    padding-top: 0;
  }
  .presets-desc {
    margin: -4px 0 8px;
  }
  .chev {
    display: flex;
    color: var(--content-muted);
  }
  .bad-badge {
    background: color-mix(in srgb, var(--indicator-error) 14%, transparent);
    color: var(--indicator-error);
  }
  .srv-err {
    margin: -6px 0 10px;
  }
</style>
