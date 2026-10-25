<script lang="ts">
  // Settings → Branded layouts: the list, and one form with a live preview.
  // Used by the settings window and (embedded) by the phone's settings page.
  import { onMount, tick } from "svelte";
  import { ChevronRight, ImagePlus, Plus, RefreshCw, Sparkles, Trash2, X } from "@lucide/svelte";
  import { api } from "../../lib/api";
  import {
    blockImages, emptyLayout, isButtonUrl, isPublicUrl, LogoError, MAX_ABOUT, MAX_BUTTON, MAX_LAYOUTS, readLogo, TEMPLATES,
  } from "../../lib/layouts";
  import { t, type MessageKey } from "../../lib/i18n.svelte";
  import { isMobile } from "../../lib/platform";
  import { confirmDialog, toast, toastError } from "../../lib/ui.svelte";
  import { back, onLeave, push, type Screen } from "../../mobile/nav.svelte";
  import type { Account, AiSettingsView, BrandedLayout, LayoutPreview, LayoutStyle } from "../../lib/types";
  import EmailFrame from "../thread/EmailFrame.svelte";
  import Select from "../Select.svelte";

  let layouts = $state<BrandedLayout[]>([]);
  let accounts = $state<Account[]>([]);
  let ai = $state<AiSettingsView | null>(null);
  let edit = $state<BrandedLayout | null>(null);
  let loaded = $state(false);

  onMount(() => {
    void load();
    api.accounts().then((a) => (accounts = a)).catch(() => {});
    api.aiSettings().then((a) => (ai = a)).catch(() => {});
  });

  async function load() {
    try {
      layouts = await api.layouts();
    } catch (e) {
      toastError(e);
    } finally {
      loaded = true;
    }
  }

  const scopeLabel = (id: string | null) => (id ? (accounts.find((a) => a.id === id)?.email ?? t("layout.unknownAccount")) : t("layout.allAccounts"));
  const logoSrc = (l: BrandedLayout) => (l.logo?.data ? `data:${l.logo.mime};base64,${l.logo.data}` : null);

  // ---------------------------------------------------------------- editing

  /** The phone's back button closes the form first, like any sheet. */
  let entry: Screen | null = null;
  /** Where the form was last designed: 0 for the first design, +1 per "Regenerate". */
  let variation = $state(0);
  let designed = $state(false);
  let wish = $state("");
  let designing = $state(false);
  let logoUrl = $state("");
  let logoBusy = $state(false);
  /** The button's two fields; the layout gets a button only when both are filled in. */
  let btnLabel = $state("");
  let btnUrl = $state("");

  function open(l: BrandedLayout) {
    edit = l;
    logoUrl = l.logo?.url ?? "";
    btnLabel = l.button?.label ?? "";
    btnUrl = l.button?.url ?? "";
    variation = 0;
    designed = l.id !== "";
    wish = "";
    if (isMobile && !entry) {
      const s: Screen = { name: "sheet", id: "layout-editor" };
      entry = s;
      push(s);
      onLeave(s, () => {
        entry = null;
        edit = null;
      });
    }
    void tick().then(() => document.querySelector(".layouts .scroll-top")?.scrollIntoView({ block: "start" }));
  }

  function close() {
    if (entry) void back();
    else edit = null;
  }

  function create() {
    if (layouts.length >= MAX_LAYOUTS) return toast(t("layout.limit", { count: MAX_LAYOUTS }), { kind: "error" });
    const acc = accounts[0];
    open(emptyLayout({ senderName: acc?.displayName ?? "", email: acc?.email ?? "", isDefault: layouts.length === 0 }));
    // Start from the classic template, in the house colours until there is a logo.
    void api.layoutPreset("classic", []).then((style) => {
      if (edit && !edit.id && !designed) edit.style = style;
    });
  }

  async function chooseLogo(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = "";
    if (!file || !edit) return;
    logoBusy = true;
    try {
      const logo = await readLogo(file, edit.logo?.alt ?? "");
      edit.logo = { ...logo, url: edit.logo?.url ?? null };
      // A fresh logo with no design yet: colours straight from it, no waiting for AI.
      // With AI assistance on, its first design is still to come ("Design with AI assistance").
      if (!designed) {
        edit.style = { ...(await api.layoutPreset(edit.style.template ?? "classic", logo.colors)), tagline: edit.style.tagline };
        if (!ai?.settings.enabled) designed = true;
      }
    } catch (err) {
      toast(t(err instanceof LogoError && err.reason === "tooBig" ? "layout.logoTooBig" : "layout.logoBad"), { kind: "error" });
    } finally {
      logoBusy = false;
    }
  }

  function removeLogo() {
    if (!edit) return;
    edit.logo = null;
    logoUrl = "";
  }

  /** The AI's design when it is on, else the built-in one; the form's own details go with the request. */
  async function design(again: boolean) {
    if (!edit || designing) return;
    if (again) variation++;
    designing = true;
    const request = `layout-${Date.now()}`;
    try {
      let style: LayoutStyle | null = null;
      if (ai?.settings.enabled) {
        try {
          const snap = $state.snapshot(edit) as BrandedLayout;
          style = (await api.aiRun(request, { type: "layout", layout: snap, variation, instructions: wish.trim() }, () => {})).style;
        } catch (err) {
          toast(t("layout.aiFailed", { error: err instanceof Error ? err.message : String(err) }), { kind: "error", duration: 5000 });
        }
      }
      style ??= await api.layoutSuggest(edit?.logo?.colors ?? [], variation);
      // The tagline is the user's text as much as the model's: keep a hand-written one.
      if (edit) edit.style = { ...style, tagline: style.tagline ?? edit.style.tagline };
      designed = true;
    } catch (err) {
      toastError(err);
    } finally {
      designing = false;
    }
  }

  /** A ready-made template, in the logo's colours; "custom" is just the label of hand-made changes. */
  async function applyTemplate(id: string) {
    if (!edit || id === "custom") return;
    try {
      const style = await api.layoutPreset(id, edit.logo?.colors ?? []);
      if (edit) edit.style = { ...style, tagline: edit.style.tagline };
      designed = true;
    } catch (err) {
      toastError(err);
    }
  }

  /** A hand-made change: the design is no longer exactly a template. */
  const customised = () => {
    if (edit) edit.style.template = null;
  };

  async function save() {
    if (!edit) return;
    const l = $state.snapshot(edit) as BrandedLayout;
    l.name = l.name.trim();
    if (!l.name) return toast(t("layout.needName"), { kind: "error" });
    if (!l.orgName.trim() && !l.logo) return toast(t("layout.needOrg"), { kind: "error" });
    if (l.logo) {
      const url = logoUrl.trim();
      if (url && !isPublicUrl(url)) return toast(t("layout.logoUrlBad"), { kind: "error" });
      l.logo = { ...l.logo, url: url || null, alt: l.logo.alt.trim() };
    }
    const [label, url] = [btnLabel.trim(), btnUrl.trim()];
    if (!label !== !url) return toast(t("layout.buttonBoth"), { kind: "error" });
    if (url && !isButtonUrl(url)) return toast(t("layout.buttonUrlBad"), { kind: "error" });
    l.button = label ? { label, url } : null;
    try {
      await api.saveLayout(l);
      toast(t("layout.saved"));
      await load();
      close();
    } catch (e) {
      toastError(e);
    }
  }

  async function remove() {
    if (!edit?.id) return close();
    const r = await confirmDialog({
      title: t("layout.deleteTitle"),
      message: t("layout.deleteBody", { name: edit.name }),
      confirmLabel: t("layout.delete"),
      danger: true,
    });
    if (r !== "confirm") return;
    try {
      await api.deleteLayout(edit.id);
      await load();
      close();
    } catch (e) {
      toastError(e);
    }
  }

  // ---------------------------------------------------------------- preview

  let previewDark = $state(false);
  let previewPhone = $state(false);
  let previewBlocked = $state(false);
  let preview = $state<LayoutPreview | null>(null);
  let previewSeq = 0;

  const sample = () => `<p dir="auto">${t("layout.sampleHi")}</p><p dir="auto">${t("layout.sampleBody")}</p>`;

  // The Rust renderer draws the preview, so what's shown is what is sent.
  $effect(() => {
    if (!edit) return;
    const snap = $state.snapshot(edit) as BrandedLayout;
    if (snap.logo) snap.logo = { ...snap.logo, url: logoUrl.trim() && isPublicUrl(logoUrl) ? logoUrl.trim() : null };
    snap.button = btnLabel.trim() && btnUrl.trim() ? { label: btnLabel.trim(), url: btnUrl.trim() } : null;
    const html = sample();
    const seq = ++previewSeq;
    const timer = setTimeout(async () => {
      try {
        const p = await api.layoutPreview(snap, html);
        if (seq === previewSeq) preview = p;
      } catch {
        // The form is half-filled; keep the last good preview.
      }
    }, 120);
    return () => clearTimeout(timer);
  });

  const shown = $derived(preview ? (previewBlocked ? blockImages(preview.full) : preview.full) : "");
  const ADJUSTED_LABEL: Record<string, string> = {
    accent: "layout.accent",
    textColor: "layout.textColor",
    mutedColor: "layout.mutedColor",
    headerBg: "layout.headerBg",
  };

  // ------------------------------------------------------------------ options

  const accountOptions = $derived([{ value: "", label: t("layout.allAccounts") }, ...accounts.map((a) => ({ value: a.id, label: a.email }))]);
  const templateOptions = $derived([
    ...TEMPLATES.map((id) => ({ value: id as string, label: t(`layout.tpl.${id}` as MessageKey) })),
    { value: "custom", label: t("layout.tpl.custom") },
  ]);
  const COLOR_FIELDS = [
    ["accent", "layout.accent"],
    ["headerBg", "layout.headerBg"],
    ["textColor", "layout.textColor"],
    ["mutedColor", "layout.mutedColor"],
  ] as const;
  const choices = $derived<{ key: keyof LayoutStyle; label: string; options: { value: string; label: string }[] }[]>([
    {
      key: "alignment",
      label: t("layout.headerAlign"),
      options: [
        { value: "left", label: t("layout.left") },
        { value: "center", label: t("layout.center") },
      ],
    },
    {
      key: "divider",
      label: t("layout.divider"),
      options: [
        { value: "none", label: t("layout.dividerNone") },
        { value: "line", label: t("layout.dividerLine") },
        { value: "bar", label: t("layout.dividerBar") },
      ],
    },
    {
      key: "logoPosition",
      label: t("layout.logoPosition"),
      options: [
        { value: "left", label: t("layout.posLeft") },
        { value: "center", label: t("layout.posCenter") },
        { value: "above", label: t("layout.posAbove") },
      ],
    },
    {
      key: "buttonStyle",
      label: t("layout.buttonStyle"),
      options: [
        { value: "filled", label: t("layout.btnFilled") },
        { value: "outline", label: t("layout.btnOutline") },
      ],
    },
    {
      key: "footerAlign",
      label: t("layout.footerAlign"),
      options: [
        { value: "left", label: t("layout.left") },
        { value: "center", label: t("layout.center") },
        { value: "right", label: t("layout.right") },
      ],
    },
    {
      key: "footerLogo",
      label: t("layout.footerLogo"),
      options: [
        { value: "none", label: t("layout.fLogoNone") },
        { value: "beside", label: t("layout.fLogoBeside") },
        { value: "above", label: t("layout.fLogoAbove") },
      ],
    },
  ]);
