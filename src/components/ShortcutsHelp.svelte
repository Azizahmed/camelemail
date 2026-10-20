<script lang="ts">
  import { mail } from "../lib/mail.svelte";
  import { ACTIONS, bindings, FIXED, GROUPS } from "../lib/shortcuts.svelte";
  import KeyCaps from "./KeyCaps.svelte";

  // Your own keys (Settings › Shortcuts) first, then the ones that don't change.
  const groups = $derived(
    GROUPS.map((title) => ({
      title,
      rows: [
        ...ACTIONS.filter((a) => a.group === title && bindings(a.id).length > 0).map((a) => ({ label: a.label, keys: bindings(a.id) })),
        ...FIXED.filter((f) => f.group === title).map((f) => ({ label: f.label, keys: f.keys })),
      ],
    })).filter((g) => g.rows.length > 0),
  );
  const helpKey = $derived(bindings("help")[0]);
</script>

<div class="overlay" role="presentation" onclick={(e) => { if (e.target === e.currentTarget) mail.modal = null; }}>
  <div class="modal help" role="dialog" aria-label="Keyboard shortcuts">
    <div class="modal-header">Keyboard Shortcuts</div>
    <div class="cols">
      {#each groups as g (g.title)}
        <section>
          <h3>{g.title}</h3>
          {#each g.rows as row, r (r)}
            <div class="row">
              <span class="keys">
                {#each row.keys as b, i (b + i)}
                  {#if i > 0}<span class="sep">/</span>{/if}<KeyCaps binding={b} />
                {/each}
              </span>
              <span class="label">{row.label}</span>
            </div>
          {/each}
        </section>
      {/each}
    </div>
    <div class="foot">
      {#if helpKey}Press <KeyCaps binding={helpKey} /> to toggle this help · {/if}Change shortcuts in Settings › Shortcuts
    </div>
  </div>
</div>

<style>
  .help {
    width: min(820px, 92vw);
    max-height: 82vh;
  }
  .cols {
    columns: 2 340px;
    column-gap: 28px;
    padding: 6px 20px 10px;
  }
  section {
    break-inside: avoid;
    margin-bottom: 14px;
  }
  h3 {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.4px;
    color: var(--content-muted);
    margin: 8px 0 6px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 4px 0;
    font-size: 13px;
  }
  .keys {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px;
    flex: none;
    width: 150px;
  }
  .sep {
    font-size: 11px;
    color: var(--content-muted);
  }
  .label {
    color: var(--content-subtle);
  }
  .foot {
    text-align: center;
    font-size: 12px;
    color: var(--content-subtle);
    padding: 6px 0 16px;
  }
</style>
