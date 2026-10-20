<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import {
    Users, SlidersHorizontal, PenLine, FileText, Clock, Info, Plus, Trash2, ChevronRight,
    Bold, Italic, Underline, Link as LinkIcon, Code, Check, X, RotateCcw, Sparkles, Plug, Eraser, CalendarDays, Keyboard,
  } from "@lucide/svelte";
  import { api } from "../../lib/api";
  import { directEditor } from "../../lib/bidi";
  import { applyTheme, confirmDialog, toast, toastError } from "../../lib/ui.svelte";
  import { BUCKET_NAMES, type Account, type AppSettings, type CleanKind, type SenderRule, type Signature, type Template } from "../../lib/types";
  import AccountSetup from "../AccountSetup.svelte";
  import AiSettings from "./AiSettings.svelte";
  import CalendarSettings from "./CalendarSettings.svelte";
  import ConnectionsSettings from "./ConnectionsSettings.svelte";
  import ShortcutsSettings from "./ShortcutsSettings.svelte";
  import ConfirmDialog from "../ConfirmDialog.svelte";
  import Toasts from "../Toasts.svelte";
  import Toggle from "../Toggle.svelte";

  let { initialSection }: { initialSection?: string } = $props();

  const SECTIONS = [
    { id: "accounts", label: "Accounts", icon: Users },
    { id: "general", label: "General", icon: SlidersHorizontal },
    { id: "autoclean", label: "Auto-clean", icon: Eraser },
    { id: "calendar", label: "Calendar", icon: CalendarDays },
    { id: "ai", label: "AI Assistant", icon: Sparkles },
    { id: "connections", label: "Connections", icon: Plug },
    { id: "signatures", label: "Signatures", icon: PenLine },
    { id: "templates", label: "Templates", icon: FileText },
    { id: "snooze", label: "Snooze", icon: Clock },
    { id: "shortcuts", label: "Shortcuts", icon: Keyboard },
    { id: "about", label: "About", icon: Info },
  ] as const;
  type SectionId = (typeof SECTIONS)[number]["id"];
  const isSection = (s: unknown): s is SectionId => SECTIONS.some((x) => x.id === s);

  let section = $state<SectionId>(isSection((() => initialSection)()) ? ((() => initialSection)() as SectionId) : "accounts");
  let settings = $state<AppSettings | null>(null);
  let accounts = $state<Account[]>([]);
  let info = $state<{ version: string; dataDir: string } | null>(null);
  let contentEl: HTMLElement | undefined = $state();

  onMount(() => {
    const un: (() => void)[] = [];
    let disposed = false;
    // Closing before a listener is in place must still remove it.
    const keep = (u: () => void) => (disposed ? u() : un.push(u));
    listen<string>("settings-section", (e) => {
      if (isSection(e.payload)) section = e.payload;
    }).then(keep);
    // Changes made elsewhere (e.g. the layout toggle in the main window); our own saves answer with the newest state.
    listen<AppSettings>("settings-changed", (e) => {
      if (saving === 0) settings = e.payload;
    }).then(keep);
    api.getSettings().then((s) => {
      settings = s;
      applyTheme(s.theme);
    }).catch((e) => toastError(e));
    api.appInfo().then((i) => (info = i)).catch(() => {});
    loadAccounts();
    loadSignatures();
    loadTemplates();
    loadRules();
    return () => {
      disposed = true;
      un.forEach((u) => u());
    };
  });

  $effect(() => {
    section;
    contentEl?.scrollTo({ top: 0 });
  });

  /** A text field (not a checkbox or button) has the keyboard. */
  function typingIn(t: EventTarget | null): t is HTMLElement {
    if (!(t instanceof HTMLElement)) return false;
    if (t.isContentEditable || t instanceof HTMLTextAreaElement) return true;
    return t instanceof HTMLInputElement && !["checkbox", "radio", "button", "submit", "reset", "range", "color", "file"].includes(t.type);
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key !== "Escape" || e.defaultPrevented) return;
    // Escape leaves the field (keeping the edit) rather than closing the window over it.
    if (typingIn(e.target)) e.target.blur();
    else getCurrentWindow().close();
  }

  let saveSeq = 0;
  /** Saves in flight; their replies (not the broadcast) settle the state. */
  let saving = 0;
  async function update(patch: Partial<AppSettings>) {
    if (!settings) return;
    settings = { ...settings, ...patch };
    const seq = ++saveSeq;
    saving++;
    try {
      const saved = await api.setSettings($state.snapshot(settings) as AppSettings);
      if (patch.theme) applyTheme(patch.theme);
      // A slower reply must not undo a newer change.
      if (seq !== saveSeq) return;
      settings = saved;
      if (section === "autoclean") void refreshPreview();
    } catch (e) {
      toastError(e);
    } finally {
      saving--;
    }
  }

  // ------------------------------------------------------------------ accounts
  const COLORS = ["#ff7c19", "#5b4ff0", "#3b81f6", "#10b981", "#f97316", "#ef4444", "#eab308", "#06b6d4", "#ec4899"];
  let showAdd = $state(false);
  let openId = $state<string | null>(null);
  let reauthId = $state<string | null>(null);
  let nameDraft = $state<Record<string, string>>({});
  let bccDraft = $state<Record<string, string>>({});

  async function loadAccounts() {
    try {
      accounts = await api.accounts();
    } catch (e) {
      toastError(e);
    }
  }

  function toggleAccount(a: Account) {
    if (openId === a.id) {
      openId = null;
      return;
    }
    openId = a.id;
    reauthId = null;
    nameDraft[a.id] = a.displayName;
    bccDraft[a.id] = a.autoBcc.join(", ");
  }

  async function savePrefs(id: string, prefs: Parameters<typeof api.updateAccountPrefs>[1]) {
    try {
      await api.updateAccountPrefs(id, prefs);
      await loadAccounts();
    } catch (e) {
      toastError(e);
    }
  }

  function saveName(a: Account) {
    const v = (nameDraft[a.id] ?? "").trim();
    if (!v) {
      nameDraft[a.id] = a.displayName;
      return;
    }
    if (v !== a.displayName) savePrefs(a.id, { displayName: v });
  }

  function saveBcc(a: Account) {
    const list = (bccDraft[a.id] ?? "").split(/[,;\s]+/).map((s) => s.trim()).filter(Boolean);
    if (list.join(",") !== a.autoBcc.join(",")) savePrefs(a.id, { autoBcc: list });
  }

  async function removeAccount(a: Account) {
    const r = await confirmDialog({
      title: "Remove account?",
      message: `Remove ${a.email} from Remail? Its downloaded mail will be deleted from this computer. Nothing is deleted on the server.`,
      confirmLabel: "Remove",
      danger: true,
    });
    if (r !== "confirm") return;
    try {
      await api.removeAccount(a.id);
      toast("Account removed");
      openId = null;
      await loadAccounts();
    } catch (e) {
      toastError(e);
    }
  }

  const statusLabel = (s: Account["status"]) => (s === "auth_error" ? "Sign-in needed" : s === "offline" ? "Offline" : "Active");

  // ------------------------------------------------------------ rich editor
  let htmlMode = $state(false);
  let editHtml = $state("");
  let linkOpen = $state(false);
  let linkUrl = $state("");
  let savedRange: Range | null = null;

  function richbox(node: HTMLElement, html: string) {
    node.innerHTML = html;
  }

  function exec(cmd: string, value?: string) {
    document.execCommand(cmd, false, value);
  }

  function openLink() {
    const sel = window.getSelection();
    savedRange = sel && sel.rangeCount ? sel.getRangeAt(0).cloneRange() : null;
    linkUrl = "https://";
    linkOpen = true;
  }

  function applyLink(box: HTMLElement | null) {
    const url = linkUrl.trim();
    linkOpen = false;
    if (!url || !box) return;
    box.focus();
    const sel = window.getSelection();
    if (savedRange && sel) {
      sel.removeAllRanges();
      sel.addRange(savedRange);
    }
    if (sel && sel.isCollapsed) exec("insertHTML", `<a href="${url.replace(/"/g, "&quot;")}">${url.replace(/</g, "&lt;")}</a>`);
    else exec("createLink", url);
    editHtml = box.innerHTML;
  }

  let boxEl: HTMLElement | null = $state(null);

  // -------------------------------------------------------------- signatures
  let signatures = $state<Signature[]>([]);
  let sigEdit = $state<Signature | null>(null);

  async function loadSignatures() {
    try {
      signatures = await api.signatures();
    } catch (e) {
      toastError(e);
    }
  }
  function editSig(s: Signature | null) {
    sigEdit = s ? { ...s } : { id: "", accountId: null, name: "", html: "", isDefault: signatures.length === 0 };
    editHtml = sigEdit.html;
    htmlMode = false;
    linkOpen = false;
  }
  async function saveSig() {
    if (!sigEdit) return;
    if (!sigEdit.name.trim()) return toast("Give the signature a name", { kind: "error" });
    try {
      await api.saveSignature({ ...sigEdit, name: sigEdit.name.trim(), html: editHtml });
      sigEdit = null;
      await loadSignatures();
      toast("Signature saved");
    } catch (e) {
      toastError(e);
    }
  }
  async function deleteSig() {
    if (!sigEdit || !sigEdit.id) return (sigEdit = null);
    const r = await confirmDialog({ title: "Delete signature?", message: `Delete "${sigEdit.name}"? This can't be undone.`, confirmLabel: "Delete", danger: true });
    if (r !== "confirm") return;
    try {
      await api.deleteSignature(sigEdit.id);
      sigEdit = null;
      await loadSignatures();
    } catch (e) {
      toastError(e);
    }
  }
  const scopeLabel = (id: string | null) => (id ? (accounts.find((a) => a.id === id)?.email ?? "Unknown account") : "All accounts");

  // --------------------------------------------------------------- templates
  let templates = $state<Template[]>([]);
  let tplEdit = $state<Template | null>(null);

  async function loadTemplates() {
    try {
      templates = await api.templates();
    } catch (e) {
      toastError(e);
    }
  }
  function editTpl(t: Template | null) {
    tplEdit = t ? { ...t } : { id: "", name: "", subject: "", html: "", updatedAt: 0 };
    editHtml = tplEdit.html;
    htmlMode = false;
    linkOpen = false;
  }
  async function saveTpl() {
    if (!tplEdit) return;
    if (!tplEdit.name.trim()) return toast("Give the template a name", { kind: "error" });
    try {
      await api.saveTemplate({ ...tplEdit, name: tplEdit.name.trim(), html: editHtml });
      tplEdit = null;
      await loadTemplates();
      toast("Template saved");
    } catch (e) {
      toastError(e);
    }
  }
  async function deleteTpl() {
    if (!tplEdit || !tplEdit.id) return (tplEdit = null);
    const r = await confirmDialog({ title: "Delete template?", message: `Delete "${tplEdit.name}"? This can't be undone.`, confirmLabel: "Delete", danger: true });
    if (r !== "confirm") return;
    try {
      await api.deleteTemplate(tplEdit.id);
      tplEdit = null;
      await loadTemplates();
    } catch (e) {
      toastError(e);
    }
  }

  // -------------------------------------------------------------- auto-clean
  const CLEAN_KINDS: { kind: CleanKind; label: string; desc: string }[] = [
    { kind: "promotions", label: "Promotions", desc: "Sales, newsletters and marketing" },
    { kind: "updates", label: "Updates", desc: "Receipts, notifications, statements and alerts" },
    { kind: "social", label: "Social", desc: "Social networks and community apps" },
    { kind: "forums", label: "Forums", desc: "Mailing lists and group discussions" },
    { kind: "otp", label: "One-time codes", desc: "Verification, sign-in and OTP codes" },
  ];
  const CLEAN_DELAYS: [number, string][] = [
    [-1, "Keep"],
    [0, "Right after I read it"],
    [60, "1 hour after reading"],
    [1440, "1 day after reading"],
    [4320, "3 days after reading"],
    [10080, "1 week after reading"],
    [43200, "30 days after reading"],
  ];
  let cleanPreview = $state<number | null>(null);

  async function refreshPreview() {
    try {
      cleanPreview = await api.autocleanPreview();
    } catch {
      cleanPreview = null;
    }
  }

  $effect(() => {
    if (section === "autoclean") void refreshPreview();
  });

  const delayOf = (kind: CleanKind) => settings?.autoClean.rules.find((r) => r.kind === kind)?.afterMinutes ?? -1;

  function setDelay(kind: CleanKind, minutes: number) {
    if (!settings) return;
    const rules = settings.autoClean.rules.filter((r) => r.kind !== kind);
    if (minutes >= 0) {
      const old = settings.autoClean.rules.find((r) => r.kind === kind);
      rules.push({ kind, afterMinutes: minutes, since: old?.since ?? 0 });
    }
    void update({ autoClean: { ...settings.autoClean, rules } });
  }

  // ------------------------------------------------------------ smart inbox
  let rules = $state<SenderRule[]>([]);

  async function loadRules() {
    try {
      rules = await api.senderRules();
    } catch {
      /* optional */
    }
  }

  async function removeRule(r: SenderRule) {
    try {
      await api.setSenderCategory(r.email, null);
      await loadRules();
    } catch (e) {
      toastError(e);
    }
  }

  // ------------------------------------------------------------------ snooze
  const SNOOZE_TIMES = [
    { key: "snoozeAfternoon", label: "Afternoon", desc: "Offered until this time of day." },
    { key: "snoozeEvening", label: "Evening", desc: "Offered after the afternoon time." },
    { key: "snoozeMorning", label: "Morning", desc: "Replaces Evening once that time has passed — wakes tomorrow morning." },
    { key: "snoozeTomorrow", label: "Tomorrow", desc: "At this time tomorrow." },
    { key: "snoozeDayAfter", label: "Day after", desc: "At this time the day after tomorrow." },
    { key: "snoozeSaturday", label: "Saturday", desc: "At this time next Saturday." },
    { key: "snoozeMonday", label: "Monday", desc: "At this time next Monday." },
  ] as const;

  function resetSnooze() {
    update({
      snoozeLaterMinutes: 120,
      snoozeAfternoon: "15:00",
      snoozeEvening: "18:00",
      snoozeMorning: "08:00",
      snoozeTomorrow: "10:00",
      snoozeDayAfter: "11:00",
      snoozeSaturday: "10:00",
      snoozeMonday: "10:00",
    });
  }
