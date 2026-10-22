<!--
  Settings › Admin › Admins: the organisation's people (account metadata only:
  name, email, role, when they joined and were last active) and who
  administers it. Owners are set on the server; the last administrator can't
  be removed.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { Search } from "@lucide/svelte";
  import { api } from "../../../lib/api";
  import { isCloudError } from "../../../lib/cloud-error";
  import { cloud, refreshCloud } from "../../../lib/cloud.svelte";
  import { errMessage, intlLocale, t, type MessageKey } from "../../../lib/i18n.svelte";
  import type { AdminUser } from "../../../lib/types";
  import { confirmDialog, toast, toastError } from "../../../lib/ui.svelte";

  const LIMIT = 500;

  let users = $state<AdminUser[] | null>(null);
  let error = $state("");
  let query = $state("");
  let busy = $state<string | null>(null);
  /** Owners and admins in the whole organisation (known from an unfiltered list). */
  let admins = $state<number | null>(null);
  let seq = 0;

  const me = $derived(cloud.status?.me ?? null);

  async function load() {
    const mine = ++seq;
    const q = query.trim();
    try {
      const list = await api.adminUsers(q || null);
      if (mine !== seq) return;
      users = list;
      error = "";
      if (!q) admins = list.filter((u) => u.role !== "user" && !u.disabled).length;
    } catch (e) {
      if (mine === seq) error = errMessage(e);
    }
  }

  let timer: ReturnType<typeof setTimeout> | undefined;
  function onsearch() {
    clearTimeout(timer);
    timer = setTimeout(load, 250);
  }

  onMount(() => {
    void load();
    return () => clearTimeout(timer);
  });

  const display = (u: AdminUser) => u.name.trim() || u.email;
  const initial = (u: AdminUser) => ([...display(u)][0] ?? "?").toLocaleUpperCase();

  function date(ms: number | null): string {
    if (!ms) return "";
    try {
      return new Intl.DateTimeFormat(intlLocale(), { dateStyle: "medium" }).format(new Date(ms));
    } catch {
      return new Date(ms).toLocaleDateString();
    }
  }

  async function setRole(u: AdminUser, role: "admin" | "user") {
    const self = u.id === me?.id;
    const name = display(u);
    const r = await confirmDialog({
      title: t(role === "admin" ? "admin.users.makeTitle" : "admin.users.removeTitle", { name }),
      message: role === "admin" ? t("admin.users.makeBody", { name }) : self ? t("admin.users.removeSelfBody") : t("admin.users.removeBody", { name }),
      confirmLabel: t(role === "admin" ? "admin.users.makeAdmin" : "admin.users.removeAdmin"),
      cancelLabel: t("common.cancel"),
      danger: role === "user",
    });
    if (r !== "confirm") return;
    busy = u.id;
    try {
      const updated = await api.adminSetRole(u.id, role);
      if (users) users = users.map((x) => (x.id === updated.id ? updated : x));
      if (admins !== null) admins += role === "admin" ? 1 : -1;
      toast(t(role === "admin" ? "admin.users.made" : "admin.users.removed", { name }), { kind: "success" });
      if (self) await refreshCloud();
    } catch (e) {
      toastError(isCloudError(e, "conflict") ? t("admin.users.lastAdmin") : errMessage(e));
    } finally {
      busy = null;
    }
  }

  const roleKey = (r: AdminUser["role"]): MessageKey => `acct.role.${r}`;

  /** Joined, last active, and anything unusual, with separators. */
  function meta(u: AdminUser): string {
    return [
      t("admin.users.joined", { when: date(u.createdAt) }),
      u.lastActiveAt ? t("acct.lastSeen", { when: date(u.lastActiveAt) }) : "",
      u.verified ? "" : t("admin.users.unconfirmed"),
      u.disabled ? t("admin.users.disabled") : "",
    ]
      .filter(Boolean)
      .join(" · ");
  }
</script>

<p class="intro">{t("admin.users.desc")}</p>

<div class="search">
  <Search size={14} />
  <input class="input" type="search" dir="auto" bind:value={query} oninput={onsearch} placeholder={t("admin.users.search")} aria-label={t("admin.users.search")} />
</div>

{#if error}
  <div class="load-error">
    <span>{error}</span>
    <button class="btn" onclick={load}>{t("common.retry")}</button>
  </div>
{:else if !users}
  <div class="loading"><span class="spinner"></span></div>
{:else}
  <div class="list">
    {#each users as u (u.id)}
      {@const lastAdmin = u.role === "admin" && admins !== null && admins <= 1}
      <div class="item person">
        <span class="avatar" aria-hidden="true">{initial(u)}</span>
        <span class="item-main">
          <span class="name">
            <bdi>{display(u)}</bdi>
            {#if u.id === me?.id}<span class="badge">{t("admin.users.you")}</span>{/if}
          </span>
          <span class="sub"><bdi>{u.email}</bdi></span>
          <span class="meta">{meta(u)}</span>
        </span>
        <span class="role">
          <span class="badge" class:accent={u.role !== "user"}>{t(roleKey(u.role))}</span>
          {#if u.role === "user"}
            <button class="btn" disabled={busy === u.id} onclick={() => setRole(u, "admin")}>{t("admin.users.makeAdmin")}</button>
          {:else if u.role === "admin"}
            <button class="btn" disabled={busy === u.id || lastAdmin} title={lastAdmin ? t("admin.users.lastAdmin") : undefined} onclick={() => setRole(u, "user")}
              >{t("admin.users.removeAdmin")}</button
            >
          {/if}
        </span>
      </div>
    {/each}
    {#if users.length === 0}<div class="empty">{t("admin.users.none")}</div>{/if}
  </div>
  {#if users.length >= LIMIT}<p class="hint">{t("admin.users.more", { n: LIMIT })}</p>{/if}
{/if}

<style>
  .search {
    position: relative;
    display: flex;
    align-items: center;
    margin-bottom: 12px;
    color: var(--content-muted);
  }
  .search :global(svg) {
    position: absolute;
    inset-inline-start: 10px;
    pointer-events: none;
  }
  /* Specific enough to beat the phone's input padding (settings.css). */
  .search input.input[type="search"] {
    padding-inline-start: 32px;
  }
  .person {
    flex-wrap: wrap;
  }
  .avatar {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 32px;
    height: 32px;
    border-radius: 50%;
    background: var(--accent-soft);
    color: var(--accent);
    font-size: 14px;
    font-weight: 600;
  }
  .role {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    margin-inline-start: auto;
  }
  .meta {
    white-space: normal;
  }
</style>
