<script lang="ts">
  // Writing an email on the phone: the desktop composer, full screen. Back asks
  // to keep or discard the draft (as closing a compose window does).
  import { X } from "@lucide/svelte";
  import { onMount } from "svelte";
  import { api, errorText } from "../lib/api";
  import { t } from "../lib/i18n.svelte";
  import type { Draft } from "../lib/types";
  import Composer from "../components/compose/Composer.svelte";
  import { back, guard, type Screen } from "./nav.svelte";

  let { screen, draftId }: { screen: Screen; draftId: string } = $props();

  let draft = $state<Draft | null>(null);
  let error = $state("");
  let subject = $state("");
  let composer: Composer | undefined = $state();
  /** Set once the composer closed itself (sent, discarded, saved): back then just leaves. */
  let finished = false;
  /** Back was pressed: the screen is already on its way out. */
  let leaving = false;

  onMount(() => {
    void (async () => {
      try {
        draft = await api.getDraft(draftId);
        if (!draft) error = t("m.compose.missing");
      } catch (e) {
        error = errorText(e);
      }
    })();
    return guard(screen, async () => {
      if (finished || !composer) return true;
      leaving = true;
      const ok = await composer.requestClose();
      leaving = false;
      return ok;
    });
  });

  function done() {
    if (finished) return;
    finished = true;
    if (!leaving) void back();
  }
</script>

<div class="m-screen compose">
  <header class="m-topbar">
    <button class="m-icon-btn" aria-label={t("m.compose.close")} onclick={() => back()}><X size={20} strokeWidth={1.6} /></button>
    <h1 dir="auto">{subject.trim() || (draft?.replyKind ? t("m.thread.reply") : t("m.compose.newMessage"))}</h1>
  </header>
  <div class="m-scroll body">
    {#if draft}
      <Composer bind:this={composer} {draft} variant="window" onclose={done} onsubject={(s) => (subject = s)} />
    {:else if error}
      <div class="m-empty">{error}</div>
    {:else}
      <div class="m-empty"><span class="spinner"></span></div>
    {/if}
  </div>
</div>

<style>
  .compose {
    background: var(--surface-overlay);
  }
  .compose .m-topbar {
    background: var(--surface-overlay);
  }
  .body {
    padding: 0 11px 20px;
  }
</style>
