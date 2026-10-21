<!-- Four bars and a word for how strong a new password looks (the server decides). -->
<script lang="ts">
  import { t, type MessageKey } from "../../lib/i18n.svelte";
  import { scorePassword } from "../../lib/password";

  let { id, password, personal = [] }: { id?: string; password: string; personal?: string[] } = $props();

  const strength = $derived(scorePassword(password, personal));
  const label = $derived(t(`pw.score.${strength.score}` as MessageKey));
  const hint = $derived(strength.hints[0]);
</script>

<div {id} class="meter s{strength.score}" aria-live="polite">
  <div class="bars" role="img" aria-label={t("pw.strength", { label })}>
    {#each [1, 2, 3, 4] as i (i)}
      <span class:on={strength.score >= i || (i === 1 && password.length > 0)}></span>
    {/each}
  </div>
  <div class="text">
    <span class="label">{label}</span>
    {#if hint}<span class="hint">{t(`pw.hint.${hint}` as MessageKey)}</span>{/if}
  </div>
</div>

<style>
  .meter {
    display: flex;
    flex-direction: column;
    gap: 5px;
    margin-top: 2px;
    --c: var(--indicator-error);
  }
  .s2 {
    --c: var(--indicator-warning);
  }
  .s3,
  .s4 {
    --c: var(--indicator-success);
  }
  .bars {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 4px;
  }
  .bars span {
    height: 4px;
    border-radius: 2px;
    background: var(--surface-active);
    transition: background var(--normal);
  }
  .bars span.on {
    background: var(--c);
  }
  .text {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 8px;
    font-size: 12px;
    line-height: 16px;
  }
  .label {
    font-weight: 600;
    color: var(--c);
  }
  .hint {
    color: var(--content-subtle);
  }
</style>
