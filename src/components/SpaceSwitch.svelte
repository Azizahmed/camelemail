<script lang="ts">
  import { CalendarDays, Mail } from "@lucide/svelte";
  import { mail, showCalendar, showMail } from "../lib/mail.svelte";
  import { titled } from "../lib/shortcuts.svelte";
</script>

<div class="switch" role="tablist" aria-label="Mail or calendar">
  <button role="tab" aria-selected={mail.space === "mail"} class:on={mail.space === "mail"} title={titled("Mail", "go.inbox")} onclick={showMail}>
    <Mail size={15} />
  </button>
  <button role="tab" aria-selected={mail.space === "calendar"} class:on={mail.space === "calendar"} title={titled("Calendar", "go.calendar")} onclick={showCalendar}>
    <CalendarDays size={15} />
  </button>
</div>

<style>
  .switch {
    display: inline-flex;
    flex: none;
    padding: 2px;
    gap: 2px;
    border-radius: 9px;
    background: var(--surface-hover);
  }
  button {
    border: none;
    background: none;
    width: 30px;
    height: 26px;
    border-radius: 7px;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--content-subtle);
  }
  button:hover {
    color: var(--content);
  }
  button.on {
    background: var(--surface-overlay);
    color: var(--accent);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.12);
  }
</style>
