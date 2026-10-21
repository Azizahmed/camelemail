<!-- Signed in, no mailbox yet: connect the first one (the usual account setup). -->
<script lang="ts">
  import { CalendarDays } from "@lucide/svelte";
  import { cloud, signOut } from "../../lib/cloud.svelte";
  import { t } from "../../lib/i18n.svelte";
  import type { Account } from "../../lib/types";
  import AccountSetup from "../AccountSetup.svelte";
  import AuthScreen from "./AuthScreen.svelte";

  let { ondone, oncalendar }: { ondone: (a?: Account) => void; oncalendar: () => void } = $props();

  const me = $derived(cloud.status?.me ?? null);
</script>

<AuthScreen title={t("first.title")} lead={t("first.body")} wide>
  <AccountSetup mode="add" compact {ondone} />

  {#snippet footer()}
    <button type="button" class="au-link" onclick={oncalendar}><CalendarDays size={14} /> {t("first.calendarOnly")}</button>
    {#if me}
      <span class="me">
        {t("first.signedInAs", { email: me.email })} ·
        <button type="button" class="au-link" onclick={() => void signOut().catch(() => {})}>{t("common.signOut")}</button>
      </span>
    {/if}
  {/snippet}
</AuthScreen>

<style>
  .me {
    flex-basis: 100%;
    font-size: 12px;
  }
</style>
