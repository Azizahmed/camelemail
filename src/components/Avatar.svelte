<script lang="ts">
  import { avatarColor, initials } from "../lib/format";

  let { name, email = "", size = 22, ring = null }: { name: string; email?: string; size?: number; ring?: string | null } = $props();
</script>

<span
  class="avatar"
  style:width="{size}px"
  style:height="{size}px"
  style:font-size="{Math.round(size * 0.45)}px"
  style:background={avatarColor(email || name)}
  style:--ring={ring}
  class:ring
  aria-hidden="true"
>
  {initials(name || email)}
</span>

<style>
  .avatar {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: 50%;
    color: #fff;
    font-weight: 500;
    flex: none;
    letter-spacing: 0.2px;
  }
  /* Account colour. Whole pixels and an outline (not a fractional box-shadow)
     keep the ring evenly spaced on every side, and the gap shows the row's
     own background. */
  .ring {
    outline: 2px solid var(--ring);
    outline-offset: 1px;
  }
</style>
