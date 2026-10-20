<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import { api, errorText } from "../../lib/api";
  import type { AppSettings, Draft } from "../../lib/types";
  import { applyTheme } from "../../lib/ui.svelte";
  import ConfirmDialog from "../ConfirmDialog.svelte";
  import Toasts from "../Toasts.svelte";
  import Composer from "./Composer.svelte";

  let { draftId }: { draftId: string } = $props();

  const win = getCurrentWindow();
  let draft: Draft | null = $state(null);
  let error = $state("");
  let composer: Composer | undefined = $state();

  onMount(() => {
    let unlisten: (() => void)[] = [];
    (async () => {
      try {
        const s = await api.getSettings();
        applyTheme(s.theme);
        draft = await api.getDraft(draftId);
        if (!draft) error = "This draft no longer exists.";
      } catch (e) {
        error = errorText(e);
      }
      unlisten.push(await listen<AppSettings>("settings-changed", (e) => applyTheme(e.payload.theme)));
      unlisten.push(
        await win.onCloseRequested(async (e) => {
          if (!composer) return;
          e.preventDefault();
          if (await composer.requestClose()) await win.destroy();
        }),
      );
    })();
    return () => unlisten.forEach((u) => u());
  });

  let subject = $state("");
  let shownTitle = "";

  $effect(() => {
    // Follows the subject as it is typed.
    const title = subject.trim() ? `${subject.trim()} - Remail` : "New Message - Remail";
    if (title === shownTitle) return;
    shownTitle = title;
    void win.setTitle(title);
  });

  function done() {
    void win.destroy();
  }

  async function onKey(e: KeyboardEvent) {
    // Ctrl/Cmd+W closes the window (with the save-draft prompt).
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "w") {
      e.preventDefault();
      if (composer && (await composer.requestClose())) await win.destroy();
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="compose-window">
  {#if draft}
    <div class="inner">
      <Composer bind:this={composer} {draft} variant="window" onclose={done} onsubject={(s) => (subject = s)} />
    </div>
  {:else if error}
    <div class="error">{error}</div>
  {:else}
    <div class="error"><span class="spinner"></span></div>
  {/if}
</div>
<ConfirmDialog />
<Toasts />

<style>
  .compose-window {
    height: 100vh;
    overflow-y: auto;
    background: var(--surface-overlay);
  }
  .inner {
    max-width: 720px;
    margin: 0 auto;
    padding: 6px 24px 24px;
  }
  .error {
    display: flex;
    justify-content: center;
    padding: 30vh 20px;
    color: var(--content-subtle);
  }
</style>
