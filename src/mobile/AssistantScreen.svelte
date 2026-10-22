<script lang="ts">
  // The AI assistant chat, full screen. Its own close button leaves the screen.
  import { onDestroy } from "svelte";
  import { mail } from "../lib/mail.svelte";
  import AssistantPanel from "../components/assistant/AssistantPanel.svelte";
  import { back } from "./nav.svelte";

  mail.assistantOpen = true;
  let leaving = false;

  $effect(() => {
    if (!mail.assistantOpen && !leaving) {
      leaving = true;
      void back();
    }
  });

  onDestroy(() => {
    leaving = true;
    mail.assistantOpen = false;
  });
</script>

<!-- The assistant panel isn't translated yet: it reads left to right. -->
<div class="m-screen assistant" dir="ltr"><AssistantPanel /></div>

<style>
  .assistant {
    background: var(--surface);
  }
  .assistant > :global(*) {
    flex: 1;
    min-height: 0;
    width: 100% !important;
    max-width: none !important;
  }
</style>
