<script lang="ts">
  import { onDestroy } from "svelte";
  import { api } from "../../lib/api";
  import { applyDirections, arabicFontFaces, fixFinalMarks, useArabicFont } from "../../lib/bidi";
  import { backgroundBehind, darkenEmail } from "../../lib/darkmail";
  import { escapeHtml } from "../../lib/format";
  import { isMobile } from "../../lib/platform";
  import { theme } from "../../lib/ui.svelte";

  let {
    html,
    text,
    allowRemote = false,
    collapseQuotes = true,
    fitWidth = isMobile,
    dark: forceDark,
  }: {
    html: string | null;
    text: string | null;
    allowRemote?: boolean;
    collapseQuotes?: boolean;
    /** Shrink emails wider than the frame (fixed-width tables, min-widths) to fit it. */
    fitWidth?: boolean;
    /** Recolour for a dark background (or not) whatever the app's theme is: previews of both looks. */
    dark?: boolean;
  } = $props();

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

  /** Dark theme: the email is recoloured to match (src/lib/darkmail.ts). */
  const dark = $derived(forceDark ?? theme.dark);

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
  /* The frame grows to the email's height, so the email never scrolls up and
     down inside it. A vertical scrollbar there would also narrow the text,
     rewrap it to another height and start a resize loop. Wide emails still
     scroll sideways; content that grows with the frame (100vh) scrolls. */
  :root:not(.own-scroll){overflow-y:hidden!important}
  body{font:14px/1.45 "Remail Arabic",-apple-system,BlinkMacSystemFont,"Segoe UI","Noto Sans","Noto Sans Arabic",Arial,sans-serif;color:#1c1c1c;overflow-wrap:anywhere;padding:2px}
  img{max-width:100%;height:auto}
  table{max-width:100%}
  pre{white-space:pre-wrap}
  a{color:#4f46e5}
  .plain{white-space:pre-wrap;font-family:inherit}
  .plain .q{color:#777}
  blockquote{margin:0;margin-inline-start:.8ex;border-inline-start:2px solid #ccc;padding-inline-start:1ex;color:#555}
  body.hide-quotes ${QUOTE_SELECTORS.join(",body.hide-quotes ")}{display:none!important}${
    // Hidden until recoloured, so the white original never flashes up.
    dark ? `\n  :root{color-scheme:dark}\n  :root:not(.remail-dark){visibility:hidden}` : ""
  }
</style></head><body>${body}</body></html>`;
  });

  /** Documents already prepared (each new srcdoc is a new document). */
  const prepared = new WeakSet<Document>();
  let resized: ResizeObserver | undefined;
  let lastOver = 0;
  let recheck = 0;

  /** Behind the frame if nothing on the page has a colour (the reading pane's). */
  const DARK_SURFACE = { r: 30, g: 31, b: 32, a: 1 };

  /** Smallest zoom for wide emails; anything wider still scrolls sideways. */
  const MIN_ZOOM = 0.35;

  /** Zoom a wide email out until it fits the frame's width, as phone mail
   *  apps do: text keeps its layout instead of running off the right edge.
   *  The zoom is worked out from the email's own width at 100% each time, so
   *  calling this again (images arrived, frame resized) settles at once. */
  function fit(d: Document) {
    if (!fitWidth || !d.body) return;
    const root = d.documentElement;
    const width = root.clientWidth;
    if (width === 0) return;
    const before = d.body.style.zoom;
    d.body.style.zoom = "";
    const natural = root.scrollWidth;
    const zoom = natural > width + 1 ? Math.max(MIN_ZOOM, Math.floor((width / natural) * 1000) / 1000) : 1;
    d.body.style.zoom = zoom < 1 ? String(zoom) : "";
    if (d.body.style.zoom !== before) lastOver = 0;
  }

  /** Recent height changes of this document, to notice a layout that never settles. */
  let changes: { t: number; height: number; grew: boolean }[] = [];
  /** Set while a flip-flopping layout is held at its tallest height, for this frame width. */
  let heldWidth = 0;

  /** How often the height may turn around within FLIP_WINDOW ms before it is held. */
  const MAX_FLIPS = 3;
  const FLIP_WINDOW = 1500;

  /** Fit the frame to the email's content, growing and shrinking. */
  function measure() {
    const d = frame?.contentDocument;
    if (!frame || !d?.documentElement) return;
    // Hidden (the calendar is showing): keep the last height until it's back.
    const width = frame.clientWidth;
    if (width === 0) return;
    // A new width lays the email out afresh.
    if (heldWidth && heldWidth !== width) heldWidth = 0;
    // The root's own box, not scrollHeight: that is never less than the frame.
    // Rounded up: offsetHeight rounds a 1020.5px email down, and a frame
    // half a pixel short of its content is what used to make it scroll.
    // Unless the email still pins the root to the frame: then only
    // scrollHeight shows how far its content goes.
    const root = d.documentElement;
    const box = Math.ceil(root.getBoundingClientRect().height);
    const own = root.scrollHeight > root.clientHeight + 1 ? root.scrollHeight : box;
    const over = Math.max(24, own) - height;
    // Never shorter than the content (that cuts off its bottom); a couple of
    // pixels taller is harmless and saves relayouts.
    if (over <= 0 && over >= -2) {
      lastOver = 0;
      return;
    }
    // Held after flip-flopping: only grow (an image arrived), never shrink back.
    if (heldWidth && over < 0) return;
    // Content sized by the frame (100vh boxes) outgrows every new height by
    // the same amount: stop growing instead of running away, and let the
    // rest scroll inside the frame.
    if (over > 0 && over === lastOver) {
      root.classList.add("own-scroll");
      return;
    }
    // Some layouts never settle: each height makes the content want the
    // other one (a line that wraps only at one of them). After a few
    // turnarounds in a short time, keep the taller height for this width.
    const now = performance.now();
    changes = changes.filter((c) => now - c.t < FLIP_WINDOW);
    changes.push({ t: now, height: height + over, grew: over > 0 });
    let flips = 0;
    for (let i = 1; i < changes.length; i++) if (changes[i].grew !== changes[i - 1].grew) flips++;
    if (flips >= MAX_FLIPS) {
      height = Math.max(own, ...changes.map((c) => c.height));
      heldWidth = width;
      changes = [];
      lastOver = 0;
      return;
    }
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
    if (dark) {
      try {
        darkenEmail(d, backgroundBehind(frame?.parentElement ?? null) ?? DARK_SURFACE, allowRemote);
      } catch (e) {
        console.error("dark email colours", e);
      }
      d.documentElement.classList.add("remail-dark");
    }
    changes = [];
    heldWidth = 0;
    lastOver = 0;
    // Arabic paragraphs read from the right, English ones from the left.
    applyDirections(d.body, "view");
    useArabicFont(d.body);
    // (No script runs in this sandboxed frame, not even listeners added from
    // here: copied text keeps these invisible marks; search ignores them.)
    fixFinalMarks(d.body);
    hasQuote = collapseQuotes && QUOTE_SELECTORS.some((s) => d.querySelector(s));
    d.body.classList.toggle("hide-quotes", hasQuote && !showQuote);
    // Links: <base target="_blank"> makes them new-window requests, which the
    // window opens in the system browser (or compose for mailto:). Android's
    // web view has no new windows: it turns them into navigating the whole
    // app, which the sandbox refuses, so nothing happened. There the tap is
    // taken here instead (Chromium runs the page's listeners in this frame).
    if (isMobile) d.addEventListener("click", openLink, true);

    // Follows the height as images without a size arrive, and on resize.
    resized?.disconnect();
    resized = new ResizeObserver(() => {
      fit(d);
      measure();
    });
    resized.observe(d.body);
    fit(d);
    measure();
  }

  /** A tap on a link in the email: web links open in the browser, mailto: in compose. */
  function openLink(e: MouseEvent) {
    const link = (e.target as Element | null)?.closest?.("a[href], area[href]") as HTMLAnchorElement | HTMLAreaElement | null;
    if (!link) return;
    e.preventDefault();
    const href = link.href;
    if (/^(https?|mailto):/i.test(href)) api.openUrl(href).catch((err) => console.error("open link", err));
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

<iframe
  bind:this={frame}
  title="Email content"
  sandbox="allow-same-origin allow-popups"
  srcdoc={doc}
  style:height="{height}px"
  onload={loaded}
></iframe>
{#if hasQuote}
  <button class="quote-toggle" title={showQuote ? "Hide quoted content" : "Show quoted content"} onclick={() => (showQuote = !showQuote)}>•••</button>
{/if}

<style>
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
