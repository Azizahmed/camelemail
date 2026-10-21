<!-- A QR code drawn as SVG (always dark on white, so phones can read it in dark mode too). -->
<script lang="ts">
  import { encodeQr, qrPath } from "../../lib/qr";

  let { text, label, size = 168 }: { text: string; label: string; size?: number } = $props();

  const qr = $derived.by(() => {
    try {
      return qrPath(encodeQr(text, "M"));
    } catch {
      return null;
    }
  });
</script>

{#if qr}
  <svg class="qr" width={size} height={size} viewBox="0 0 {qr.size} {qr.size}" role="img" aria-label={label} shape-rendering="crispEdges">
    <rect width={qr.size} height={qr.size} fill="#fff" />
    <path d={qr.path} fill="#000" />
  </svg>
{/if}

<style>
  .qr {
    display: block;
    border-radius: var(--radius-lg);
    border: 0.5px solid var(--separator);
  }
</style>
