<script lang="ts">
  import { Lock, Plus, RotateCcw, X } from "@lucide/svelte";
  import {
    ACTIONS, FIXED, GROUPS, bindings, calendarClash, eventKey, formatBinding, isDefault, normalize, removeBinding, reservedBy,
    resetAll, resetBinding, setBinding, setBindings,
  } from "../../lib/shortcuts.svelte";
  import { confirmDialog, toast } from "../../lib/ui.svelte";
  import KeyCaps from "../KeyCaps.svelte";

  /** How long to wait for the second key of a two-key shortcut. */
  const SEQUENCE_WAIT = 1000;

  /** The shortcut being recorded: which action, which of its keys it replaces (null adds one), the keys so far. */
  let rec = $state<{ id: string; replace: string | null; keys: string[] } | null>(null);
  let recTimer: ReturnType<typeof setTimeout> | undefined;
  /** A line under a row about the last change (keys taken from another action, a key that can't be used). */
  let note = $state<{ id: string; text: string; error?: boolean } | null>(null);

  const rows = $derived(
    GROUPS.map((g) => ({ group: g, actions: ACTIONS.filter((a) => a.group === g), fixed: FIXED.filter((f) => f.group === g) })),
  );

  function start(id: string, replace: string | null) {
    stop();
    note = null;
    // Keys pressed now must not click the focused button.
    (document.activeElement as HTMLElement | null)?.blur?.();
    rec = { id, replace, keys: [] };
  }

  function stop() {
    clearTimeout(recTimer);
    rec = null;
  }

  function onRecordKey(e: KeyboardEvent) {
    if (!rec) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    if (e.key === "Escape") return stop();
    if (e.repeat) return;
    const k = eventKey(e);
    if (!k) return; // a modifier on its own: wait for the key it goes with
    rec.keys.push(k);
    clearTimeout(recTimer);
    if (rec.keys.length >= 2) finish();
    else recTimer = setTimeout(finish, SEQUENCE_WAIT);
  }

  $effect(() => {
    if (!rec) return;
    const swallow = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopImmediatePropagation();
    };
    const away = (e: PointerEvent) => {
      if (!(e.target as Element | null)?.closest?.(".chip.recording")) stop();
    };
    window.addEventListener("keydown", onRecordKey, true);
    window.addEventListener("keyup", swallow, true);
    window.addEventListener("pointerdown", away, true);
    window.addEventListener("blur", stop);
    return () => {
      window.removeEventListener("keydown", onRecordKey, true);
      window.removeEventListener("keyup", swallow, true);
      window.removeEventListener("pointerdown", away, true);
      window.removeEventListener("blur", stop);
    };
  });

  const labelOf = (id: string) => ACTIONS.find((a) => a.id === id)?.label ?? id;

  /** Every action's keys, to put back with Undo. */
  const snapshot = () => Object.fromEntries(ACTIONS.map((a) => [a.id, [...bindings(a.id)]]));

  /** Tell what a change took from other actions, with Undo. */
  function report(id: string, b: string, taken: { id: string; binding: string }[], before: Record<string, string[]>) {
    const lines: string[] = [];
    if (taken.length > 0) {
      const from = [...new Set(taken.map((t) => `"${labelOf(t.id)}"`))].join(", ");
      lines.push(`${formatBinding(b)} was removed from ${from}.`);
      toast(`${formatBinding(b)} was removed from ${from}`, {
        action: {
          label: "Undo",
          run: () => {
            for (const x of [id, ...taken.map((t) => t.id)]) setBindings(x, before[x]);
            note = null;
          },
        },
      });
    }
    const clash = calendarClash(id, b);
    if (clash) lines.push(`In the calendar, ${formatBinding(b)} now does this instead of "${clash}".`);
    note = lines.length > 0 ? { id, text: lines.join(" ") } : null;
  }

  function finish() {
    clearTimeout(recTimer);
    if (!rec) return;
    const { id, replace, keys } = rec;
    rec = null;
    if (keys.length === 0) return;
    const b = normalize(keys.join(" "));
    if (b === replace) return;
    const reserved = reservedBy(b);
    if (reserved) {
      note = { id, text: `${formatBinding(b).split(" then ")[0]} is kept for "${reserved}" and can't be used here.`, error: true };
      return;
    }
    const before = snapshot();
    report(id, b, setBinding(id, b, replace), before);
  }

  function reset(id: string) {
    stop();
    const before = snapshot();
    const taken = resetBinding(id);
    note = null;
    if (taken.length > 0) report(id, taken[0].binding, taken, before);
  }

  async function confirmResetAll() {
    stop();
    const r = await confirmDialog({
      title: "Reset all shortcuts?",
      message: "Every shortcut goes back to the keys Remail came with.",
      confirmLabel: "Reset all",
      danger: true,
    });
    if (r !== "confirm") return;
    resetAll();
    note = null;
    toast("Shortcuts reset to defaults");
  }
</script>

<h1>Shortcuts</h1>
<p class="intro">
  Click a shortcut and press the new keys. For a two-key shortcut like <KeyCaps binding="g i" />, press the keys one after the other.
  <kbd>Esc</kbd> cancels.
</p>

