<script lang="ts">
  import { isMobile } from "../../lib/platform";
  import Select from "../Select.svelte";
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { CalendarPlus, Check, Link, MapPin, RefreshCw, Trash2, X } from "@lucide/svelte";
  import { api, errorText } from "../../lib/api";
  import { calApi, type CalAccount, type CalendarPrefs, type Place } from "../../lib/calendar.svelte";
  import type { Account, AppSettings, EngineEvent } from "../../lib/types";
  import { confirmDialog, toast, toastError } from "../../lib/ui.svelte";
  import Toggle from "../Toggle.svelte";

  let { settings, update }: { settings: AppSettings; update: (patch: Partial<AppSettings>) => Promise<void> } = $props();

  const COLORS = ["#d39033", "#ff3b30", "#ff9500", "#ffcc00", "#34c759", "#00c7be", "#007aff", "#5856d6", "#af52de", "#ff2d55", "#a2845e", "#8e8e93"];

  let accounts = $state<CalAccount[]>([]);
  let mailAccounts = $state<Account[]>([]);
  let connecting = $state<string | null>(null);
  /** Calendar account waiting for a Microsoft sign-in in the browser. */
  let signingIn = $state<string | null>(null);
  let other = $state<null | { url: string; username: string; password: string; allowInvalidCerts: boolean }>(null);
  let ics = $state<null | { url: string; name: string; needsPassword: boolean; username: string; password: string; google: boolean }>(null);
  let placeQuery = $state("");
  let places = $state<Place[]>([]);
  let colorFor = $state<string | null>(null);

  const prefs = $derived(settings.calendar);
  const connectable = $derived(mailAccounts.filter((m) => !accounts.some((a) => a.mailAccountId === m.id)));

  async function load() {
    try {
      [accounts, mailAccounts] = await Promise.all([calApi.accounts(), api.accounts()]);
    } catch (e) {
      toastError(e);
    }
  }

  onMount(() => {
    void load();
    let un: (() => void) | undefined;
    let disposed = false;
    listen<EngineEvent>("engine-event", (e) => {
      if ((e.payload as { type: string }).type === "calendarChanged") void load();
    }).then((u) => (disposed ? u() : (un = u)));
    return () => {
      disposed = true;
      un?.();
    };
  });

  const setPrefs = (patch: Partial<CalendarPrefs>) => update({ calendar: { ...prefs, ...patch } } as Partial<AppSettings>);

  async function connectMail(m: Account) {
    connecting = m.id;
    try {
      const a = await calApi.connect({ mailAccountId: m.id, url: null, username: null, password: null, allowInvalidCerts: m.allowInvalidCerts });
      toast(`Connected ${a.calendars.length} calendar${a.calendars.length === 1 ? "" : "s"} for ${m.email}`, { kind: "success" });
      await load();
    } catch (e) {
      toast(`${m.email}: ${errorText(e)}`, { kind: "error", duration: 9000 });
      if (m.auth !== "microsoft") other = { url: "", username: m.username || m.email, password: "", allowInvalidCerts: m.allowInvalidCerts };
      await load();
    } finally {
      connecting = null;
    }
  }

  async function connectOther() {
    if (!other) return;
    if (!/^https?:\/\//i.test(other.url.trim())) return toast("Enter the server address, starting with https://", { kind: "error" });
    connecting = "other";
    try {
      const a = await calApi.connect({ mailAccountId: null, url: other.url.trim(), username: other.username.trim(), password: other.password, allowInvalidCerts: other.allowInvalidCerts });
      toast(`Connected ${a.calendars.length} calendar${a.calendars.length === 1 ? "" : "s"}`, { kind: "success" });
      other = null;
      await load();
    } catch (e) {
      toast(errorText(e), { kind: "error", duration: 9000 });
      await load();
    } finally {
      connecting = null;
    }
  }

  async function subscribe() {
    if (!ics || connecting) return;
    if (!ics.url.trim()) return toast("Paste the calendar's iCal link", { kind: "error" });
    connecting = "ics";
    try {
      const a = await calApi.subscribe({
        url: ics.url.trim(),
        name: ics.name.trim() || null,
        username: ics.needsPassword ? ics.username.trim() || null : null,
        password: ics.needsPassword ? ics.password || null : null,
      });
      toast(`Subscribed to ${a.name}`, { kind: "success" });
      ics = null;
      await load();
    } catch (e) {
      toast(errorText(e), { kind: "error", duration: 9000 });
    } finally {
      connecting = null;
    }
  }

  /** Sign the Microsoft account in again, so Camel Mail may read its calendar. */
  async function signInAgain(a: CalAccount) {
    if (!a.mailAccountId) return;
    signingIn = a.id;
    try {
      await api.reauthorizeAccount(a.mailAccountId);
      toast("Signed in. Your Outlook calendars are syncing.", { kind: "success" });
    } catch (e) {
      toastError(e, "Sign-in didn't work");
    } finally {
      signingIn = null;
      await load();
    }
  }

  /** Where an account lives, without showing secret tokens in iCal links. */
  function where(a: CalAccount): string {
    if (a.kind === "graph") return "Outlook calendars · Microsoft account";
    if (a.kind !== "ics") return a.url;
    try {
      return `iCal subscription · ${new URL(a.url).host}`;
    } catch {
      return "iCal subscription";
    }
  }

  async function disconnect(a: CalAccount) {
    const r = await confirmDialog(
      a.kind === "ics"
        ? {
            title: "Unsubscribe?",
            message: `Remove ${a.name} from Camel Mail? The calendar itself isn't changed; you can subscribe again with its link.`,
            confirmLabel: "Unsubscribe",
            danger: true,
          }
        : {
            title: "Disconnect calendar account?",
            message: `Remove ${a.name} from Camel Mail? Its events are only removed from this computer, not from the server.`,
            confirmLabel: "Disconnect",
            danger: true,
          },
    );
    if (r !== "confirm") return;
    try {
      await calApi.removeAccount(a.id);
      await load();
    } catch (e) {
      toastError(e);
    }
  }

  async function sync(a: CalAccount) {
    try {
      await calApi.sync(a.id);
      await load();
      toast("Calendars synced");
    } catch (e) {
      toastError(e, "Sync failed");
    }
  }

  async function setCal(id: string, enabled: boolean | null, color: string | null) {
    try {
      await calApi.setCalendar(id, enabled, color);
      await load();
    } catch (e) {
      toastError(e);
    }
  }

  let placeTimer: ReturnType<typeof setTimeout> | undefined;
  function searchPlaces() {
    clearTimeout(placeTimer);
    placeTimer = setTimeout(async () => {
      if (placeQuery.trim().length < 2) {
        places = [];
        return;
      }
      try {
        places = await calApi.places(placeQuery.trim());
      } catch (e) {
        toastError(e, "Couldn't look up places");
      }
    }, 300);
  }

  const ago = (ms: number | null) => {
    if (!ms) return "never";
    const m = Math.round((Date.now() - ms) / 60_000);
    return m < 1 ? "just now" : m < 60 ? `${m} min ago` : `${Math.round(m / 60)} h ago`;
  };
</script>

<h1>Calendar</h1>
<p class="intro">Outlook, Hotmail, Live, MSN and Microsoft 365 accounts bring all their calendars by themselves once you sign in with Microsoft. Camel Mail syncs other calendars over CalDAV (iCloud, Fastmail, Nextcloud, Zoho, Yahoo, GMX, mailbox.org and others), using the same app password as your mail. You can also subscribe to any calendar with its iCal link (Google Calendar, holidays, sports, school…); those are read-only.{#if !isMobile} The calendar is next to your mail: use the switch at the top left, or press <kbd>G</kbd> then <kbd>C</kbd>.{/if}</p>

<div class="group-title">Connected calendars</div>
<div class="list">
  {#each accounts as a (a.id)}
    <div class="acct">
      <div class="acct-head">
        <span class="acct-names"><span class="name">{a.name}</span><span class="sub selectable" title={a.kind === "ics" ? "" : a.url}>{where(a)}</span></span>
        <span class="status" class:bad={a.status !== "ok"} title={a.lastError ?? ""}>{a.status === "signin" ? "Needs sign-in" : a.status === "error" ? "Sync problem" : a.lastSync ? `Synced ${ago(a.lastSync)}` : "Syncing…"}</span>
        {#if a.kind === "graph" && a.status === "signin"}
          {#if signingIn === a.id}
            <span class="sub">Waiting for the browser…</span>
            <button class="btn" onclick={() => api.cancelSignIn()}>Cancel</button>
          {:else}
            <button class="btn" disabled={signingIn !== null} onclick={() => signInAgain(a)}>Sign in again</button>
          {/if}
        {/if}
        <button class="icon-btn" title="Sync now" onclick={() => sync(a)}><RefreshCw size={14} /></button>
        <button class="icon-btn" title={a.kind === "ics" ? "Unsubscribe" : "Disconnect"} onclick={() => disconnect(a)}><Trash2 size={14} /></button>
      </div>
      {#if a.lastError}<div class="err selectable">{a.lastError}</div>{/if}
      {#each a.calendars as c (c.id)}
        <div class="cal">
          <button class="swatch" style:background={c.color} title="Colour" onclick={() => (colorFor = colorFor === c.id ? null : c.id)}></button>
          <span class="cname">{c.name}</span>
          {#if c.readOnly}<span class="ro">read-only</span>{/if}
          <Toggle label="Show {c.name}" checked={c.enabled} onchange={(v) => setCal(c.id, v, null)} />
        </div>
        {#if colorFor === c.id}
          <div class="colors">
            {#each COLORS as col (col)}
              <button class="dot" style:background={col} aria-label={col} onclick={() => { colorFor = null; void setCal(c.id, null, col); }}>
                {#if c.color.toLowerCase() === col}<Check size={11} color="#fff" strokeWidth={3} />{/if}
              </button>
            {/each}
          </div>
        {/if}
      {/each}
    </div>
  {:else}
    <div class="empty">No calendars connected yet.</div>
  {/each}
</div>

{#if connectable.length}
  {#each connectable as m (m.id)}
    <div class="connect-row">
      <span class="sub">{m.email}</span>
      {#if m.isGmail}
        <button class="btn" disabled={connecting !== null} onclick={() => (ics = { url: "", name: "", needsPassword: false, username: "", password: "", google: true })}>
          <Link size={14} /> Add with iCal link
        </button>
      {:else}
        <button class="btn" disabled={connecting !== null} onclick={() => connectMail(m)}>
          {#if connecting === m.id}<span class="spinner"></span> Connecting…{:else}<CalendarPlus size={14} /> Connect calendar{/if}
        </button>
      {/if}
    </div>
  {/each}
{/if}

{#if other}
  <div class="card">
    <div class="card-title">Other CalDAV account</div>
    <div class="field"><label for="cd-url">Server address</label><input id="cd-url" class="input" bind:value={other.url} placeholder="https://cloud.example.com/remote.php/dav" spellcheck="false" /></div>
    <div class="field"><label for="cd-user">Username</label><input id="cd-user" class="input" bind:value={other.username} spellcheck="false" /></div>
    <div class="field"><label for="cd-pass">Password or app password</label><input id="cd-pass" class="input" type="password" bind:value={other.password} /></div>
    <label class="check"><input type="checkbox" bind:checked={other.allowInvalidCerts} /> Accept self-signed certificates</label>
    <div class="form-actions">
      <span class="grow"></span>
      <button class="btn" onclick={() => (other = null)}>Cancel</button>
      <button class="btn primary" disabled={connecting !== null} onclick={connectOther}>{connecting === "other" ? "Connecting…" : "Connect"}</button>
    </div>
  </div>
{/if}

{#if ics}
  <div class="card">
    <div class="card-title">Subscribe with an iCal link</div>
    <p class="hint">
      {#if ics.google}
        In Google Calendar on the web, open <b>Settings</b>, pick the calendar under “Settings for my calendars”, then copy <b>Secret address in iCal format</b>.
      {:else}
        Paste a <code>webcal://</code> or <code>.ics</code> link: Google Calendar's “Secret address in iCal format”, a published Outlook calendar, an iCloud public calendar, or a holiday, sports or school calendar.
      {/if}
      Subscribed calendars are read-only; Camel Mail checks them for changes every 30 minutes, or as often as the calendar asks.
    </p>
    <div class="field"><label for="ics-url">iCal link</label><input id="ics-url" class="input" bind:value={ics.url} placeholder="webcal://… or https://…/calendar.ics" spellcheck="false" onkeydown={(e) => e.key === "Enter" && subscribe()} /></div>
    <div class="field"><label for="ics-name">Name (optional)</label><input id="ics-name" class="input" bind:value={ics.name} placeholder="Uses the calendar's own name" /></div>
    <label class="check"><input type="checkbox" bind:checked={ics.needsPassword} /> This link needs a username and password</label>
    {#if ics.needsPassword}
      <div class="field"><label for="ics-user">Username</label><input id="ics-user" class="input" bind:value={ics.username} spellcheck="false" /></div>
      <div class="field"><label for="ics-pass">Password</label><input id="ics-pass" class="input" type="password" bind:value={ics.password} /></div>
    {/if}
    <div class="form-actions">
      <span class="grow"></span>
      <button class="btn" onclick={() => (ics = null)}>Cancel</button>
      <button class="btn primary" disabled={connecting !== null} onclick={subscribe}>{connecting === "ics" ? "Subscribing…" : "Subscribe"}</button>
    </div>
  </div>
{/if}

{#if !other && !ics}
  <div class="adds">
    <button class="btn" onclick={() => (ics = { url: "", name: "", needsPassword: false, username: "", password: "", google: false })}><Link size={14} /> Subscribe with iCal link…</button>
    <button class="btn" onclick={() => (other = { url: "", username: "", password: "", allowInvalidCerts: false })}><CalendarPlus size={14} /> Other CalDAV account…</button>
  </div>
{/if}

<div class="group-title">Preferences</div>
<div class="row">
  <div class="row-label">Week starts on</div>
  <div class="seg" role="radiogroup" aria-label="Week starts on">
    {#each [[6, "Saturday"], [0, "Sunday"], [1, "Monday"]] as [v, l] (v)}
      <button role="radio" aria-checked={prefs.weekStart === v} class:on={prefs.weekStart === v} onclick={() => setPrefs({ weekStart: v as 0 | 1 | 6 })}>{l}</button>
    {/each}
  </div>
</div>
<div class="row">
  <div class="row-label">Show week numbers</div>
  <Toggle label="Show week numbers" checked={prefs.weekNumbers} onchange={(v) => setPrefs({ weekNumbers: v })} />
</div>
<div class="row">
  <div class="row-label">Calendar for new events</div>
  <Select
    width={220}
    label="Default calendar"
    value={prefs.defaultCalendar ?? ""}
    options={[{ value: "", label: "First calendar" }, ...accounts.flatMap((a) => a.calendars.filter((c) => !c.readOnly)).map((c) => ({ value: c.id, label: `${c.name} — ${c.accountName}` }))]}
    onchange={(v) => setPrefs({ defaultCalendar: v || null })}
  />
</div>

<div class="group-title">Weather</div>
<div class="row">
  <div>
    <div class="row-label">Forecast in the calendar</div>
    <div class="row-desc">From Open-Meteo (no account needed). Only the place you pick is sent.</div>
  </div>
  <div class="seg" role="radiogroup" aria-label="Units">
    {#each [["c", "°C"], ["f", "°F"]] as [v, l] (v)}
      <button role="radio" aria-checked={prefs.units === v} class:on={prefs.units === v} onclick={() => setPrefs({ units: v as "c" | "f" })}>{l}</button>
    {/each}
  </div>
</div>
{#if prefs.weather}
  <div class="place">
    <MapPin size={14} /> <span>{prefs.weather.name}{prefs.weather.region ? `, ${prefs.weather.region}` : ""}, {prefs.weather.country}</span>
    <button class="icon-btn" title="Turn off weather" onclick={() => setPrefs({ weather: null })}><X size={14} /></button>
  </div>
{/if}
<div class="field">
  <input class="input" placeholder={prefs.weather ? "Change location…" : "City for the forecast, e.g. Riyadh"} bind:value={placeQuery} oninput={searchPlaces} />
</div>
{#if places.length}
  <div class="list">
    {#each places as p (p.latitude + "," + p.longitude)}
      <button class="placeopt" onclick={() => { void setPrefs({ weather: p }); places = []; placeQuery = ""; }}>
        <MapPin size={13} /> {p.name}<span class="sub">{p.region ? `${p.region}, ` : ""}{p.country}</span>
      </button>
    {/each}
  </div>
{/if}

<style>
  h1 {
    margin: 0 0 18px;
    font-size: 20px;
    line-height: 26px;
    font-weight: 600;
  }
  .intro {
    margin: -6px 0 12px;
    color: var(--content-subtle);
    font-size: 13px;
    line-height: 19px;
  }
  .group-title {
    margin: 24px 0 8px;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--content-muted);
  }
  .list {
    border: 0.5px solid var(--separator);
    border-radius: var(--radius-xl);
    background: var(--surface-elevated);
    overflow: hidden;
    margin-bottom: 10px;
  }
  .list > :global(* + *) {
    border-top: 0.5px solid var(--separator-subtle);
  }
  .acct {
    padding: 10px 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .acct-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .acct-names {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .name {
    font-weight: 600;
  }
  .sub {
    font-size: 12px;
    color: var(--content-subtle);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .status {
    font-size: 11.5px;
    color: var(--indicator-success);
    white-space: nowrap;
  }
  .status.bad {
    color: var(--indicator-error);
  }
  .err {
    font-size: 12px;
    color: var(--indicator-error);
  }
  .cal {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 3px 0 3px 4px;
  }
  .swatch {
    width: 14px;
    height: 14px;
    border-radius: 4px;
    border: none;
    padding: 0;
  }
  .cname {
    flex: 1;
    font-size: 13px;
  }
  .ro {
    font-size: 11px;
    color: var(--content-muted);
  }
  .colors {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 2px 0 6px 28px;
  }
  .dot {
    width: 20px;
    height: 20px;
    border-radius: 50%;
    border: none;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .empty {
    padding: 18px;
    text-align: center;
    color: var(--content-subtle);
  }
  .connect-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 6px 2px;
  }
  .adds {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 6px;
  }
  .hint {
    margin: -4px 0 0;
    font-size: 12.5px;
    line-height: 18px;
    color: var(--content-subtle);
  }
  .hint code {
    font-size: 11.5px;
  }
  .intro :global(kbd) {
    font-family: inherit;
    font-size: 11px;
    padding: 0 4px;
    border-radius: 4px;
    border: 0.5px solid var(--border-tertiary);
  }
  .card {
    border: 0.5px solid var(--separator);
    border-radius: var(--radius-xl);
    background: var(--surface-elevated);
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin-top: 8px;
  }
  .card-title {
    font-weight: 600;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .field label {
    font-size: 12px;
    font-weight: 500;
    color: var(--content-subtle);
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 12.5px;
    color: var(--content-subtle);
  }
  .form-actions {
    display: flex;
    gap: 8px;
  }
  .grow {
    flex: 1;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 24px;
    padding: 12px 0;
    border-bottom: 0.5px solid var(--separator-subtle);
  }
  .row-label {
    font-size: 13px;
    font-weight: 500;
  }
  .row-desc {
    margin-top: 2px;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .seg {
    display: inline-flex;
    padding: 2px;
    border-radius: var(--radius-lg);
    background: var(--surface-hover);
  }
  .seg button {
    border: none;
    background: none;
    padding: 4px 12px;
    border-radius: 6px;
    font-size: 12px;
    font-weight: 500;
    color: var(--content-subtle);
  }
  .seg button.on {
    background: var(--surface-overlay);
    color: var(--content);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.12);
  }
  .place {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 0 6px;
    font-size: 13px;
  }
  .place span {
    flex: 1;
  }
  .placeopt {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 8px;
    border: none;
    background: none;
    padding: 8px 12px;
    text-align: left;
    font-size: 13px;
    color: var(--content);
  }
  .placeopt:hover {
    background: var(--surface-hover);
  }
  .placeopt .sub {
    margin-left: auto;
  }
</style>
