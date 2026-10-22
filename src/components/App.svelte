<script lang="ts">
  // Each Tauri window loads index.html with a hash naming its role, and only
  // that window's code: the main window doesn't parse Settings, a compose
  // window doesn't parse the mail list or the calendar.
  //
  // Every window waits for the Remail account (see lib/gate.ts): the main
  // window shows the sign-in screens, the move of an existing setup, the
  // first sync and the first mailbox before the app; compose and settings
  // windows show nothing until it's ready.
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { cloud, initCloud, retryCloud } from "../lib/cloud.svelte";
  import { gateScreen, windowReady } from "../lib/gate";
  import { applyLanguageSetting, t } from "../lib/i18n.svelte";
  import { showCalendar } from "../lib/mail.svelte";
  import { isMobile } from "../lib/platform";
  import { applyTheme } from "../lib/ui.svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import Toasts from "./Toasts.svelte";

  const hash = window.location.hash.replace(/^#/, "");
  const [route, arg] = hash.split("=");
  const main = !(route === "compose" && arg) && route !== "settings";

  /** Mailboxes on this device, asked for once the account is ready. */
  let accounts = $state<number | null>(null);
  /** The app was shown: it stays until signing out (which reloads the window). */
  let entered = $state(false);
  let calendarOnly = $state(false);

  const screen = $derived(
    gateScreen({ status: cloud.status, error: cloud.error, accounts, holdAuth: cloud.holdAuth, entered, skipFirstMailbox: calendarOnly }),
  );
  const ready = $derived(windowReady(cloud.status?.phase));

  onMount(() => {
    void initCloud();
    // Theme and language before the app's own settings load (the sign-in screens).
    api.getSettings().then((s) => {
      applyTheme(s.theme);
      applyLanguageSetting(s.language);
    }).catch(() => {});
  });

  $effect(() => {
    if (main && ready && accounts === null)
      api.accounts().then((a) => (accounts = a.length)).catch(() => (accounts = 0));
  });
  $effect(() => {
    if (screen === "app") entered = true;
  });

  function firstMailboxDone() {
    accounts = Math.max(1, accounts ?? 0);
  }

  function calendarInstead() {
    showCalendar();
    calendarOnly = true;
  }
</script>

{#snippet failed(e: unknown)}
  <p class="load-error">Remail couldn't load this window ({String(e)}). <button onclick={() => location.reload()}>Try again</button></p>
{/snippet}

{#snippet waiting()}
  {#if cloud.error}
    <div class="center">
      <div class="note" role="alert">
        <b>{t("gate.loadError")}</b>
        <span class="selectable detail">{cloud.error}</span>
        <button class="btn" onclick={retryCloud}>{t("common.retry")}</button>
      </div>
    </div>
  {:else if cloud.status}
    <div class="center"><p class="note">{t("gate.signInMain")}</p></div>
  {/if}
  <Toasts />
{/snippet}

{#if route === "compose" && arg}
  {#if ready}
    {#await import("./compose/ComposeWindow.svelte") then { default: ComposeWindow }}
      <ComposeWindow draftId={decodeURIComponent(arg)} />
    {:catch e}
      {@render failed(e)}
    {/await}
  {:else}
    {@render waiting()}
  {/if}
{:else if route === "settings"}
  {#if ready}
    {#await import("./settings/SettingsWindow.svelte") then { default: SettingsWindow }}
      <SettingsWindow initialSection={arg} />
    {:catch e}
      {@render failed(e)}
    {/await}
  {:else}
    {@render waiting()}
  {/if}
{:else if screen === "app" && isMobile}
  {#await import("../mobile/MobileApp.svelte") then { default: MobileApp }}
    <MobileApp />
  {:catch e}
    {@render failed(e)}
  {/await}
{:else if screen === "app"}
  {#await import("./MainWindow.svelte") then { default: MainWindow }}
    <MainWindow />
  {:catch e}
    {@render failed(e)}
  {/await}
{:else}
  {#if screen === "loading"}
    <div class="center"><span class="spinner"></span></div>
  {:else if screen === "error"}
    {@render waiting()}
  {:else if screen === "auth"}
    {#await import("./auth/AuthFlow.svelte") then { default: AuthFlow }}
      <AuthFlow />
    {:catch e}
      {@render failed(e)}
    {/await}
  {:else if screen === "migrate"}
    {#await import("./auth/Migrate.svelte") then { default: Migrate }}
      <Migrate />
    {:catch e}
      {@render failed(e)}
    {/await}
  {:else if screen === "syncing"}
    {#await import("./auth/Syncing.svelte") then { default: Syncing }}
      <Syncing />
    {:catch e}
      {@render failed(e)}
    {/await}
  {:else if screen === "firstMailbox"}
    {#await import("./auth/FirstMailbox.svelte") then { default: FirstMailbox }}
      <FirstMailbox ondone={firstMailboxDone} oncalendar={calendarInstead} />
    {:catch e}
      {@render failed(e)}
    {/await}
  {/if}
  {#if screen !== "error"}
    <ConfirmDialog />
    <Toasts />
  {/if}
{/if}

<style>
  .load-error {
    margin: 40px;
    color: var(--content-subtle);
  }
  .center {
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 24px;
  }
  .note {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    max-width: 360px;
    margin: 0;
    text-align: center;
    color: var(--content-subtle);
    font-size: 13px;
    line-height: 19px;
  }
  .note b {
    color: var(--content);
  }
  .detail {
    font-size: 12px;
    overflow-wrap: anywhere;
  }
</style>
