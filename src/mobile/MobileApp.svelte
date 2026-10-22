<script lang="ts">
  // The phone app (Android, later iOS): one window of touch screens over the
  // same engine and mail state as the desktop. Screens stack over the mail
  // list (see nav.svelte.ts); compose and settings, which are windows on the
  // desktop, arrive as `mobile-open-compose` / `mobile-open-settings` events.
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { t } from "../lib/i18n.svelte";
  import { initMail, mail } from "../lib/mail.svelte";
  import { dismissStaleTextMenu } from "../lib/platform";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import Toasts from "../components/Toasts.svelte";
  import Drawer from "./Drawer.svelte";
  import MailHome from "./MailHome.svelte";
  import { initNav, nav, push } from "./nav.svelte";

  let failed = $state("");

  function openSettings(section?: string | null) {
    if (section && section !== "home") push({ name: "settingsPage", section });
    else push({ name: "settings" });
  }

  onMount(() => {
    document.documentElement.classList.add("mobile");
    initNav();
    const un: (() => void)[] = [];
    void (async () => {
      un.push(await listen<string>("mobile-open-compose", (e) => push({ name: "compose", draftId: e.payload })));
      un.push(await listen<string | null>("mobile-open-settings", (e) => openSettings(e.payload)));
      try {
        await initMail();
      } catch (e) {
        failed = t("m.app.failed", { error: String(e) });
      }
    })();
    // The status and navigation bars follow the theme.
    const bars = () => {
      const dark = document.documentElement.dataset.theme === "dark";
      const background = getComputedStyle(document.documentElement).getPropertyValue("--m-bg").trim() || (dark ? "#141516" : "#f7f7f8");
      void invoke("set_system_bars", { dark, background }).catch(() => {});
    };
    bars();
    // Back in front after a while in the background: check for mail now.
    let hiddenAt = 0;
    const visibility = () => {
      if (document.hidden) hiddenAt = Date.now();
      else {
        dismissStaleTextMenu();
        if (mail.ready && Date.now() - hiddenAt > 30_000) void api.syncNow().catch(() => {});
      }
    };
    document.addEventListener("visibilitychange", visibility);
    // A system dialog (a permission prompt) closing gives the window focus back.
    window.addEventListener("focus", dismissStaleTextMenu);
    un.push(() => {
      document.removeEventListener("visibilitychange", visibility);
      window.removeEventListener("focus", dismissStaleTextMenu);
    });
    const mo = new MutationObserver(bars);
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    return () => {
      mo.disconnect();
      un.forEach((u) => u());
    };
  });
</script>

{#if failed}
  <div class="m-empty">{failed}</div>
{:else if !mail.ready}
  <div class="m-empty"><span class="spinner"></span></div>
{:else}
  <MailHome onsettings={openSettings} />
  {#each nav.stack as s (s)}
    {#if s.name === "drawer"}
      <Drawer onsettings={openSettings} />
    {:else if s.name === "thread"}
      {#await import("./ThreadScreen.svelte") then { default: ThreadScreen }}
        <ThreadScreen />
      {/await}
    {:else if s.name === "compose"}
      {#await import("./ComposeScreen.svelte") then { default: ComposeScreen }}
        <ComposeScreen screen={s} draftId={s.draftId} />
      {/await}
    {:else if s.name === "search"}
      {#await import("./SearchScreen.svelte") then { default: SearchScreen }}
        <SearchScreen />
      {/await}
    {:else if s.name === "settings"}
      {#await import("./SettingsScreen.svelte") then { default: SettingsScreen }}
        <SettingsScreen onopen={(section: string) => push({ name: "settingsPage", section })} />
      {/await}
    {:else if s.name === "settingsPage"}
      {#await import("./SettingsPage.svelte") then { default: SettingsPage }}
        <SettingsPage section={s.section} />
      {/await}
    {:else if s.name === "calendar"}
      {#await import("./CalendarScreen.svelte") then { default: CalendarScreen }}
        <CalendarScreen />
      {/await}
    {:else if s.name === "assistant"}
      {#await import("./AssistantScreen.svelte") then { default: AssistantScreen }}
        <AssistantScreen />
      {/await}
    {:else if s.name === "scheduled" || s.name === "unsubscribe"}
      {#await import("./ViewScreen.svelte") then { default: ViewScreen }}
        <ViewScreen kind={s.name} />
      {/await}
    {/if}
  {/each}
{/if}
<ConfirmDialog />
<Toasts />
