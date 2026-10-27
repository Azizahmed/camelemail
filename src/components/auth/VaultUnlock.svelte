<!--
  Asks once for the Camel Mail password on a device that can't read the
  end-to-end encrypted mailbox sign-ins yet: one signed in before they were
  encrypted (the password turns that on), or one that lost its vault key.
  Mail goes on meanwhile with the sign-ins this device already has.
-->
<script lang="ts">
  import { api } from "../../lib/api";
  import { autofocus } from "../../lib/actions";
  import { cloud } from "../../lib/cloud.svelte";
  import { errMessage, t } from "../../lib/i18n.svelte";
  import { toast } from "../../lib/ui.svelte";

  /** "Later" hides it until the app starts again. */
  let later = $state(false);
  let password = $state("");
  let busy = $state(false);
  let error = $state("");

  const open = $derived(!later && cloud.status?.phase === "ready" && cloud.status.vault === "locked");
  /** The account has no vault key yet: entering the password makes one. */
  const turnOn = $derived(!cloud.status?.me?.vaultKeyId);

  async function submit(e: Event) {
    e.preventDefault();
    if (busy || !password) return;
    busy = true;
    error = "";
    try {
      await api.cloudUnlock(password);
      password = "";
      toast(t("vault.done"));
    } catch (err) {
      error = errMessage(err);
    } finally {
      busy = false;
    }
  }
</script>

{#if open}
  <div class="overlay vault-overlay">
    <div class="modal vault" role="dialog" aria-modal="true" aria-labelledby="vault-title">
      <form onsubmit={submit}>
        <div class="modal-header" id="vault-title">{turnOn ? t("vault.turnOnTitle") : t("vault.unlockTitle")}</div>
        <div class="modal-body">
          <p class="lead">{turnOn ? t("vault.turnOnLead") : t("vault.unlockLead")}</p>
          <label for="vault-pw">{t("vault.password")}</label>
          <input id="vault-pw" class="input" type="password" dir="ltr" autocomplete="current-password" bind:value={password} use:autofocus />
          {#if error}<div class="au-error" role="alert">{error}</div>{/if}
        </div>
        <div class="modal-actions">
          <button type="button" class="btn" onclick={() => (later = true)}>{t("vault.later")}</button>
          <button type="submit" class="btn primary" disabled={busy || !password}>
            {#if busy}<span class="spinner"></span>{/if}
            {turnOn ? t("vault.turnOn") : t("vault.unlock")}
          </button>
        </div>
      </form>
    </div>
  </div>
{/if}

<style>
  .vault-overlay {
    z-index: 550;
  }
  .vault {
    width: min(92vw, 420px);
  }
  .lead {
    margin: 0 0 14px;
    color: var(--content-muted);
    line-height: 1.5;
  }
  label {
    display: block;
    font-weight: 600;
    margin-bottom: 6px;
  }
  input {
    width: 100%;
    box-sizing: border-box;
  }
  .au-error {
    margin-top: 8px;
  }
</style>