</script>

<svelte:window {onkeydown} />

{#snippet toolbar()}
  <div class="toolbar">
    <button type="button" class="icon-btn" title="Bold" aria-label="Bold" onmousedown={(e) => e.preventDefault()} onclick={() => { exec("bold"); editHtml = boxEl?.innerHTML ?? editHtml; }} disabled={htmlMode}><Bold size={15} /></button>
    <button type="button" class="icon-btn" title="Italic" aria-label="Italic" onmousedown={(e) => e.preventDefault()} onclick={() => { exec("italic"); editHtml = boxEl?.innerHTML ?? editHtml; }} disabled={htmlMode}><Italic size={15} /></button>
    <button type="button" class="icon-btn" title="Underline" aria-label="Underline" onmousedown={(e) => e.preventDefault()} onclick={() => { exec("underline"); editHtml = boxEl?.innerHTML ?? editHtml; }} disabled={htmlMode}><Underline size={15} /></button>
    <button type="button" class="icon-btn" class:active={linkOpen} title="Link" aria-label="Link" onmousedown={(e) => e.preventDefault()} onclick={openLink} disabled={htmlMode}><LinkIcon size={15} /></button>
    <span class="grow"></span>
    <button type="button" class="icon-btn" class:active={htmlMode} title="Edit HTML" aria-label="Edit HTML" onclick={() => (htmlMode = !htmlMode)}><Code size={15} /></button>
  </div>
  {#if linkOpen && !htmlMode}
    <form class="link-row" onsubmit={(e) => { e.preventDefault(); applyLink(boxEl); }}>
      <input class="input" aria-label="Link URL" bind:value={linkUrl} placeholder="https://" />
      <button type="submit" class="btn primary">Apply</button>
      <button type="button" class="icon-btn" aria-label="Cancel link" onclick={() => (linkOpen = false)}><X size={15} /></button>
    </form>
  {/if}
{/snippet}

{#snippet editor()}
  <div class="editor">
    {@render toolbar()}
    {#key htmlMode}
      {#if htmlMode}
        <textarea class="input html-area selectable" aria-label="HTML source" bind:value={editHtml} spellcheck="false"></textarea>
      {:else}
        <div
          class="sig-editor selectable"
          contenteditable="true"
          role="textbox"
          tabindex="0"
          aria-multiline="true"
          aria-label="Content"
          bind:this={boxEl}
          use:richbox={editHtml}
          oninput={(e) => {
            if (!(e as unknown as InputEvent).isComposing) directEditor(e.currentTarget);
            editHtml = e.currentTarget.innerHTML;
          }}
          oncompositionend={(e) => {
            directEditor(e.currentTarget);
            editHtml = e.currentTarget.innerHTML;
          }}
        ></div>
      {/if}
    {/key}
  </div>
{/snippet}

{#snippet row(label: string, desc: string | null, control: import("svelte").Snippet)}
  <div class="row">
    <div class="row-text">
      <div class="row-label">{label}</div>
      {#if desc}<div class="row-desc">{desc}</div>{/if}
    </div>
    <div class="row-control">{@render control()}</div>
  </div>
{/snippet}

<div class="win">
  <nav class="rail" aria-label="Settings sections">
    <div class="rail-title">Settings</div>
    {#each SECTIONS as s}
      <button class="rail-item" class:active={section === s.id} aria-current={section === s.id ? "page" : undefined} onclick={() => (section = s.id)}>
        <s.icon size={16} />
        {s.label}
      </button>
    {/each}
  </nav>

  <main class="content" bind:this={contentEl}>
    <div class="inner">
      {#if section === "accounts"}
        <h1>Accounts</h1>
        <div class="list">
          {#each accounts as a (a.id)}
            <div class="acct" class:open={openId === a.id}>
              <button class="acct-head" aria-expanded={openId === a.id} onclick={() => toggleAccount(a)}>
                <span class="dot" style:background={a.color}></span>
                <span class="acct-names">
                  <span class="acct-name">{a.displayName}</span>
                  <span class="acct-email">{a.email}</span>
                </span>
                {#if a.status !== "active"}
                  <span class="badge" title={a.lastError ?? statusLabel(a.status)}>{statusLabel(a.status)}</span>
                {/if}
                <span class="chev" class:open={openId === a.id}><ChevronRight size={15} /></span>
              </button>
              {#if openId === a.id}
                <div class="acct-body">
                  {@render row("Display name", null, nameCtl)}
                  {#snippet nameCtl()}
                    <input class="input narrow" aria-label="Display name" bind:value={nameDraft[a.id]} onblur={() => saveName(a)} onkeydown={(e) => e.key === "Enter" && e.currentTarget.blur()} />
                  {/snippet}
                  {@render row("Account color", null, colorCtl)}
                  {#snippet colorCtl()}
                    <div class="swatches" role="radiogroup" aria-label="Account color">
                      {#each COLORS as c}
                        <button class="swatch" role="radio" aria-checked={a.color.toLowerCase() === c} aria-label={c} style:background={c} onclick={() => savePrefs(a.id, { color: c })}>
                          {#if a.color.toLowerCase() === c}<Check size={12} color="#fff" strokeWidth={3} />{/if}
                        </button>
                      {/each}
                    </div>
                  {/snippet}
                  {@render row("Show in Unified Inbox", null, unifiedCtl)}
                  {#snippet unifiedCtl()}
                    <Toggle label="Show in Unified Inbox" checked={!a.hiddenInUnified} onchange={(v) => savePrefs(a.id, { hiddenInUnified: !v })} />
                  {/snippet}
                  {@render row("Auto-BCC", "Always BCC these addresses, comma separated.", bccCtl)}
                  {#snippet bccCtl()}
                    <input class="input narrow" aria-label="Auto-BCC" placeholder="name@example.com" bind:value={bccDraft[a.id]} onblur={() => saveBcc(a)} onkeydown={(e) => e.key === "Enter" && e.currentTarget.blur()} />
                  {/snippet}
                  <div class="server-line selectable">
                    IMAP {a.imap.host}:{a.imap.port} · SMTP {a.smtp.host}:{a.smtp.port}
                  </div>
                  {#if a.lastError}<div class="err-line selectable">{a.lastError}</div>{/if}
                  {#if reauthId === a.id}
                    <div class="inline-setup">
                      <AccountSetup mode="edit" account={a} compact ondone={() => { reauthId = null; loadAccounts(); }} oncancel={() => (reauthId = null)} />
                    </div>
                  {:else}
                    <div class="acct-actions">
                      <button class="btn" onclick={() => (reauthId = a.id)}>{a.auth === "microsoft" ? "Sign in again" : "Re-authenticate"}</button>
                      <button class="btn danger" onclick={() => removeAccount(a)}>Remove account</button>
                    </div>
                  {/if}
                </div>
              {/if}
            </div>
          {/each}
          {#if accounts.length === 0}<div class="empty">No accounts yet.</div>{/if}
        </div>

        {#if showAdd}
          <div class="card">
            <div class="card-title">Add account</div>
            <AccountSetup mode="add" compact ondone={() => { showAdd = false; loadAccounts(); }} oncancel={() => (showAdd = false)} />
          </div>
        {:else}
          <button class="btn add-btn" onclick={() => (showAdd = true)}><Plus size={15} /> Add account</button>
        {/if}

        {#if settings}
          <div class="group-title">Microsoft sign-in</div>
          {@render row("App (client) ID", "Outlook.com, Hotmail and Microsoft 365 accounts sign in through a Microsoft Entra app registration. Leave empty to use the one Remail was built with.", msCtl)}
          {#snippet msCtl()}
            <input
              class="input narrow"
              aria-label="Microsoft app (client) ID"
              placeholder="00000000-0000-0000-0000-000000000000"
              spellcheck="false"
              value={settings!.microsoftClientId}
              onchange={(e) => update({ microsoftClientId: e.currentTarget.value.trim() })}
            />
          {/snippet}
        {/if}

      {:else if section === "general" && settings}
        <h1>General</h1>
        <div class="group-title">Appearance</div>
        {@render row("Theme", null, themeCtl)}
        {#snippet themeCtl()}
          <div class="seg" role="radiogroup" aria-label="Theme">
            {#each [["system", "System"], ["light", "Light"], ["dark", "Dark"]] as [v, l]}
              <button role="radio" aria-checked={settings!.theme === v} class:on={settings!.theme === v} onclick={() => update({ theme: v as AppSettings["theme"] })}>{l}</button>
            {/each}
          </div>
        {/snippet}

        {@render row("Layout", "Split view shows the open email beside the list.", layoutCtl)}
        {#snippet layoutCtl()}
          <div class="seg" role="radiogroup" aria-label="Layout">
            {#each [["split", "Split view"], ["list", "Full-width list"]] as [v, l]}
              <button role="radio" aria-checked={settings!.layout === v} class:on={settings!.layout === v} onclick={() => update({ layout: v as AppSettings["layout"] })}>{l}</button>
            {/each}
          </div>
        {/snippet}

        <div class="group-title">Smart Inbox</div>
        {@render row("Smart Inbox", "Split the Inbox into Important and Other (updates, promotions, social, forums).", smartCtl)}
        {#snippet smartCtl()}<Toggle label="Smart Inbox" checked={settings!.smartInbox} onchange={(v) => update({ smartInbox: v })} />{/snippet}
        {#if settings.smartInbox}
          {@render row("Notify only for Important email", null, impCtl)}
          {#snippet impCtl()}<Toggle label="Notify only for Important email" checked={settings!.notifyImportantOnly} onchange={(v) => update({ notifyImportantOnly: v })} />{/snippet}
          {#if rules.length > 0}
            <div class="rules">
              <div class="row-desc">Senders you've sorted yourself:</div>
              {#each rules as r (r.email)}
                <div class="rule">
                  <span class="rule-email selectable">{r.email}</span>
                  <span class="badge">{BUCKET_NAMES[r.category] ?? r.category}</span>
                  <button class="icon-btn" title="Forget" onclick={() => removeRule(r)}><X size={13} /></button>
                </div>
              {/each}
            </div>
          {/if}
        {/if}

        <div class="group-title">Sending</div>
        {@render row("Undo send delay", "How long you can take back a sent email.", undoCtl)}
        {#snippet undoCtl()}
          <select class="input sel" aria-label="Undo send delay" value={settings!.undoSendSeconds} onchange={(e) => update({ undoSendSeconds: Number(e.currentTarget.value) })}>
            <option value={0}>Off</option><option value={5}>5 seconds</option><option value={10}>10 seconds</option><option value={30}>30 seconds</option>
          </select>
        {/snippet}
        {@render row("Confirm before sending", null, confirmCtl)}
        {#snippet confirmCtl()}<Toggle label="Confirm before sending" checked={settings!.confirmBeforeSend} onchange={(v) => update({ confirmBeforeSend: v })} />{/snippet}
        {@render row("Default reply", null, replyCtl)}
        {#snippet replyCtl()}
          <div class="seg" role="radiogroup" aria-label="Default reply">
            {#each [["reply", "Reply"], ["replyAll", "Reply all"]] as [v, l]}
              <button role="radio" aria-checked={settings!.defaultReply === v} class:on={settings!.defaultReply === v} onclick={() => update({ defaultReply: v as AppSettings["defaultReply"] })}>{l}</button>
            {/each}
          </div>
        {/snippet}

        <div class="group-title">Reading</div>
        {@render row("After archiving or deleting", "What to show next when the open email leaves the list.", afterCtl)}
        {#snippet afterCtl()}
          <select class="input sel" aria-label="After archiving or deleting" value={settings!.afterRemove} onchange={(e) => update({ afterRemove: e.currentTarget.value as AppSettings["afterRemove"] })}>
            <option value="nextUnread">Next unread email</option>
            <option value="next">Next email</option>
            <option value="previous">Previous email</option>
            <option value="list">Back to the list</option>
          </select>
        {/snippet}
        {@render row("Mark as read when opened", null, readCtl)}
        {#snippet readCtl()}<Toggle label="Mark as read when opened" checked={settings!.markReadOnOpen} onchange={(v) => update({ markReadOnOpen: v })} />{/snippet}
        {@render row("Load remote images", "Remote images can reveal when you open an email. When off, Remail asks per message.", imgCtl)}
        {#snippet imgCtl()}<Toggle label="Load remote images" checked={settings!.loadRemoteImages} onchange={(v) => update({ loadRemoteImages: v })} />{/snippet}

        <div class="group-title">Notifications</div>
        {@render row("Notifications", "Show a desktop notification for new mail", notifCtl)}
        {#snippet notifCtl()}<Toggle label="Notifications" checked={settings!.notifications} onchange={(v) => update({ notifications: v })} />{/snippet}
        {@render row("Notify when a snoozed email returns", null, wakeCtl)}
        {#snippet wakeCtl()}<Toggle label="Notify when a snoozed email returns" checked={settings!.notifyOnSnoozeWake} onchange={(v) => update({ notifyOnSnoozeWake: v })} />{/snippet}

        <div class="group-title">System</div>
        {@render row("Keep running in the tray when the window is closed", "Needed for Send Later and snooze reminders while the window is closed.", trayCtl)}
        {#snippet trayCtl()}<Toggle label="Keep running in the tray" checked={settings!.closeToTray} onchange={(v) => update({ closeToTray: v })} />{/snippet}
        {@render row("Sync interval", "How often Remail checks for new mail.", syncCtl)}
        {#snippet syncCtl()}
          <select class="input sel" aria-label="Sync interval" value={settings!.syncIntervalSeconds} onchange={(e) => update({ syncIntervalSeconds: Number(e.currentTarget.value) })}>
            <option value={15}>15 seconds</option><option value={30}>30 seconds</option><option value={60}>1 minute</option><option value={300}>5 minutes</option>
          </select>
        {/snippet}

      {:else if section === "autoclean" && settings}
        <h1>Auto-clean</h1>
        <p class="intro">Remove read mail you don't need to keep, so your Inbox stays clear. Choose which kinds of email and how long after you've read them.</p>
        {@render row("Auto-clean read email", null, cleanCtl)}
        {#snippet cleanCtl()}<Toggle label="Auto-clean read email" checked={settings!.autoClean.enabled} onchange={(v) => update({ autoClean: { ...settings!.autoClean, enabled: v } })} />{/snippet}

        <div class="group-title">Remove</div>
        {#each CLEAN_KINDS as k (k.kind)}
          {@render row(k.label, k.desc, delayCtl)}
          {#snippet delayCtl()}
            <select class="input sel" aria-label={k.label} disabled={!settings!.autoClean.enabled} value={delayOf(k.kind)} onchange={(e) => setDelay(k.kind, Number(e.currentTarget.value))}>
              {#each CLEAN_DELAYS as [m, l] (m)}<option value={m}>{l}</option>{/each}
            </select>
          {/snippet}
        {/each}

        {@render row("What to do with them", null, actionCtl)}
        {#snippet actionCtl()}
          <div class="seg" role="radiogroup" aria-label="Auto-clean action">
            {#each [["trash", "Move to Trash"], ["archive", "Archive"]] as [v, l]}
              <button role="radio" aria-checked={settings!.autoClean.action === v} class:on={settings!.autoClean.action === v} onclick={() => update({ autoClean: { ...settings!.autoClean, action: v as "trash" | "archive" } })}>{l}</button>
            {/each}
          </div>
        {/snippet}

        <p class="note">
          Never removed: unread email, starred email, and conversations you've written in. "Right after I read it" waits until you move
          on to another email. Only mail you read after turning a rule on is affected, and the notice that appears has an Undo button.
          {#if settings.autoClean.enabled && cleanPreview !== null}<br /><b>{cleanPreview === 0 ? "Nothing to clean right now." : `${cleanPreview} conversation${cleanPreview === 1 ? "" : "s"} will be cleaned shortly.`}</b>{/if}
        </p>

      {:else if section === "calendar" && settings}
        <CalendarSettings {settings} {update} />

      {:else if section === "ai"}
        <AiSettings />

      {:else if section === "connections"}
        <ConnectionsSettings />

      {:else if section === "signatures"}
        <h1>Signatures</h1>
        {#if sigEdit}
          <div class="card">
            <div class="field"><label for="sg-name">Name</label><input id="sg-name" class="input" dir="auto" bind:value={sigEdit.name} placeholder="Work" /></div>
            <div class="field">
              <label for="sg-acct">Account</label>
              <select id="sg-acct" class="input" value={sigEdit.accountId ?? ""} onchange={(e) => (sigEdit!.accountId = e.currentTarget.value || null)}>
                <option value="">All accounts</option>
                {#each accounts as a}<option value={a.id}>{a.email}</option>{/each}
              </select>
            </div>
            <div class="field"><span class="lbl">Signature</span>{@render editor()}</div>
            <label class="check"><input type="checkbox" bind:checked={sigEdit.isDefault} /> Use as default</label>
            <div class="form-actions">
              <button class="btn danger" onclick={deleteSig}><Trash2 size={14} /> {sigEdit.id ? "Delete" : "Discard"}</button>
              <span class="grow"></span>
              <button class="btn" onclick={() => (sigEdit = null)}>Cancel</button>
              <button class="btn primary" onclick={saveSig}>Save</button>
            </div>
          </div>
        {:else}
          <div class="list">
            {#each signatures as s (s.id)}
              <button class="item" onclick={() => editSig(s)}>
                <span class="item-main"><span class="acct-name" dir="auto">{s.name}</span><span class="acct-email">{scopeLabel(s.accountId)}</span></span>
                {#if s.isDefault}<span class="badge accent">Default</span>{/if}
                <span class="chev"><ChevronRight size={15} /></span>
              </button>
            {/each}
            {#if signatures.length === 0}<div class="empty">No signatures yet.</div>{/if}
          </div>
          <button class="btn add-btn" onclick={() => editSig(null)}><Plus size={15} /> New signature</button>
        {/if}

      {:else if section === "templates"}
        <h1>Templates</h1>
        {#if tplEdit}
          <div class="card">
            <div class="field"><label for="tp-name">Name</label><input id="tp-name" class="input" dir="auto" bind:value={tplEdit.name} placeholder="Meeting follow-up" /></div>
            <div class="field"><label for="tp-subj">Subject (optional)</label><input id="tp-subj" class="input" dir="auto" bind:value={tplEdit.subject} /></div>
            <div class="field"><span class="lbl">Message</span>{@render editor()}</div>
            <div class="form-actions">
              <button class="btn danger" onclick={deleteTpl}><Trash2 size={14} /> {tplEdit.id ? "Delete" : "Discard"}</button>
              <span class="grow"></span>
              <button class="btn" onclick={() => (tplEdit = null)}>Cancel</button>
              <button class="btn primary" onclick={saveTpl}>Save</button>
            </div>
          </div>
        {:else}
          <div class="list">
            {#each templates as t (t.id)}
              <button class="item" onclick={() => editTpl(t)}>
                <span class="item-main"><span class="acct-name" dir="auto">{t.name}</span>{#if t.subject}<span class="acct-email" dir="auto">{t.subject}</span>{/if}</span>
                <span class="chev"><ChevronRight size={15} /></span>
              </button>
            {/each}
            {#if templates.length === 0}<div class="empty">You have no templates. Add one for the replies you write again and again.</div>{/if}
          </div>
          <button class="btn add-btn" onclick={() => editTpl(null)}><Plus size={15} /> New template</button>
        {/if}

      {:else if section === "snooze" && settings}
        <h1>Snooze</h1>
        <p class="intro">Snooze sends an email away and brings it back to your inbox at a better time. Choose your times.</p>
        {@render row("Later", "Snooze for a few hours from now.", laterCtl)}
        {#snippet laterCtl()}
          <select class="input sel" aria-label="Later" value={settings!.snoozeLaterMinutes} onchange={(e) => update({ snoozeLaterMinutes: Number(e.currentTarget.value) })}>
            {#each [1, 2, 3, 4, 8] as h}<option value={h * 60}>{h} {h === 1 ? "hour" : "hours"}</option>{/each}
          </select>
        {/snippet}
        {#each SNOOZE_TIMES as t (t.key)}
          {@render row(t.label, t.desc, timeCtl)}
          {#snippet timeCtl()}
            <input class="input time" type="time" aria-label={t.label} value={settings![t.key]} onchange={(e) => e.currentTarget.value && update({ [t.key]: e.currentTarget.value })} />
          {/snippet}
        {/each}
        <button class="btn add-btn" onclick={resetSnooze}><RotateCcw size={14} /> Reset to defaults</button>

      {:else if section === "shortcuts"}
        <ShortcutsSettings />

      {:else if section === "about"}
        <h1>About</h1>
        <div class="about">
          <img class="app-icon" src="/brand/remail-mark.png" alt="" />
          <div class="about-name">Remail</div>
          <div class="about-ver">{info ? "Version " + info.version : ""}</div>
        </div>
        <p class="intro center">A fast, private email client written in Rust.</p>
        {#if info}
          {@render row("Data folder", null, dirCtl)}
          {#snippet dirCtl()}<span class="path selectable">{info!.dataDir}</span>{/snippet}
        {/if}
        <p class="note">Scheduled emails are sent by Remail on this computer, so Remail needs to be running at the scheduled time.</p>
      {/if}
    </div>
  </main>
</div>

<ConfirmDialog />
<Toasts />

<style>
  .win {
    display: flex;
    height: 100vh;
    background: var(--surface);
  }
  .rail {
    width: 190px;
    flex: none;
    padding: 18px 10px;
    background: var(--surface-nav);
    border-right: 0.5px solid var(--separator);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .rail-title {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--content-muted);
    padding: 0 10px 10px;
  }
  .rail-item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 10px;
    border: none;
    border-radius: var(--radius-lg);
    background: none;
    font-size: 13px;
    text-align: left;
    color: var(--content-subtle);
  }
  .rail-item:hover {
    background: var(--surface-nav-hover);
    color: var(--content);
  }
  .rail-item.active {
    background: var(--surface-nav-active);
    color: var(--content);
    font-weight: 500;
  }
  .rail-item:focus-visible,
  .acct-head:focus-visible,
  .item:focus-visible,
  .seg button:focus-visible {
    outline: none;
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .content {
    flex: 1;
    overflow-y: auto;
  }
  .inner {
    max-width: 620px;
    padding: 28px 32px 48px;
  }
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
    margin-top: 2px;
    font-size: 12px;
    line-height: 16px;
    color: var(--content-subtle);
  }
  .row-control {
    flex: none;
    display: flex;
    align-items: center;
  }
  .sel {
    width: auto;
    min-width: 130px;
  }
  .time {
    width: 110px;
  }
  .narrow {
    width: 240px;
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
  .intro {
    margin: -6px 0 12px;
    color: var(--content-subtle);
    font-size: 13px;
    line-height: 19px;
  }
  .intro.center {
    text-align: center;
  }
  .note {
    margin-top: 18px;
    padding: 10px 12px;
    border-radius: var(--radius-lg);
    background: var(--surface-hover);
    color: var(--content-subtle);
    font-size: 12px;
    line-height: 17px;
  }
  .list {
    border: 0.5px solid var(--separator);
    border-radius: var(--radius-xl);
    background: var(--surface-elevated);
    overflow: hidden;
    margin-bottom: 14px;
  }
  .list > :global(* + *) {
    border-top: 0.5px solid var(--separator-subtle);
  }
  .empty {
    padding: 22px;
    text-align: center;
    color: var(--content-subtle);
  }
  .acct-head,
  .item {
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    padding: 11px 14px;
    border: none;
    background: none;
    text-align: left;
  }
  .acct-head:hover,
  .item:hover {
    background: var(--surface-hover);
  }
  .dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    flex: none;
  }
  .acct-names,
  .item-main {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }
  .acct-name {
    font-weight: 500;
  }
  .acct-email {
    font-size: 12px;
    color: var(--content-subtle);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badge {
    font-size: 11px;
    font-weight: 500;
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--draft-badge);
    color: var(--draft-text);
  }
  .badge.accent {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .chev {
    display: inline-flex;
    color: var(--content-muted);
    transition: transform var(--normal);
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .acct-body {
    padding: 2px 14px 14px;
    background: var(--surface);
  }
  .acct-body .row:first-child {
    border-top: none;
  }
  .swatches {
    display: flex;
    gap: 6px;
  }
  .swatch {
    width: 20px;
    height: 20px;
    border-radius: 50%;
    border: none;
    padding: 0;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .swatch:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .server-line {
    margin-top: 12px;
    font-size: 12px;
    color: var(--content-subtle);
    font-family: var(--font-mono);
  }
  .err-line {
    margin-top: 6px;
    font-size: 12px;
    color: var(--indicator-error);
  }
  .acct-actions {
    display: flex;
    gap: 8px;
    margin-top: 14px;
  }
  .inline-setup {
    margin-top: 14px;
    padding-top: 14px;
    border-top: 0.5px solid var(--separator-subtle);
  }
  .add-btn {
    margin-top: 4px;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 16px;
    border: 0.5px solid var(--separator);
    border-radius: var(--radius-xl);
    background: var(--surface-elevated);
  }
  .card-title {
    font-size: 14px;
    font-weight: 600;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .field label,
  .lbl {
    font-size: 12px;
    font-weight: 500;
    color: var(--content-subtle);
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .check input {
    accent-color: var(--accent);
  }
  .form-actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .grow {
    flex: 1;
  }
  .editor {
    border: 1px solid var(--separator);
    border-radius: var(--radius-lg);
    background: var(--canvas-subtle);
    overflow: hidden;
  }
  .editor:focus-within {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 4px;
    border-bottom: 0.5px solid var(--separator-subtle);
  }
  .toolbar .icon-btn {
    width: 26px;
    height: 26px;
  }
  .link-row {
    display: flex;
    gap: 6px;
    align-items: center;
    padding: 6px;
    border-bottom: 0.5px solid var(--separator-subtle);
  }
  .sig-editor {
    min-height: 120px;
    padding: 10px 12px;
    outline: none;
    line-height: 19px;
    cursor: text;
  }
  .sig-editor :global([style*="font-family"]) {
    font-family: var(--font-family) !important;
  }
  .sig-editor :global(a) {
    color: var(--accent);
  }
  .html-area {
    display: block;
    min-height: 120px;
    height: 160px;
    border: none;
    border-radius: 0;
    padding: 10px 12px;
    resize: vertical;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 17px;
  }
  .html-area:focus {
    box-shadow: none;
  }
  .about {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    padding: 12px 0 8px;
  }
  .rules {
    padding: 8px 0 4px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .rule {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12.5px;
  }
  .rule-email {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .app-icon {
    width: 60px;
    height: 56px;
    object-fit: contain;
    margin-bottom: 6px;
  }
  .about-name {
    font-size: 18px;
    font-weight: 600;
  }
  .about-ver {
    font-size: 12px;
    color: var(--content-subtle);
  }
  .path {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--content-subtle);
    word-break: break-all;
    text-align: right;
  }
</style>