{#each rows as { group, actions, fixed } (group)}
  <div class="group-title">{group}</div>
  <div class="list">
    {#each actions as a (a.id)}
      {@const keys = bindings(a.id)}
      <div class="sc">
        <div class="sc-row">
          <span class="sc-label" dir="auto">{a.label}</span>
          <div class="sc-keys">
            {#each keys as b (b)}
              {#if rec?.id === a.id && rec.replace === b}
                {@render recording()}
              {:else}
                <span class="chip-wrap">
                  <button class="chip" title="Change {formatBinding(b)}" onclick={() => start(a.id, b)}><KeyCaps binding={b} /></button>
                  <button class="chip-x" title="Remove {formatBinding(b)}" aria-label="Remove {formatBinding(b)}" onclick={() => { stop(); note = null; removeBinding(a.id, b); }}>
                    <X size={11} />
                  </button>
                </span>
              {/if}
            {/each}
            {#if rec?.id === a.id && rec.replace === null}
              {@render recording()}
            {:else if keys.length === 0}
              <button class="chip none" onclick={() => start(a.id, null)}>Not set</button>
            {:else if keys.length < 3}
              <button class="icon-btn mini" title="Add another shortcut" aria-label="Add another shortcut for {a.label}" onclick={() => start(a.id, null)}><Plus size={14} /></button>
            {/if}
            <button class="icon-btn mini" class:hidden={isDefault(a.id)} title="Reset to default" aria-label="Reset {a.label} to default" onclick={() => reset(a.id)}>
              <RotateCcw size={13} />
            </button>
          </div>
        </div>
        {#if note?.id === a.id}
          <div class="sc-note" class:error={note.error} role="status">{note.text}</div>
        {/if}
      </div>
    {/each}
    {#each fixed as f (f.label)}
      <div class="sc fixed">
        <div class="sc-row">
          <span class="sc-label" dir="auto">{f.label}</span>
          <div class="sc-keys" title="Can't be changed">
            {#each f.keys as b, i (b)}
              {#if i > 0}<span class="or">/</span>{/if}<KeyCaps binding={b} />
            {/each}
            <span class="lock"><Lock size={12} /></span>
          </div>
        </div>
      </div>
    {/each}
  </div>
{/each}

<button class="btn add-btn" onclick={confirmResetAll}><RotateCcw size={14} /> Reset all shortcuts</button>

{#snippet recording()}
  <span class="chip recording" role="status" aria-live="polite">
    {#if rec && rec.keys.length > 0}
      <KeyCaps binding={rec.keys.join(" ")} />
    {:else}
      Press keys…
    {/if}
  </span>
{/snippet}

<style>
  h1 {
    margin: 0 0 18px;
    font-size: 20px;
    line-height: 26px;
    font-weight: 600;
  }
  .intro {
    margin: -6px 0 12px;
    color: var(--content-subtle);
    font-size: 13px;
    line-height: 22px;
  }
  .group-title {
    margin: 24px 0 8px;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--content-muted);
  }
  .list {
    border: 0.5px solid var(--separator);
    border-radius: var(--radius-xl);
    background: var(--surface-elevated);
    overflow: hidden;
    margin-bottom: 10px;
  }
  .list > :global(* + *) {
    border-top: 0.5px solid var(--separator-subtle);
  }
  .sc {
    padding: 7px 10px 7px 14px;
  }
  .sc-row {
    display: flex;
    align-items: center;
    gap: 16px;
    min-height: 28px;
  }
  .sc-label {
    flex: 1;
    min-width: 0;
    font-size: 13px;
  }
  .fixed .sc-label {
    color: var(--content-subtle);
  }
  .sc-keys {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    align-items: center;
    gap: 4px;
  }
  .chip-wrap {
    position: relative;
    display: inline-flex;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    min-height: 26px;
    padding: 2px 6px;
    border: 1px solid transparent;
    border-radius: var(--radius-lg);
    background: none;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .chip:hover {
    background: var(--surface-hover);
    border-color: var(--separator);
  }
  .chip:focus-visible,
  .chip-x:focus-visible {
    outline: none;
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .chip.none {
    font-style: italic;
    color: var(--content-muted);
  }
  .chip.recording {
    min-width: 86px;
    justify-content: center;
    border: 1px dashed var(--accent);
    background: var(--accent-soft);
    color: var(--accent);
  }
  .chip-x {
    position: absolute;
    top: -5px;
    right: -5px;
    width: 16px;
    height: 16px;
    padding: 0;
    display: none;
    align-items: center;
    justify-content: center;
    border: 0.5px solid var(--separator);
    border-radius: 50%;
    background: var(--surface-overlay);
    color: var(--content-subtle);
  }
  .chip-x:hover {
    color: var(--indicator-error);
  }
  .chip-wrap:hover .chip-x,
  .chip-x:focus-visible {
    display: inline-flex;
  }
  .mini {
    width: 26px;
    height: 26px;
  }
  .mini.hidden {
    visibility: hidden;
  }
  .or {
    font-size: 11px;
    color: var(--content-muted);
  }
  .lock {
    display: inline-flex;
    width: 26px;
    justify-content: center;
    color: var(--content-muted);
  }
  .sc-note {
    margin: 4px 0 2px;
    font-size: 12px;
    line-height: 16px;
    color: var(--content-subtle);
  }
  .sc-note.error {
    color: var(--indicator-error);
  }
  .add-btn {
    margin-top: 4px;
  }
</style>
