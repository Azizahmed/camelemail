<script lang="ts">
  // An event's details in a bottom sheet (the desktop's popover): time,
  // calendar, place, meeting link, people and notes, with Edit and Delete
  // unless the calendar is read-only.
  import { Bell, Clock3, ExternalLink, MapPin, Pencil, Repeat, Trash2, Users, Video } from "@lucide/svelte";
  import { api } from "../../lib/api";
  import { t } from "../../lib/i18n.svelte";
  import { calendarOf, fmtRange, type CalEvent } from "../../lib/calendar.svelte";
  import { openLinks, renderMarkdown } from "../../lib/markdown";
  import Sheet from "../Sheet.svelte";
  import { alarmLabel, mapsUrl, meetingLabel, partstatLabel } from "./model";

  let {
    event,
    open = $bindable(false),
    onedit,
    ondelete,
  }: {
    event: CalEvent | null;
    open?: boolean;
    onedit: (e: CalEvent) => void;
    ondelete: (e: CalEvent) => void;
  } = $props();

  let sheet: Sheet | undefined = $state();
  const calendar = $derived(event ? calendarOf(event.calendarId) : undefined);
  const meeting = $derived(event ? meetingLabel(event.url) : null);
  /** A video call (Teams, Zoom, Meet) rather than any web link. */
  const isCall = $derived(!!event && /^https:\/\/([^/]*\.)?(teams\.microsoft\.com|teams\.live\.com|zoom\.us|meet\.google\.com)[/:]/i.test(event.url + "/"));

  /** Close the sheet first, so whatever comes next lands after it in the history. */
  async function then(fn: (e: CalEvent) => void) {
    const e = event;
    await sheet?.close();
    if (e) fn(e);
  }
</script>

<Sheet bind:this={sheet} bind:open>
  {#if event}
    <div class="ev">
      <div class="head">
        <span class="swatch" style:background={event.color}></span>
        <h2 class="selectable" dir="auto">{event.summary || "(No title)"}</h2>
      </div>
      <div class="line"><Clock3 size={18} strokeWidth={1.5} /><span>{fmtRange(event)}</span></div>
      {#if event.recurring}<div class="line"><Repeat size={18} strokeWidth={1.5} /><span>{t("m.cal.repeats")}</span></div>{/if}
      {#if calendar}
        <div class="line"><span class="dot-wrap"><i style:background={calendar.color}></i></span><span><bdi>{calendar.name}</bdi>{#if calendar.accountName}<small> · <bdi>{calendar.accountName}</bdi></small>{/if}{#if event.readOnly}<small> · read-only</small>{/if}</span></div>
      {/if}
      {#if event.location}
        <button class="line link" onclick={() => api.openUrl(mapsUrl(event!.location))}>
          <MapPin size={18} strokeWidth={1.5} /><span dir="auto">{event.location}</span>
        </button>
      {/if}
      {#if meeting}
        <button class="line link" title={event.url} onclick={() => api.openUrl(event!.url)}>
          {#if isCall}<Video size={18} strokeWidth={1.5} />{:else}<ExternalLink size={18} strokeWidth={1.5} />{/if}<span>{meeting}</span>
        </button>
      {/if}
      {#if event.alarmsMinutesBefore.length}
        <div class="line"><Bell size={18} strokeWidth={1.5} /><span>{event.alarmsMinutesBefore.map(alarmLabel).join(", ")}</span></div>
      {/if}
      {#if event.attendees.length}
        <div class="line top">
          <Users size={18} strokeWidth={1.5} />
          <ul class="people">
            {#each event.attendees as a, i (i)}
              {@const answer = partstatLabel(a.partstat)}
              <li><bdi>{a.name || a.email}</bdi>{#if a.email && event.organizer && event.organizer.toLowerCase().includes(a.email.toLowerCase())}<small> · organizer</small>{/if}{#if answer}<small> · {answer}</small>{/if}</li>
            {/each}
          </ul>
        </div>
      {/if}
      {#if event.description}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="desc selectable" dir="auto" onclick={openLinks}>{@html renderMarkdown(event.description)}</div>
      {/if}
      {#if !event.readOnly}
        <div class="actions">
          <button class="m-primary soft danger" onclick={() => then(ondelete)}><Trash2 size={16} strokeWidth={1.6} /> {t("m.cal.delete")}</button>
          <button class="m-primary" onclick={() => then(onedit)}><Pencil size={16} strokeWidth={1.6} /> {t("m.cal.edit")}</button>
        </div>
      {/if}
    </div>
  {/if}
</Sheet>

<style>
  .ev {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 5px 7px 3px;
  }
  .head {
    display: flex;
    gap: 11px;
    align-items: stretch;
    padding: 3px 13px 8px;
  }
  .swatch {
    flex: none;
    width: 5px;
    border-radius: 2px;
  }
  h2 {
    flex: 1;
    min-width: 0;
    margin: 0;
    font-size: 18px;
    font-weight: 500;
    line-height: 1.3;
    overflow-wrap: anywhere;
  }
  .line {
    display: flex;
    align-items: center;
    gap: 15px;
    width: 100%;
    min-height: 39px;
    padding: 7px 13px;
    border: 0;
    border-radius: 20px;
    background: transparent;
    color: var(--m-text);
    font: inherit;
    font-size: 14px;
    line-height: 1.35;
    text-align: start;
  }
  .line > :global(svg) {
    flex: none;
    color: var(--m-text-2);
  }
  .line.top {
    align-items: flex-start;
  }
  .line > span {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .line small {
    color: var(--m-text-2);
    font-size: 12px;
  }
  .link {
    color: var(--m-accent);
  }
  .link:active {
    background: var(--m-pill);
  }
  .dot-wrap {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 18px;
  }
  .dot-wrap i {
    width: 11px;
    height: 11px;
    border-radius: 4px;
  }
  .people {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .desc {
    margin: 7px 13px 0;
    padding-top: 10px;
    border-top: 1px solid var(--m-separator);
    font-size: 13px;
    line-height: 1.5;
    color: var(--m-text-read);
    overflow-wrap: anywhere;
    user-select: text;
    -webkit-user-select: text;
  }
  .desc :global(p) {
    margin: 0 0 7px;
  }
  .desc :global(a) {
    color: var(--m-accent);
  }
  .actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    padding: 13px 13px 3px;
  }
  .actions .m-primary {
    flex: 1;
  }
  .danger {
    background: color-mix(in srgb, var(--m-swipe-delete) 13%, transparent);
    color: var(--m-swipe-delete);
  }
</style>
