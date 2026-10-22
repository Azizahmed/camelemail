<script lang="ts">
  // Move to the Inbox, Archive, Spam, Trash or a folder of the account.
  import { Archive, Folder, Inbox, ShieldAlert, Trash2 } from "@lucide/svelte";
  import { labelName, mail } from "../lib/mail.svelte";
  import { t } from "../lib/i18n.svelte";
  import { L } from "../lib/types";
  import Sheet from "./Sheet.svelte";

  let {
    open = $bindable(false),
    accountId,
    current,
    onpick,
  }: { open?: boolean; accountId: string; current: string | null; onpick: (label: string) => void } = $props();

  let sheet: Sheet | undefined = $state();

  const options = $derived([
    ...[L.INBOX, L.ARCHIVE, L.SPAM, L.TRASH].map((id) => ({ id, name: labelName(id, accountId) })),
    ...(mail.labels[accountId] ?? []).filter((l) => l.kind === "user").map((l) => ({ id: l.id, name: l.name })),
  ].filter((o) => o.id !== current));

  const iconFor = (id: string) => (id === L.INBOX ? Inbox : id === L.ARCHIVE ? Archive : id === L.TRASH ? Trash2 : id === L.SPAM ? ShieldAlert : Folder);

  async function pick(id: string) {
    await sheet?.close();
    onpick(id);
  }
</script>

<Sheet bind:this={sheet} bind:open title={t("m.move.title")}>
  {#each options as o (o.id)}
    {@const Icon = iconFor(o.id)}
    <button class="m-sheet-item" onclick={() => pick(o.id)}><Icon size={18} strokeWidth={1.6} /> <span dir="auto">{o.name}</span></button>
  {/each}
</Sheet>
