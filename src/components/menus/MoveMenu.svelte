<script lang="ts">
  import { Folder, Inbox, Archive, Trash2, ShieldAlert, Search } from "@lucide/svelte";
  import { L } from "../../lib/types";
  import { labelName, mail } from "../../lib/mail.svelte";
  import Popover from "./Popover.svelte";

  let {
    anchor,
    accountId,
    current,
    onpick,
    onclose,
  }: { anchor: DOMRect; accountId: string; current: string | null; onpick: (label: string) => void; onclose: () => void } = $props();

  let filter = $state("");
  let index = $state(0);

  const options = $derived.by(() => {
    const labels = mail.labels[accountId] ?? [];
    const system = [L.INBOX, L.ARCHIVE, L.SPAM, L.TRASH].map((id) => ({ id, name: labelName(id, accountId), system: true }));
    const user = labels.filter((l) => l.kind === "user").map((l) => ({ id: l.id, name: l.name, system: false }));
    return [...system, ...user]
      .filter((o) => o.id !== current)
      .filter((o) => !filter || o.name.toLowerCase().includes(filter.toLowerCase()));
  });

  const iconFor = (id: string) =>
    id === L.INBOX ? Inbox : id === L.ARCHIVE ? Archive : id === L.TRASH ? Trash2 : id === L.SPAM ? ShieldAlert : Folder;

  function key(e: KeyboardEvent) {
    if (e.key === "ArrowDown") {
      index = Math.min(options.length - 1, index + 1);
      e.preventDefault();
    } else if (e.key === "ArrowUp") {
      index = Math.max(0, index - 1);
      e.preventDefault();
    } else if (e.key === "Enter" && options[index]) {
      onpick(options[index].id);
    }
  }
</script>

<Popover {anchor} width={250} height={320} {onclose}>
  <div class="search">
    <Search size={14} />
    <!-- svelte-ignore a11y_autofocus -->
    <input placeholder="Move to…" bind:value={filter} onkeydown={key} oninput={() => (index = 0)} autofocus />
  </div>
  <div class="menu-sep"></div>
  {#each options as o, i (o.id)}
    {@const Icon = iconFor(o.id)}
    <button class="menu-item" class:selected={i === index} onmouseenter={() => (index = i)} onclick={() => onpick(o.id)}>
      <Icon size={14} />
      <span>{o.name}</span>
    </button>
  {:else}
    <div class="none">No folders match</div>
  {/each}
</Popover>

<style>
  .search {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px;
    color: var(--content-subtle);
  }
  .search input {
    border: none;
    outline: none;
    background: none;
    flex: 1;
    font-size: 13px;
    padding: 4px 0;
  }
  .none {
    padding: 10px;
    color: var(--content-subtle);
    font-size: 12px;
  }
</style>
