<script lang="ts">
  // One settings page on the phone. The phone's own pages (pins, swipe,
  // notifications) live in ./settings; every other section is the desktop's
  // settings section itself, embedded and restyled for a phone (settings.css).
  import "./settings/settings.css";
  import { ArrowLeft } from "@lucide/svelte";
  import SettingsWindow from "../components/settings/SettingsWindow.svelte";
  import { t, type MessageKey } from "../lib/i18n.svelte";
  import { back } from "./nav.svelte";
  import NotificationsPage from "./settings/NotificationsPage.svelte";
  import PinsPage from "./settings/PinsPage.svelte";
  import SwipePage from "./settings/SwipePage.svelte";

  let { section }: { section: string } = $props();

  const TITLES: Record<string, MessageKey> = {
    account: "settings.account",
    accounts: "mset.mailboxes",
    notifications: "mset.notifications",
    swipe: "mset.swipe",
    pins: "mset.pinsTitle",
    general: "mset.general",
    autoclean: "mset.autoclean",
    calendar: "mset.calendar",
    signatures: "mset.signatures",
    layouts: "layout.title",
    templates: "mset.templates",
    snooze: "mset.snooze",
    ai: "settings.ai",
    admin: "settings.admin",
    about: "mset.about",
  };
  // `admin:<part>` opens one part of Admin.
  const base = $derived(section.split(":")[0]);
  // Translated sections follow the language; the rest are still English, so they read left to right.
  const TRANSLATED = ["account", "accounts", "ai", "admin", "layouts"];
</script>

<div class="m-screen">
  <header class="m-topbar">
    <button class="m-icon-btn" aria-label={t("common.back")} onclick={() => back()}><ArrowLeft size={20} class="rtl-flip" /></button>
    <h1>{t(TITLES[base] ?? "mset.title")}</h1>
  </header>

  <div class="m-scroll">
    {#if section === "pins"}
      <PinsPage />
    {:else if section === "swipe"}
      <SwipePage />
    {:else if section === "notifications"}
      <NotificationsPage />
    {:else}
      <div class="m-settings-embed" dir={TRANSLATED.includes(base) ? undefined : "ltr"}>
        <SettingsWindow embedded initialSection={section} />
      </div>
    {/if}
  </div>
</div>