</script>

<div class="layouts">
  <span class="scroll-top"></span>
  <h1>{t("layout.title")}</h1>

  {#if edit}
    {@const l = edit}
    <div class="card">
      <div class="card-title">{l.id ? t("layout.editTitle") : t("layout.newTitle")}</div>

      <div class="field">
        <label for="ly-name">{t("layout.name")}</label>
        <input id="ly-name" class="input" dir="auto" bind:value={l.name} placeholder={t("layout.namePlaceholder")} />
      </div>
      <div class="field">
        <label for="ly-acct">{t("layout.account")}</label>
        <Select id="ly-acct" value={l.accountId ?? ""} options={accountOptions} onchange={(v) => (l.accountId = v || null)} />
      </div>

      <div class="group">{t("layout.company")}</div>
      <div class="field">
        <label for="ly-org">{t("layout.org")}</label>
        <input id="ly-org" class="input" dir="auto" bind:value={l.orgName} placeholder={t("layout.orgPlaceholder")} />
      </div>
      <div class="field">
        <label for="ly-about">{t("layout.about")}</label>
        <textarea id="ly-about" class="input about" dir="auto" rows="3" maxlength={MAX_ABOUT} bind:value={l.about} placeholder={t("layout.aboutPlaceholder")}></textarea>
        <span class="hint count">{l.about.length} / {MAX_ABOUT}</span>
      </div>

      <div class="field">
        <span class="lbl">{t("layout.logo")}</span>
        <div class="logo-row">
          {#if logoSrc(l)}
            <span class="thumb"><img src={logoSrc(l)} alt="" /></span>
          {/if}
          <label class="btn file" class:disabled={logoBusy}>
            <ImagePlus size={14} />
            {l.logo ? t("layout.logoReplace") : t("layout.logoChoose")}
            <input type="file" accept="image/png,image/jpeg,image/gif,image/webp" disabled={logoBusy} onchange={chooseLogo} />
          </label>
          {#if l.logo}
            <button class="btn" onclick={removeLogo}><X size={14} /> {t("layout.logoRemove")}</button>
          {/if}
        </div>
        <span class="hint">{t("layout.logoHint")}</span>
      </div>
      {#if l.logo}
        <div class="field">
          <label for="ly-alt">{t("layout.logoAlt")}</label>
          <input id="ly-alt" class="input" dir="auto" bind:value={l.logo.alt} />
          <span class="hint">{t("layout.logoAltHint")}</span>
        </div>
        <div class="field">
          <label for="ly-url">{t("layout.logoUrl")}</label>
          <input id="ly-url" class="input" dir="ltr" inputmode="url" bind:value={logoUrl} placeholder="https://" />
          <span class="hint">{t("layout.logoUrlHint")}</span>
        </div>
      {/if}

      <div class="field">
        <label for="ly-btn">{t("layout.buttonLabel")}</label>
        <input id="ly-btn" class="input" dir="auto" maxlength={MAX_BUTTON} bind:value={btnLabel} placeholder={t("layout.buttonPlaceholder")} />
      </div>
      <div class="field">
        <label for="ly-btnurl">{t("layout.buttonUrl")}</label>
        <input id="ly-btnurl" class="input" dir="ltr" inputmode="url" bind:value={btnUrl} placeholder="https://" />
        <span class="hint">{t("layout.buttonUrlHint")}</span>
      </div>

      <div class="group">{t("layout.footer")}</div>
      <div class="field">
        <label for="ly-sender">{t("layout.sender")}</label>
        <input id="ly-sender" class="input" dir="auto" bind:value={l.senderName} autocomplete="name" />
      </div>
      <div class="field">
        <label for="ly-pos">{t("layout.position")}</label>
        <input id="ly-pos" class="input" dir="auto" bind:value={l.position} />
      </div>
      <div class="field">
        <label for="ly-phone">{t("layout.phone")}</label>
        <input id="ly-phone" class="input" dir="ltr" inputmode="tel" bind:value={l.phone} autocomplete="tel" />
      </div>
      <div class="field">
        <label for="ly-email">{t("layout.email")}</label>
        <input id="ly-email" class="input" dir="ltr" inputmode="email" bind:value={l.email} autocomplete="email" />
      </div>
    </div>

    <div class="card">
      <div class="card-title">{t("layout.design")}</div>
      <p class="hint flush">{t("layout.designHelp")}</p>
      <div class="field">
        <label for="ly-tpl">{t("layout.template")}</label>
        <Select id="ly-tpl" value={l.style.template ?? "custom"} options={templateOptions} onchange={applyTemplate} />
      </div>
      {#if ai?.settings.enabled}
        <div class="field">
          <label for="ly-wish">{t("layout.wish")}</label>
          <input id="ly-wish" class="input" dir="auto" bind:value={wish} placeholder={t("layout.wishPlaceholder")} />
        </div>
      {/if}
      <div class="design-actions">
        {#if designed}
          <button class="btn" disabled={designing} onclick={() => design(true)}>
            <RefreshCw size={14} class={designing ? "spin" : ""} /> {designing ? t("layout.designing") : t("layout.regenerate")}
          </button>
        {:else}
          <button class="btn" disabled={designing} onclick={() => design(false)}>
            <Sparkles size={14} /> {designing ? t("layout.designing") : ai?.settings.enabled ? t("layout.generate") : t("layout.generateBuiltIn")}
          </button>
        {/if}
      </div>
    </div>

    <div class="card preview-card">
      <div class="preview-bar">
        <span class="card-title">{t("layout.preview")}</span>
        <span class="grow"></span>
        <div class="seg" role="group" aria-label={t("layout.preview")}>
          <button class:on={!previewDark} aria-pressed={!previewDark} onclick={() => (previewDark = false)}>{t("layout.previewLight")}</button>
          <button class:on={previewDark} aria-pressed={previewDark} onclick={() => (previewDark = true)}>{t("layout.previewDark")}</button>
        </div>
        {#if !isMobile}
          <div class="seg" role="group" aria-label={t("layout.preview")}>
            <button class:on={!previewPhone} aria-pressed={!previewPhone} onclick={() => (previewPhone = false)}>{t("layout.previewDesktop")}</button>
            <button class:on={previewPhone} aria-pressed={previewPhone} onclick={() => (previewPhone = true)}>{t("layout.previewPhone")}</button>
          </div>
        {/if}
      </div>
      <label class="check"><input type="checkbox" bind:checked={previewBlocked} /> {t("layout.previewBlocked")}</label>
      <div class="stage" class:dark={previewDark} class:phone={previewPhone}>
        {#if shown}
          <EmailFrame html={shown} text={null} collapseQuotes={false} dark={previewDark} fitWidth />
        {/if}
      </div>
    </div>

    <div class="card">
      <div class="card-title">{t("layout.tune")}</div>
      <div class="colors">
        {#each COLOR_FIELDS as [key, label] (key)}
          <label class="color">
            <input type="color" bind:value={l.style[key]} oninput={customised} />
            <span>{t(label)}</span>
          </label>
        {/each}
      </div>
      {#if preview && preview.adjusted.length}
        <p class="note">{t("layout.adjusted", { fields: preview.adjusted.map((f) => t((ADJUSTED_LABEL[f] ?? "layout.accent") as MessageKey)).join(", ") })}</p>
      {/if}

      {#each choices as c (c.key)}
        <div class="field">
          <span class="lbl">{c.label}</span>
          <div class="seg" role="group" aria-label={c.label}>
            {#each c.options as o (o.value)}
              <button
                class:on={l.style[c.key] === o.value}
                aria-pressed={l.style[c.key] === o.value}
                onclick={() => {
                  (l.style as unknown as Record<string, unknown>)[c.key] = o.value;
                  customised();
                }}>{o.label}</button>
            {/each}
          </div>
          {#if c.key === "footerAlign"}<span class="hint">{t("layout.alignHint")}</span>{/if}
        </div>
      {/each}
      <div class="field">
        <label for="ly-tag">{t("layout.tagline")}</label>
        <input id="ly-tag" class="input" dir="auto" maxlength="80" value={l.style.tagline ?? ""} oninput={(e) => (l.style.tagline = e.currentTarget.value || null)} />
      </div>

      <label class="check"><input type="checkbox" bind:checked={l.useOnReplies} /> {t("layout.useOnReplies")}</label>
      <label class="check"><input type="checkbox" bind:checked={l.isDefault} /> {t("layout.makeDefault")}</label>
    </div>

    <div class="form-actions">
      <button class="btn danger" onclick={remove}><Trash2 size={14} /> {l.id ? t("layout.delete") : t("layout.discard")}</button>
      <span class="grow"></span>
      <button class="btn" onclick={close}>{t("common.cancel")}</button>
      <button class="btn primary" onclick={save}>{t("layout.save")}</button>
    </div>
  {:else}
    <p class="intro">{t("layout.intro")}</p>
    <div class="list">
      {#each layouts as l (l.id)}
        <button class="item" onclick={() => open({ ...l, style: { ...l.style }, logo: l.logo ? { ...l.logo, colors: [...l.logo.colors] } : null })}>
          {#if logoSrc(l)}<span class="thumb small"><img src={logoSrc(l)} alt="" /></span>{/if}
          <span class="item-main">
            <span class="name" dir="auto">{l.name}</span>
            <span class="sub" dir="auto">{l.orgName || scopeLabel(l.accountId)}{l.orgName ? ` · ${scopeLabel(l.accountId)}` : ""}</span>
          </span>
          {#if l.isDefault}<span class="badge">{t("layout.default")}</span>{/if}
          <span class="chev"><ChevronRight size={15} /></span>
        </button>
      {/each}
      {#if loaded && layouts.length === 0}<div class="empty">{t("layout.empty")}</div>{/if}
    </div>
    <button class="btn add-btn" disabled={layouts.length >= MAX_LAYOUTS} onclick={create}><Plus size={15} /> {t("layout.new")}</button>
    {#if layouts.length >= MAX_LAYOUTS}<p class="hint">{t("layout.limit", { count: MAX_LAYOUTS })}</p>{/if}
  {/if}
</div>

<style>
  .layouts {
    display: block;
  }
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
  .item {
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    padding: 11px 14px;
    border: none;
    background: none;
    text-align: start;
  }
  .item:hover {
    background: var(--surface-hover);
  }
  .item:focus-visible {
    outline: none;
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .item-main {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }
  .name {
    font-weight: 500;
  }
  .sub {
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
    background: var(--accent-soft);
    color: var(--accent);
  }
  .chev {
    display: inline-flex;
    color: var(--content-muted);
  }
  :global([dir="rtl"]) .chev {
    transform: scaleX(-1);
  }
  .add-btn {
    margin-top: 4px;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 16px;
    margin-bottom: 14px;
    border: 0.5px solid var(--separator);
    border-radius: var(--radius-xl);
    background: var(--surface-elevated);
  }
  .card-title {
    font-size: 14px;
    font-weight: 600;
  }
  .group {
    margin-top: 6px;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--content-muted);
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
  .hint {
    font-size: 12px;
    line-height: 17px;
    color: var(--content-subtle);
  }
  .hint.flush {
    margin: -8px 0 0;
  }
  .note {
    margin: 0;
    padding: 9px 12px;
    border-radius: var(--radius-lg);
    background: var(--surface-hover);
    color: var(--content-subtle);
    font-size: 12px;
    line-height: 17px;
  }
  .logo-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .thumb {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 96px;
    height: 48px;
    border: 0.5px solid var(--separator);
    border-radius: var(--radius-lg);
    background: #fff;
    overflow: hidden;
  }
  .thumb.small {
    width: 40px;
    height: 28px;
    flex: none;
  }
  .thumb img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
  }
  .btn.file {
    position: relative;
    cursor: pointer;
  }
  .btn.file.disabled {
    opacity: 0.5;
  }
  .btn.file input {
    position: absolute;
    inset: 0;
    opacity: 0;
    cursor: pointer;
    width: 100%;
  }
  .design-actions {
    display: flex;
    gap: 8px;
  }
  textarea.about {
    height: auto;
    min-height: 74px;
    padding: 8px 10px;
    resize: vertical;
    line-height: 19px;
    font-family: inherit;
  }
  .hint.count {
    align-self: flex-end;
    margin-top: -2px;
  }
  .colors {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: 10px;
  }
  .color {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .color input {
    width: 34px;
    height: 26px;
    padding: 0;
    border: 1px solid var(--separator);
    border-radius: 6px;
    background: none;
    flex: none;
  }
  .seg {
    display: inline-flex;
    align-self: flex-start;
    flex-wrap: wrap;
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
  .seg button:focus-visible {
    outline: none;
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .seg button.on {
    background: var(--surface-overlay);
    color: var(--content);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.12);
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .check input {
    accent-color: var(--accent);
  }
  .preview-bar {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
  }
  .grow {
    flex: 1;
  }
  /* The preview shows the mail on the colour a reader's window would have. */
  .stage {
    width: 100%;
    max-width: 600px;
    margin: 0 auto;
    padding: 12px;
    border-radius: var(--radius-lg);
    background: #ffffff;
    border: 0.5px solid var(--separator);
    box-sizing: border-box;
  }
  .stage.phone {
    max-width: 375px;
  }
  .stage.dark {
    background: rgb(30, 31, 32);
  }
  .form-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
  }
  :global(.spin) {
    animation: layout-spin 0.9s linear infinite;
  }
  @keyframes layout-spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
