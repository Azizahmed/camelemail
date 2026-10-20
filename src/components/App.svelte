<script lang="ts">
  // Each Tauri window loads index.html with a hash naming its role, and only
  // that window's code: the main window doesn't parse Settings, a compose
  // window doesn't parse the mail list or the calendar.
  const hash = window.location.hash.replace(/^#/, "");
  const [route, arg] = hash.split("=");
</script>

{#snippet failed(e: unknown)}
  <p class="load-error">Remail couldn't load this window ({String(e)}). <button onclick={() => location.reload()}>Try again</button></p>
{/snippet}

{#if route === "compose" && arg}
  {#await import("./compose/ComposeWindow.svelte") then { default: ComposeWindow }}
    <ComposeWindow draftId={decodeURIComponent(arg)} />
  {:catch e}
    {@render failed(e)}
  {/await}
{:else if route === "settings"}
  {#await import("./settings/SettingsWindow.svelte") then { default: SettingsWindow }}
    <SettingsWindow initialSection={arg} />
  {:catch e}
    {@render failed(e)}
  {/await}
{:else}
  {#await import("./MainWindow.svelte") then { default: MainWindow }}
    <MainWindow />
  {:catch e}
    {@render failed(e)}
  {/await}
{/if}

<style>
  .load-error {
    margin: 40px;
    color: var(--content-subtle);
  }
</style>
