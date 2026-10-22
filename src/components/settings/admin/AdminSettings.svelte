<!--
  Settings › Admin: the organisation's AI assistant, email providers and
  administrators. Only owners and admins get here (the server refuses
  everyone else too). Secrets are write-only: the server says whether they're
  set, never what they are.
-->
<script lang="ts">
  import "./admin.css";
  import { onMount } from "svelte";
  import { api } from "../../../lib/api";
  import type { AdminTab } from "../../../lib/admin";
  import { cloud } from "../../../lib/cloud.svelte";
  import { errMessage, t, type MessageKey } from "../../../lib/i18n.svelte";
  import type { AdminConfig } from "../../../lib/types";
  import AdminAi from "./AdminAi.svelte";
  import AdminProviders from "./AdminProviders.svelte";
  import AdminUsers from "./AdminUsers.svelte";

  let { tab = $bindable("ai") }: { tab?: AdminTab } = $props();

  const TABS: { id: AdminTab; key: MessageKey }[] = [
    { id: "ai", key: "admin.tab.ai" },
    { id: "providers", key: "admin.tab.providers" },
    { id: "admins", key: "admin.tab.admins" },
  ];

  let config = $state<AdminConfig | null>(null);
  let error = $state("");
  /** Parts opened so far stay mounted, so switching tabs keeps unsaved edits. */
  let opened = $state<Record<string, boolean>>({});
  $effect(() => {
    opened[tab] = true;
  });

  async function load() {
    error = "";
    try {
      config = await api.adminConfig();
    } catch (e) {
      error = errMessage(e);
    }
  }

  onMount(load);

  function onkey(e: KeyboardEvent, i: number) {
    const step = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    if (!step) return;
    // Arrows follow the reading direction.
    const rtl = getComputedStyle(e.currentTarget as Element).direction === "rtl";
    const next = TABS[(i + (rtl ? -step : step) + TABS.length) % TABS.length];
    tab = next.id;
    ((e.currentTarget as HTMLElement).parentElement?.children[TABS.indexOf(next)] as HTMLElement | undefined)?.focus();
    e.preventDefault();
  }
</script>

<div class="adm">
  <h1>{t("settings.admin")}</h1>
  <p class="intro">{t("admin.intro", { org: cloud.status?.me?.orgName || "Camel Mail" })}</p>

  <div class="seg tabs" role="tablist" aria-label={t("admin.tabs")}>
    {#each TABS as x, i (x.id)}
      <button
        role="tab"
        id="adm-tab-{x.id}"
        aria-selected={tab === x.id}
        aria-controls="adm-panel-{x.id}"
        tabindex={tab === x.id ? 0 : -1}
        class:on={tab === x.id}
        onclick={() => (tab = x.id)}
        onkeydown={(e) => onkey(e, i)}>{t(x.key)}</button
      >
    {/each}
  </div>

  {#if opened.admins}
    <div role="tabpanel" id="adm-panel-admins" aria-labelledby="adm-tab-admins" hidden={tab !== "admins"}>
      <AdminUsers />
    </div>
  {/if}
  {#if tab !== "admins"}
    {#if error}
      <div class="load-error">
        <span>{t("admin.loadError")} {error}</span>
        <button class="btn" onclick={load}>{t("common.retry")}</button>
      </div>
    {:else if !config}
      <div class="loading"><span class="spinner"></span></div>
    {/if}
  {/if}
  {#if config}
    {#if opened.ai}
      <div role="tabpanel" id="adm-panel-ai" aria-labelledby="adm-tab-ai" hidden={tab !== "ai"}>
        <AdminAi {config} onsaved={(c) => (config = c)} />
      </div>
    {/if}
    {#if opened.providers}
      <div role="tabpanel" id="adm-panel-providers" aria-labelledby="adm-tab-providers" hidden={tab !== "providers"}>
        <AdminProviders {config} onsaved={(c) => (config = c)} />
      </div>
    {/if}
  {/if}
</div>
