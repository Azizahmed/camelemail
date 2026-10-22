<script lang="ts">
  // Settings › Pinned labels: up to MAX_PINS views on the bottom bar, in order.
  // Every change is saved at once (UI prefs, so other phones follow).
  import { Check, ChevronDown, ChevronUp, RotateCcw } from "@lucide/svelte";
  import { flip } from "svelte/animate";
  import { t } from "../../lib/i18n.svelte";
  import { mail } from "../../lib/mail.svelte";
  import { L } from "../../lib/types";
  import { DEFAULT_PINS, MAX_PINS, pins, setPins, SMART_BUCKETS, viewIcon, viewName, type ViewId } from "../views.svelte";

  const SYSTEM: ViewId[] = [L.INBOX, L.UNREAD, L.STARRED, L.SNOOZED, L.ARCHIVE, L.SENT, L.SCHEDULED, L.DRAFTS, L.SPAM, L.TRASH, "calendar"];

  const pinned = $derived(pins());
  const full = $derived(pinned.length >= MAX_PINS);

  /** What can be pinned, grouped; the pinned ones are listed above instead. */
  const groups = $derived.by(() => {
    const out: { title: string; ids: ViewId[] }[] = [{ title: t("mset.views"), ids: SYSTEM }];
    if (mail.settings?.smartInbox) out.push({ title: t("mset.smartInbox"), ids: SMART_BUCKETS.map((b) => `smart:${b.id}`) });
    const many = mail.accounts.length > 1;
    for (const a of mail.accounts) {
      const ids = (mail.labels[a.id] ?? []).filter((l) => l.kind === "user").map((l) => `folder:${a.id}:${l.id}`);
      if (ids.length) out.push({ title: many ? a.email : t("mset.folders"), ids });
    }
    return out.map((g) => ({ ...g, ids: g.ids.filter((id) => !pinned.includes(id)) })).filter((g) => g.ids.length > 0);
  });

  function toggle(id: ViewId) {
    if (pinned.includes(id)) {
      // The bar keeps at least one view.
      if (pinned.length > 1) setPins(pinned.filter((x) => x !== id));
    } else if (!full) setPins([...pinned, id]);
  }

  function move(i: number, by: -1 | 1) {
    const next = [...pinned];
    [next[i], next[i + by]] = [next[i + by], next[i]];
    setPins(next);
  }

  const isDefault = $derived(pinned.join() === DEFAULT_PINS.join());
</script>

{#snippet box(on: boolean)}
  <span class="box" class:on aria-hidden="true">{#if on}<Check size={14} strokeWidth={3} />{/if}</span>
{/snippet}

<p class="ms-intro">{t("mset.pinsIntro", { n: MAX_PINS })}</p>

<div class="m-section-title">{t("mset.pinned")} · {pinned.length}/{MAX_PINS}</div>
{#each pinned as id, i (id)}
  {@const Icon = viewIcon(id)}
  <div class="m-card pin" animate:flip={{ duration: 160 }}>
    <button class="m-row" role="checkbox" aria-checked="true" disabled={pinned.length <= 1} onclick={() => toggle(id)}>
      {@render box(true)}
      <Icon size={20} strokeWidth={1.5} />
      <span class="m-row-text"><span class="ms-row-title">{viewName(id)}</span></span>
    </button>
    <button class="m-icon-btn" aria-label={t("mset.moveUp")} disabled={i === 0} onclick={() => move(i, -1)}><ChevronUp size={18} /></button>
    <button class="m-icon-btn" aria-label={t("mset.moveDown")} disabled={i === pinned.length - 1} onclick={() => move(i, 1)}><ChevronDown size={18} /></button>
  </div>
{/each}

{#if full}<p class="full">{t("mset.pinsFull")}</p>{/if}

{#each groups as g (g.title)}
  <div class="m-section-title" dir="auto">{g.title}</div>
  {#each g.ids as id (id)}
    {@const Icon = viewIcon(id)}
    <div class="m-card">
      <button class="m-row" role="checkbox" aria-checked="false" disabled={full} onclick={() => toggle(id)}>
        {@render box(false)}
        <Icon size={20} strokeWidth={1.5} />
        <span class="m-row-text"><span class="ms-row-title" dir="auto">{viewName(id)}</span></span>
      </button>
    </div>
  {/each}
{/each}

<div class="reset">
  <button class="m-primary soft" disabled={isDefault} onclick={() => setPins([...DEFAULT_PINS])}><RotateCcw size={15} /> {t("mset.resetDefault")}</button>
</div>
<div class="ms-end"></div>

<style>
  .pin {
    display: flex;
    align-items: center;
    padding-inline-end: 5px;
  }
  .pin .m-row {
    flex: 1;
    min-width: 0;
  }
  .m-row > :global(svg) {
    flex: none;
    color: var(--m-icon);
  }
  .m-row:disabled {
    opacity: 1;
  }
  .m-card:has(.m-row[aria-checked="false"]:disabled) {
    opacity: 0.5;
  }
  .m-icon-btn:disabled {
    opacity: 0.3;
  }
  .box {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    border: 2px solid var(--m-text-3);
    border-radius: 6px;
    color: #fff;
  }
  .box.on {
    border-color: var(--m-accent);
    background: var(--m-accent);
  }
  .full {
    margin: 8px var(--m-pad) 0;
    color: var(--m-text-2);
    font-size: 12px;
  }
  .reset {
    display: flex;
    justify-content: center;
    margin-top: 23px;
  }
  .reset .m-primary:disabled {
    opacity: 0.5;
  }
</style>
