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
    templates: "mset.templates",
    snooze: "mset.snooze",
    ai: "settings.ai",
    admin: "settings.ai",
    about: "mset.about",
  };
</script>

<div class="m-screen">
  <header class="m-topbar">
    <button class="m-icon-btn" aria-label={t("common.back")} onclick={() => back()}><ArrowLeft size={20} class="rtl-flip" /></button>
    <h1>{t(TITLES[section] ?? "mset.title")}</h1>
  </header>

  <div class="m-scroll">
    {#if section === "pins"}
      <PinsPage />
    {:else if section === "swipe"}
      <SwipePage />
    {:else if section === "notifications"}
      <NotificationsPage />
    {:else}
      <!-- Account and the add-mailbox form are translated; the other sections are still English, so they read left to right. -->
      <div class="m-settings-embed" dir={section === "account" || section === "accounts" ? undefined : "ltr"}>
        <SettingsWindow embedded initialSection={section} />
      </div>
    {/if}
  </div>
</div>
