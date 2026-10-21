<!-- The backup codes shown once after turning on two-step verification. -->
<script lang="ts">
  import { Copy } from "@lucide/svelte";
  import { t } from "../../lib/i18n.svelte";
  import { toast } from "../../lib/ui.svelte";

  let { codes, ondone }: { codes: string[]; ondone: () => void } = $props();

  let saved = $state(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(codes.join("\n"));
      toast(t("common.copied"));
    } catch {
      /* the codes stay selectable */
    }
  }
</script>

<div class="au-form">
  <h2 class="sub">{t("totp.codesTitle")}</h2>
  <p class="au-help">{t("totp.codesBody")}</p>
  <ol class="codes selectable" dir="ltr">
    {#each codes as c (c)}<li>{c}</li>{/each}
  </ol>
  <button type="button" class="btn" onclick={copy}><Copy size={14} /> {t("totp.copyCodes")}</button>
  <label class="check">
    <input type="checkbox" bind:checked={saved} />
    {t("totp.saved")}
  </label>
  <button type="button" class="btn primary block" disabled={!saved} onclick={ondone}>{t("common.continue")}</button>
</div>

<style>
  .sub {
    margin: 0;
    font-size: 16px;
    line-height: 22px;
    font-weight: 600;
  }
  .codes {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 6px 16px;
    margin: 0;
    padding: 12px 14px;
    list-style: none;
    border-radius: var(--radius-lg);
    background: var(--surface-hover);
    font-family: var(--font-mono);
    font-size: 13.5px;
    line-height: 20px;
    text-align: center;
  }
  .btn {
    align-self: flex-start;
  }
  .btn.block {
    align-self: stretch;
  }
  .check {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    font-size: 13px;
    line-height: 18px;
  }
  .check input {
    margin: 2px 0 0;
    accent-color: var(--accent);
  }
</style>
