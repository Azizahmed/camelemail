<script lang="ts">
  let {
    checked,
    onchange,
    label,
    disabled = false,
  }: { checked: boolean; onchange: (v: boolean) => void; label?: string; disabled?: boolean } = $props();
</script>

<button
  type="button"
  role="switch"
  aria-checked={checked}
  aria-label={label}
  class="toggle"
  class:on={checked}
  {disabled}
  onclick={() => onchange(!checked)}
>
  <span class="knob"></span>
</button>

<style>
  .toggle {
    position: relative;
    width: 34px;
    height: 20px;
    border-radius: 999px;
    border: none;
    padding: 0;
    flex: none;
    background: var(--surface-active);
    transition: background var(--normal);
  }
  .toggle.on {
    background: var(--accent);
  }
  .toggle:focus-visible {
    outline: none;
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.25);
    transition: transform var(--normal);
  }
  .on .knob {
    transform: translateX(14px);
  }
  .toggle:disabled {
    opacity: 0.5;
  }
</style>
