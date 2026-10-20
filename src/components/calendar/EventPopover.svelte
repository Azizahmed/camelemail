<script lang="ts">
  import { Bell, Clock3, MapPin, Pencil, Repeat, Trash2, Users, Video } from "@lucide/svelte";
  import { api } from "../../lib/api";
  import { calendarOf, fmtRange, type CalEvent } from "../../lib/calendar.svelte";
  import { openLinks, renderMarkdown } from "../../lib/markdown";
  import Popover from "../menus/Popover.svelte";

  let { event, anchor, onclose, onedit, ondelete }: { event: CalEvent; anchor: DOMRect; onclose: () => void; onedit: () => void; ondelete: () => void } =
    $props();

  const cal = $derived(calendarOf(event.calendarId));
  /** An online meeting's link (Teams, Zoom, Meet…), named after its service. */
  const meeting = $derived.by(() => {
    if (!/^https:\/\//i.test(event.url)) return null;
    try {
      const host = new URL(event.url).hostname.replace(/^www\./, "");
      const name = host.endsWith("teams.microsoft.com") || host.endsWith("teams.live.com") ? "Teams" : host.endsWith("zoom.us") ? "Zoom" : host === "meet.google.com" ? "Google Meet" : null;
      return name ? `Join ${name} meeting` : host;
    } catch {
      return null;
    }
  });
  const alarm = (m: number) => (m === 0 ? "At start" : m % 1440 === 0 ? `${m / 1440} day${m === 1440 ? "" : "s"} before` : m % 60 === 0 ? `${m / 60} h before` : `${m} min before`);
</script>

<Popover {anchor} width={330} height={300} align="left" {onclose}>
  <div class="pop">
    <div class="top">
      <span class="swatch" style:background={event.color}></span>
      <div class="title selectable" dir="auto">{event.summary || "(No title)"}</div>
    </div>
    <div class="line"><Clock3 size={13} /> {fmtRange(event)}</div>
    {#if event.recurring}<div class="line"><Repeat size={13} /> Repeats</div>{/if}
    {#if event.location}<div class="line selectable"><MapPin size={13} /> <span dir="auto">{event.location}</span></div>{/if}
    {#if meeting}<div class="line"><Video size={13} /> <button class="join" title={event.url} onclick={() => api.openUrl(event.url)}>{meeting}</button></div>{/if}
    {#if event.alarmsMinutesBefore.length}<div class="line"><Bell size={13} /> {event.alarmsMinutesBefore.map(alarm).join(", ")}</div>{/if}
    {#if event.attendees.length}
      <div class="line"><Users size={13} /> <span>{#each event.attendees as a, i (i)}{i ? ", " : ""}<bdi>{a.name || a.email}</bdi>{/each}</span></div>
    {/if}
    {#if event.description}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <div class="desc selectable" onclick={openLinks}>{@html renderMarkdown(event.description)}</div>
    {/if}
    <div class="foot">
      <span class="cal"><i style:background={cal?.color ?? event.color}></i><bdi>{cal?.name ?? ""}</bdi></span>
      {#if !event.readOnly}
        <button class="icon-btn" title="Delete" onclick={ondelete}><Trash2 size={14} /></button>
        <button class="btn" onclick={onedit}><Pencil size={13} /> Edit</button>
      {/if}
    </div>
  </div>
</Popover>

<style>
  .pop {
    display: flex;
    flex-direction: column;
    gap: 7px;
    padding: 6px 6px 4px;
  }
  .top {
    display: flex;
    gap: 10px;
    align-items: flex-start;
  }
  .swatch {
    width: 4px;
    align-self: stretch;
    border-radius: 2px;
    flex: none;
  }
  .title {
    flex: 1;
    min-width: 0;
    font-size: 15px;
    font-weight: 600;
    line-height: 20px;
  }
  .line {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12.5px;
    color: var(--content-subtle);
  }
  .join {
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--accent);
    cursor: pointer;
  }
  .join:hover {
    text-decoration: underline;
  }
  .desc {
    font-size: 12.5px;
    line-height: 18px;
    max-height: 120px;
    overflow-y: auto;
    border-top: 0.5px solid var(--separator-subtle);
    padding-top: 6px;
    overflow-wrap: anywhere;
  }
  .desc :global(p) {
    margin: 0 0 6px;
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 6px;
    border-top: 0.5px solid var(--separator-subtle);
    padding-top: 7px;
  }
  .cal {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .cal i {
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }
</style>
