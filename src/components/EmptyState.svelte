<script lang="ts">
  import { Archive, CalendarClock, Clock3, FileText, Inbox, Search, Sparkles, Star, Trash2 } from "@lucide/svelte";
  import { L } from "../lib/types";

  let { label, search = "" }: { label: string; search?: string } = $props();

  // A different line each day keeps an empty folder from feeling the same every time.
  const day = Math.floor(Date.now() / 86_400_000);
  const pick = (xs: string[]) => xs[day % xs.length];

  const state = $derived.by(() => {
    if (search) return { icon: Search, title: `Nothing matches "${search}"`, sub: "Try different words, or look on the server for older mail." };
    switch (label) {
      case L.INBOX:
        return {
          icon: Sparkles,
          title: "Your inbox is clear",
          sub: pick([
            "Nothing is waiting for you. Enjoy it.",
            "Time for a coffee, or a walk.",
            "Every email answered, archived or snoozed.",
            "New mail will show up here as it arrives.",
          ]),
        };
      case L.UNREAD:
        return { icon: Inbox, title: "Nothing unread", sub: "You've opened everything in your inbox." };
      case L.STARRED:
        return { icon: Star, title: "No starred email", sub: pick(["Star an email to find it here quickly.", "Press S on any email to keep it close."]) };
      case L.SNOOZED:
        return { icon: Clock3, title: "Nothing snoozed", sub: "Snoozed email comes back to your inbox when its time comes." };
      case L.DRAFTS:
        return { icon: FileText, title: "No drafts", sub: "Emails you start and don't send are kept here." };
      case L.TRASH:
        return { icon: Trash2, title: "Trash is empty", sub: "Deleted email waits here until you empty it." };
      case L.SCHEDULED:
        return { icon: CalendarClock, title: "Nothing scheduled", sub: "Emails you set to send later wait here." };
      case L.ARCHIVE:
        return { icon: Archive, title: "Archive is empty", sub: "Email you archive is kept here, out of your inbox." };
      default:
        return { icon: Inbox, title: "This folder is empty", sub: pick(["Move email here to see it.", "No email in this folder yet."]) };
    }
  });
</script>

<div class="empty">
  <div class="icon"><state.icon size={28} strokeWidth={1.5} /></div>
  <div class="title">{state.title}</div>
  <div class="sub">{state.sub}</div>
</div>

<style>
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: 18vh 24px 0;
    text-align: center;
    gap: 6px;
  }
  .icon {
    width: 64px;
    height: 64px;
    border-radius: 20px;
    background: var(--accent-soft);
    color: var(--accent);
    display: flex;
    align-items: center;
    justify-content: center;
    margin-bottom: 10px;
  }
  .title {
    font-size: 16px;
    font-weight: 600;
  }
  .sub {
    color: var(--content-subtle);
    font-size: 13px;
  }
</style>
