<!--
  The frame of every sign-in screen: the language switch at the top, a
  centred card, and an optional back button. One column, so it also works at
  phone widths.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { ChevronLeft } from "@lucide/svelte";
  import { t } from "../../lib/i18n.svelte";
  import { isTyping } from "../../lib/ui.svelte";
  import LanguageSwitch from "./LanguageSwitch.svelte";

  let {
    title,
    lead,
    onback,
    wide = false,
    logo = false,
    children,
    footer,
  }: {
    title?: string;
    lead?: string;
    onback?: () => void;
    /** Room for the mailbox setup form. */
    wide?: boolean;
    /** The app mark above the title (the first screen). */
    logo?: boolean;
    children: Snippet;
    footer?: Snippet;
  } = $props();

  function onkeydown(e: KeyboardEvent) {
    // Escape goes back, unless it's leaving a field.
    if (e.key === "Escape" && onback && !e.defaultPrevented && !isTyping(e)) {
      e.preventDefault();
      onback();
    }
  }
</script>

<svelte:window {onkeydown} />

<div class="screen">
  <div class="top">
    <LanguageSwitch />
  </div>
  <div class="center">
    <section class="card" class:wide aria-labelledby={title ? "auth-title" : undefined}>
      {#if onback}
        <button type="button" class="back" onclick={onback}><ChevronLeft size={15} class="rtl-flip" /> {t("common.back")}</button>
      {/if}
      {#if logo}
        <img class="logo" src="/brand/camel-mail-icon.png" alt="" />
      {/if}
      {#if title}
        <h1 id="auth-title" class="title">{title}</h1>
      {/if}
      {#if lead}
        <p class="lead">{lead}</p>
      {/if}
      {@render children()}
    </section>
    {#if footer}
      <div class="footer">{@render footer()}</div>
    {/if}
  </div>
</div>

<style>
  .screen {
    height: 100%;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    background: var(--surface);
  }
  .top {
    display: flex;
    justify-content: flex-end;
    padding: 14px 18px 0;
  }
  .center {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 16px;
    padding: 16px 16px 40px;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 14px;
    width: 100%;
    max-width: 400px;
    padding: 28px;
    background: var(--surface-elevated);
    border: 0.5px solid var(--separator);
    border-radius: var(--radius-xl);
    box-shadow: var(--shadow-dropdown);
  }
  .card.wide {
    max-width: 460px;
  }
  .logo {
    align-self: center;
    width: 56px;
    height: 52px;
    object-fit: contain;
  }
  .title {
    margin: 0;
    font-size: 20px;
    line-height: 28px;
    font-weight: 600;
  }
  .logo ~ .title,
  .logo ~ .lead {
    text-align: center;
  }
  .lead {
    margin: -6px 0 2px;
    font-size: 13px;
    line-height: 20px;
    color: var(--content-subtle);
  }
  .back {
    align-self: flex-start;
    display: inline-flex;
    align-items: center;
    gap: 2px;
    margin: -10px 0 -4px;
    margin-inline-start: -6px;
    padding: 4px 6px;
    border: none;
    border-radius: var(--radius-md);
    background: none;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .back:hover {
    color: var(--content);
    background: var(--surface-hover);
  }
  .back:focus-visible {
    outline: none;
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .footer {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 6px;
    font-size: 12.5px;
    color: var(--content-subtle);
    text-align: center;
  }
  @media (max-width: 480px) {
    .center {
      justify-content: flex-start;
      padding: 8px 12px 24px;
    }
    .card {
      padding: 20px 18px;
      box-shadow: none;
    }
  }
</style>
