<!-- English / العربية, for the screens shown before signing in. -->
<script lang="ts">
  import { i18n, LOCALES, setLocale, t } from "../../lib/i18n.svelte";
</script>

<div class="langs" role="radiogroup" aria-label={t("lang.label")}>
  {#each LOCALES as l (l.value)}
    <button
      type="button"
      role="radio"
      lang={l.value}
      aria-checked={i18n.locale === l.value}
      class:on={i18n.locale === l.value}
      onclick={() => setLocale(l.value)}>{l.label}</button
    >
  {/each}
</div>

<style>
  .langs {
    display: inline-flex;
    padding: 2px;
    border-radius: var(--radius-lg);
    background: var(--surface-hover);
  }
  button {
    border: none;
    background: none;
    padding: 4px 12px;
    border-radius: 6px;
    font-size: 12px;
    font-weight: 500;
    color: var(--content-subtle);
  }
  button.on {
    background: var(--surface-overlay);
    color: var(--content);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.12);
  }
  button:focus-visible {
    outline: none;
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
</style>
