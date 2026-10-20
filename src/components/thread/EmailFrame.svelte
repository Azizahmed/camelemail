<script lang="ts">
  import { onDestroy } from "svelte";
  import { applyDirections, arabicFontFaces, fixFinalMarks, useArabicFont } from "../../lib/bidi";
  import { escapeHtml } from "../../lib/format";

  let {
    html,
    text,
    allowRemote = false,
    collapseQuotes = true,
  }: { html: string | null; text: string | null; allowRemote?: boolean; collapseQuotes?: boolean } = $props();

  let frame: HTMLIFrameElement | undefined = $state();
  let height = $state(40);
  let hasQuote = $state(false);
  let showQuote = $state(false);

  const QUOTE_SELECTORS = [".gmail_quote", "blockquote[type=cite]", "#appendonsend", "#divRplyFwdMsg", ".yahoo_quoted", ".moz-cite-prefix"];

  /** Links in a line of plain text (found before escaping; trailing punctuation stays text). */
  function linkify(line: string): string {
    let out = "";
    let last = 0;
    for (const m of line.matchAll(/https?:\/\/[^\s<>"']+/g)) {
      const url = m[0].replace(/[.,;:!?)\]]+$/, "");
      const href = escapeHtml(url);
      out += `${escapeHtml(line.slice(last, m.index))}<a href="${href}">${href}</a>`;
      last = m.index + url.length;
    }
    return out + escapeHtml(line.slice(last));
  }

  /** Plain text as one block per line, so each line can take its own direction. */
  function plainHtml(s: string): string {
    return s
      .replace(/\r?\n$/, "")
      .split(/\r?\n/)
      .map((line) => `<div${/^\s*>/.test(line) ? ' class="q"' : ""}>${linkify(line) || "<br>"}</div>`)
      .join("");
  }

  const doc = $derived.by(() => {
    const remote = allowRemote ? " https: http:" : "";
    const csp = `default-src 'none'; img-src data: blob:${remote}; style-src 'unsafe-inline'${remote}; font-src data:${remote}; media-src data:${remote}`;
    const body = html ?? `<div class="plain">${plainHtml(text ?? "")}</div>`;
    return `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${csp}">
<base target="_blank">
<style>
  ${arabicFontFaces(true)}
  html,body{margin:0;padding:0;background:transparent}
  /* :root outranks the html,body{height:100%!important} many templates ship. */
  :root,:root body{height:auto!important;min-height:0!important}
  body{font:14px/1.45 "Remail Arabic",-apple-system,BlinkMacSystemFont,"Segoe UI","Noto Sans","Noto Sans Arabic",Arial,sans-serif;color:#1c1c1c;overflow-wrap:anywhere;padding:2px}
  img{max-width:100%;height:auto}
  table{max-width:100%}
  pre{white-space:pre-wrap}
  a{color:#4f46e5}
  .plain{white-space:pre-wrap;font-family:inherit}
  .plain .q{color:#777}
  blockquote{margin:0;margin-inline-start:.8ex;border-inline-start:2px solid #ccc;padding-inline-start:1ex;color:#555}
  body.hide-quotes ${QUOTE_SELECTORS.join(",body.hide-quotes ")}{display:none!important}
</style></head><body>${body}</body></html>`;
  });

  /** Documents already prepared (each new srcdoc is a new document). */
  const prepared = new WeakSet<Document>();
  let resized: ResizeObserver | undefined;
  let lastOver = 0;
  let recheck = 0;

  /** Fit the frame to the email's content, growing and shrinking. */
  function measure() {
    const d = frame?.contentDocument;
    if (!frame || !d?.documentElement) return;
    // Hidden (the calendar is showing): keep the last height until it's back.
    if (frame.clientWidth === 0) return;
    // The root's own box, not scrollHeight: that is never less than the frame.
    // Unless the email still pins the root to the frame: then only
    // scrollHeight shows how far its content goes.
    const root = d.documentElement;
    const own = root.scrollHeight > root.clientHeight + 1 ? root.scrollHeight : root.offsetHeight;
    const over = Math.max(24, own) - height;
    if (Math.abs(over) <= 1) {
      lastOver = 0;
      return;
    }
    // Content sized by the frame (100vh boxes) outgrows every new height by
    // the same amount: stop growing instead of running away.
    if (over > 0 && over === lastOver) return;
    lastOver = over;
    height += over;
    cancelAnimationFrame(recheck);
    recheck = requestAnimationFrame(measure);
  }

  onDestroy(() => {
    resized?.disconnect();
    cancelAnimationFrame(recheck);
  });

  function prepare(d: Document) {
    prepared.add(d);
    // Arabic paragraphs read from the right, English ones from the left.
    applyDirections(d.body, "view");
    useArabicFont(d.body);
    // (No script runs in this sandboxed frame, not even listeners added from
    // here: copied text keeps these invisible marks; search ignores them.)
    fixFinalMarks(d.body);
    hasQuote = collapseQuotes && QUOTE_SELECTORS.some((s) => d.querySelector(s));
    d.body.classList.toggle("hide-quotes", hasQuote && !showQuote);
    // Links: <base target="_blank"> makes them new-window requests, which the
    // window opens in the system browser (or compose for mailto:).

    // Follows the height as images without a size arrive, and on resize.
    resized?.disconnect();
    resized = new ResizeObserver(() => measure());
    resized.observe(d.body);
    measure();
  }

  // The frame's load event waits for every image, so lay the email out as
  // soon as its HTML is parsed: full height at once, images fill in after.
  $effect(() => {
    void doc;
    if (!frame) return;
    const stale = frame.contentDocument;
    const until = performance.now() + 10_000;
    let raf = 0;
    const step = () => {
      const d = frame?.contentDocument;
      if (d && d !== stale && d.URL === "about:srcdoc" && d.readyState !== "loading" && d.body) {
        if (!prepared.has(d)) prepare(d);
      } else if (performance.now() < until) {
        raf = requestAnimationFrame(step);
      }
    };
    raf = requestAnimationFrame(step);
    return () => cancelAnimationFrame(raf);
  });

  // Fallback when no frames run (minimised window).
  function loaded() {
    const d = frame?.contentDocument;
    if (d?.body && !prepared.has(d)) prepare(d);
    measure();
  }

  $effect(() => {
    // The body's resize observer refits the frame.
    frame?.contentDocument?.body?.classList.toggle("hide-quotes", hasQuote && !showQuote);
  });
</script>

<div class="paper" class:plain={!html}>
  <iframe
    bind:this={frame}
    title="Email content"
    sandbox="allow-same-origin allow-popups"
    srcdoc={doc}
    style:height="{height}px"
    onload={loaded}
  ></iframe>
</div>
{#if hasQuote}
  <button class="quote-toggle" title={showQuote ? "Hide quoted content" : "Show quoted content"} onclick={() => (showQuote = !showQuote)}>•••</button>
{/if}

<style>
  .paper {
    background: var(--mail-paper);
    border-radius: 10px;
    padding: 12px 14px;
  }
  :global(html[data-theme="light"]) .paper {
    padding: 0;
    background: transparent;
  }
  iframe {
    width: 100%;
    border: none;
    display: block;
    background: transparent;
  }
  .quote-toggle {
    margin-top: 6px;
    border: 0.5px solid var(--separator);
    background: var(--surface-hover);
    color: var(--content-subtle);
    border-radius: 6px;
    padding: 0 8px;
    font-size: 11px;
    line-height: 16px;
    letter-spacing: 1px;
  }
</style>
