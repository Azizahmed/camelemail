<!-- The first screen: create an account or sign in, the language, and "Using Camel Mail at work?". -->
<script lang="ts">
  import { Building2 } from "@lucide/svelte";
  import { t } from "../../lib/i18n.svelte";
  import type { Meta } from "../../lib/types";
  import AuthScreen from "./AuthScreen.svelte";

  let {
    meta,
    server,
    custom,
    connecting,
    unreachable,
    outdated,
    oncreate,
    onsignin,
    onserver,
    onretry,
  }: {
    meta: Meta | null;
    /** The server in use or about to be (null: none chosen and no default). */
    server: string | null;
    /** The server isn't the build's default (show it, with "Change"). */
    custom: boolean;
    connecting: boolean;
    unreachable: boolean;
    /** The server needs a newer app: this version. */
    outdated: string | null;
    oncreate: () => void;
    onsignin: () => void;
    onserver: () => void;
    onretry: () => void;
  } = $props();

  const host = $derived(server ? server.replace(/^https?:\/\//, "").replace(/\/$/, "") : "");
  const licenceBad = $derived(meta?.licence === "expired" || meta?.licence === "invalid");
  const blocked = $derived(!!outdated || unreachable);
</script>

<AuthScreen logo title={meta?.orgName ? t("welcome.org", { org: meta.orgName }) : t("welcome.title")} lead={t("welcome.subtitle")}>
  {#if outdated}
    <div class="au-error" role="alert">{t("welcome.update", { version: outdated })}</div>
  {:else if unreachable}
    <div class="au-error" role="alert">
      {t("welcome.unreachable", { server: host })}
      <button type="button" class="au-link" onclick={onretry}>{t("common.retry")}</button>
    </div>
  {:else if licenceBad}
    <div class="au-info" role="status">{t("welcome.licence")}</div>
  {/if}

  <div class="buttons">
    <button type="button" class="btn primary block" disabled={blocked || connecting} onclick={oncreate}>
      {meta?.signup === "invite" ? t("welcome.invite") : t("welcome.create")}
    </button>
    <button type="button" class="btn block" disabled={blocked || connecting} onclick={onsignin}>{t("welcome.signIn")}</button>
  </div>

  {#if connecting}
    <p class="status" role="status"><span class="spinner"></span> {t("welcome.connecting", { server: host })}</p>
  {:else if custom && server}
    <p class="status">
      <span class="host">{t("welcome.server", { server: host })}</span>
      <button type="button" class="au-link" onclick={onserver}>{t("welcome.changeServer")}</button>
    </p>
  {/if}

  {#snippet footer()}
    {#if !custom || !server}
      <button type="button" class="au-link work" onclick={onserver}><Building2 size={14} /> {t("welcome.work")}</button>
    {/if}
  {/snippet}
</AuthScreen>

<style>
  .buttons {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 4px;
  }
  .status {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: center;
    gap: 6px;
    margin: 0;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .host {
    overflow-wrap: anywhere;
  }
  .work {
    font-size: 13px;
  }
</style>
