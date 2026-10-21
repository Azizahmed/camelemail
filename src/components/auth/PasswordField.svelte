<!-- A password input with a show/hide button and, for new passwords, a strength meter. -->
<script lang="ts">
  import { Eye, EyeOff } from "@lucide/svelte";
  import { autofocus } from "../../lib/actions";
  import { t } from "../../lib/i18n.svelte";
  import StrengthMeter from "./StrengthMeter.svelte";

  let {
    id,
    label,
    value = $bindable(""),
    autocomplete = "current-password",
    meter = false,
    personal = [],
    focus = false,
    placeholder,
  }: {
    id: string;
    label: string;
    value?: string;
    autocomplete?: "current-password" | "new-password";
    /** Show how strong the password is (new passwords). */
    meter?: boolean;
    /** Name and e-mail words that weaken it. */
    personal?: string[];
    focus?: boolean;
    placeholder?: string;
  } = $props();

  let shown = $state(false);
</script>

<div class="au-field">
  <label for={id}>{label}</label>
  <div class="wrap">
    <input
      {id}
      class="input"
      type={shown ? "text" : "password"}
      dir="ltr"
      bind:value
      {autocomplete}
      {placeholder}
      spellcheck="false"
      autocapitalize="off"
      use:autofocus={focus}
      aria-describedby={meter && value ? `${id}-meter` : undefined}
    />
    <button
      type="button"
      class="eye"
      aria-label={shown ? t("common.hidePassword") : t("common.showPassword")}
      title={shown ? t("common.hidePassword") : t("common.showPassword")}
      aria-pressed={shown}
      onclick={() => (shown = !shown)}
    >
      {#if shown}<EyeOff size={15} />{:else}<Eye size={15} />{/if}
    </button>
  </div>
  {#if meter && value}
    <StrengthMeter id="{id}-meter" password={value} {personal} />
  {/if}
</div>

<style>
  .wrap {
    position: relative;
  }
  .wrap .input {
    padding-inline-end: 36px;
  }
  .eye {
    position: absolute;
    inset-inline-end: 4px;
    top: 50%;
    transform: translateY(-50%);
    width: 26px;
    height: 26px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    border-radius: var(--radius-md);
    background: none;
    color: var(--icon);
  }
  .eye:hover {
    color: var(--icon-hover);
    background: var(--surface-hover);
  }
  .eye:focus-visible {
    outline: none;
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
</style>
