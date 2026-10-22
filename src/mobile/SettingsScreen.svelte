<script lang="ts">
  // Settings home on the phone: the Camel Mail account at the top, then one card
  // per settings page. The desktop-only sections (Connections, Shortcuts) are
  // left out.
  import {
    ArrowLeft, Bell, CalendarDays, Check, Clock, Eraser, FileText, Info, LogOut, Mailbox, MoveHorizontal, PenLine, Pin,
    ShieldCheck, SlidersHorizontal, Sparkles, X,
  } from "@lucide/svelte";
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { cloud, refreshCloud, setMe, signOut } from "../lib/cloud.svelte";
  import { errMessage, t, type MessageKey } from "../lib/i18n.svelte";
  import { mail } from "../lib/mail.svelte";
  import { confirmDialog, toastError } from "../lib/ui.svelte";
  import { back } from "./nav.svelte";
  import { followPermission, notify } from "./settings/notify.svelte";
  import type { Icon } from "./views.svelte";

  let { onopen }: { onopen: (section: string) => void } = $props();

  const me = $derived(cloud.status?.me ?? null);
  const initial = $derived(([...(me?.name.trim() || me?.email || "")][0] ?? "").toLocaleUpperCase());
  let devices = $state<number | null>(null);

  const count = (n: number, one: MessageKey, many: MessageKey) => (n === 1 ? t(one) : t(many, { n }));
  const accounts = $derived(mail.accounts.length);
  const needsPermission = $derived(notify.permission !== null && notify.permission !== "granted");

  const rows = $derived<{ id: string; icon: Icon; title: string; sub?: string; dot?: boolean }[]>([
    { id: "accounts", icon: Mailbox, title: t("mset.mailboxes"), sub: accounts === 0 ? t("mset.accounts.none") : count(accounts, "mset.accounts.one", "mset.accounts") },
    {
      id: "notifications",
      icon: Bell,
      title: t("mset.notifications"),
      sub: needsPermission ? t("mset.permissionNeeded") : mail.settings ? t(mail.settings.notifications ? "mset.on" : "mset.off") : undefined,
      dot: needsPermission,
    },
    { id: "swipe", icon: MoveHorizontal, title: t("mset.swipe") },
    { id: "pins", icon: Pin, title: t("mset.pins"), sub: t("mset.pinsDesc") },
    { id: "general", icon: SlidersHorizontal, title: t("mset.general"), sub: t("mset.generalDesc") },
    { id: "autoclean", icon: Eraser, title: t("mset.autoclean") },
    { id: "calendar", icon: CalendarDays, title: t("mset.calendar") },
    { id: "signatures", icon: PenLine, title: t("mset.signatures") },
    { id: "templates", icon: FileText, title: t("mset.templates") },
    { id: "snooze", icon: Clock, title: t("mset.snooze") },
    { id: "ai", icon: Sparkles, title: t("settings.ai") },
    // Owners and administrators of the organisation only.
    ...(me?.role === "owner" || me?.role === "admin" ? [{ id: "admin", icon: ShieldCheck, title: t("settings.admin"), sub: t("admin.sub") }] : []),
    { id: "about", icon: Info, title: t("mset.about") },
  ]);

  async function signOutHere() {
    const r = await confirmDialog({
      title: t("acct.signOutTitle"),
      message: t("mset.signOutBody"),
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
    api.cloudMe().then(setMe, () => refreshCloud());
    api.cloudDevices().then((d) => (devices = d.length), () => {});
    return followPermission();
  });
</script>

<div class="m-screen">
  <header class="m-topbar">
    <button class="m-icon-btn" aria-label={t("common.back")} onclick={() => back()}><ArrowLeft size={20} class="rtl-flip" /></button>
    <h1>{t("mset.title")}</h1>
  </header>

  <div class="m-scroll">
    {#if me}
      <div class="profile">
        <div class="avatar" aria-hidden="true">{initial}</div>
        <div class="name" dir="auto">{me.name}</div>
        <div class="email" dir="ltr">{me.email}</div>
      </div>

      <section class="account">
        <div class="account-head">
          <h2>{t("mset.remailAccount")}</h2>
          <span class="tag" dir="auto">{me.orgName || t("mset.synced")}</span>
        </div>
        <p class="pitch">{t("mset.accountPitch")}</p>
        <ul class="checks">
          {#if accounts > 0}
            <li><Check size={16} strokeWidth={2.2} /> {count(accounts, "mset.mailboxesSynced.one", "mset.mailboxesSynced")}</li>
          {/if}
          <li class:off={!me.totp}>
            {#if me.totp}<Check size={16} strokeWidth={2.2} />{:else}<X size={16} strokeWidth={2.2} />{/if}
            {t(me.totp ? "mset.twofaOn" : "mset.twofaOff")}
          </li>
          {#if devices}
            <li><Check size={16} strokeWidth={2.2} /> {count(devices, "mset.devices.one", "mset.devices")}</li>
          {/if}
        </ul>
        <button class="m-primary manage" onclick={() => onopen("account")}>{t("mset.manageAccount")}</button>
      </section>
    {/if}

    <div class="m-section-title">{t("mset.preferences")}</div>
    {#each rows as r (r.id)}
      <div class="m-card">
        <button class="m-row" onclick={() => onopen(r.id)}>
          <r.icon size={21} strokeWidth={1.5} />
          <span class="m-row-text">
            <span class="title">{r.title}{#if r.dot}<span class="red-dot" aria-hidden="true"></span>{/if}</span>
            {#if r.sub}<small>{r.sub}</small>{/if}
          </span>
        </button>
      </div>
    {/each}

    {#if me}
      <div class="m-card sign-out">
        <button class="m-row" onclick={signOutHere}>
          <LogOut size={21} strokeWidth={1.5} class="rtl-flip" />
          <span class="m-row-text"><span class="title">{t("common.signOut")}</span></span>
        </button>
      </div>
    {/if}
    <div class="end"></div>
  </div>
</div>

<style>
  .profile {
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 10px var(--m-pad) 0;
    text-align: center;
  }
  .avatar {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 82px;
    height: 82px;
    border-radius: 50%;
    background: var(--m-avatar);
    color: #fff;
    font-size: 39px;
    font-weight: 500;
    line-height: 1;
  }
  .name {
    margin-top: 15px;
    font-size: 25px;
    font-weight: 700;
    line-height: 30px;
    overflow-wrap: anywhere;
  }
  .email {
    margin-top: 3px;
    font-size: 14px;
    color: var(--m-text-2);
    overflow-wrap: anywhere;
  }

  .account {
    margin: 23px 15px 0;
    padding: 20px;
    border-radius: 20px;
    background: var(--m-surface-2);
  }
  .account-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  h2 {
    margin: 0;
    font-size: 18px;
    font-weight: 600;
  }
  .tag {
    padding: 2px 8px;
    border-radius: 819px;
    background: var(--m-accent-soft);
    color: var(--m-accent);
    font-size: 11px;
    font-weight: 600;
  }
  .pitch {
    margin: 7px 0 15px;
    color: var(--m-text-2);
    font-size: 13px;
    line-height: 18px;
  }
  .checks {
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin: 0 0 20px;
    padding: 0;
    list-style: none;
    font-size: 13px;
  }
  .checks li {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .checks li :global(svg) {
    flex: none;
    color: var(--m-accent);
  }
  .checks li.off :global(svg) {
    color: var(--m-text-3);
  }
  .manage {
    width: 100%;
    height: 43px;
    border-radius: 21px;
    background: var(--m-accent);
    color: #fff;
  }
  :global(html[data-theme="dark"]) .manage {
    background: #b9772a;
  }

  .m-row :global(svg) {
    flex: none;
    color: var(--m-icon);
  }
  .title {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    font-size: 16px;
  }
  .red-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--m-swipe-delete);
  }
  .sign-out {
    margin-top: 16px;
  }
  .sign-out .m-row,
  .sign-out .m-row :global(svg) {
    color: var(--m-swipe-delete);
  }
  .end {
    height: 33px;
  }
</style>
