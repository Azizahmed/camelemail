// Dark theme for received mail. Emails are written for a white page, so in
// the dark theme they are recoloured element by element: light backgrounds
// turn dark (white becomes the reading pane's own colour), dark text on them
// turns light, and light borders soften. Hues stay; only lightness changes
// (in OKLab, so yellow and blue end up as light or dark as grey does). Colours
// that already suit a dark page (dark banners, white text on a button) and
// pictures are left alone.

export interface Rgba {
  /** 0–255 */
  r: number;
  g: number;
  b: number;
  /** 0–1 */
  a: number;
}

/** Computed colours: `rgb(…)`/`rgba(…)` with commas or spaces. Others (oklch, color()) → null. */
export function parseColor(s: string): Rgba | null {
  if (s === "transparent") return { r: 0, g: 0, b: 0, a: 0 };
  const m = /^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)(?:\s*[,/]\s*([\d.]+)(%?))?\s*\)$/.exec(s.trim());
  if (!m) return null;
  const a = m[4] === undefined ? 1 : Number(m[4]) / (m[5] ? 100 : 1);
  return { r: Number(m[1]), g: Number(m[2]), b: Number(m[3]), a };
}

export function formatColor(c: Rgba): string {
  return c.a >= 1 ? `rgb(${c.r}, ${c.g}, ${c.b})` : `rgba(${c.r}, ${c.g}, ${c.b}, ${Math.round(c.a * 1000) / 1000})`;
}

const toLinear = (c: number) => {
  c /= 255;
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
};
const fromLinear = (c: number) => 255 * (c <= 0.0031308 ? 12.92 * c : 1.055 * c ** (1 / 2.4) - 0.055);

/** OKLab [L, a, b] of an sRGB colour (alpha ignored). */
export function oklab(c: Rgba): [number, number, number] {
  const r = toLinear(c.r);
  const g = toLinear(c.g);
  const b = toLinear(c.b);
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

/** Linear sRGB of an OKLab colour (may fall outside 0–1). */
function linearFromOklab(L: number, A: number, B: number): [number, number, number] {
  const l = (L + 0.3963377774 * A + 0.2158037573 * B) ** 3;
  const m = (L - 0.1055613458 * A - 0.0638541728 * B) ** 3;
  const s = (L - 0.0894841775 * A - 1.291485548 * B) ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
}

const inGamut = (lin: number[]) => lin.every((v) => v >= -1e-4 && v <= 1 + 1e-4);

/** The same hue at OKLab lightness `L`, with less chroma where sRGB can't hold it. */
export function withLightness(c: Rgba, L: number): Rgba {
  const [, A, B] = oklab(c);
  let k = 1;
  if (!inGamut(linearFromOklab(L, A, B))) {
    let lo = 0;
    let hi = 1;
    for (let i = 0; i < 12; i++) {
      const mid = (lo + hi) / 2;
      if (inGamut(linearFromOklab(L, A * mid, B * mid))) lo = mid;
      else hi = mid;
    }
    k = lo;
  }
  const lin = linearFromOklab(L, A * k, B * k);
  const ch = (v: number) => Math.max(0, Math.min(255, Math.round(fromLinear(Math.max(0, Math.min(1, v))))));
  return { r: ch(lin[0]), g: ch(lin[1]), b: ch(lin[2]), a: c.a };
}

/** WCAG relative luminance. */
function luminance(c: Rgba): number {
  return 0.2126 * toLinear(c.r) + 0.7152 * toLinear(c.g) + 0.0722 * toLinear(c.b);
}

/** WCAG contrast ratio of two opaque colours (1–21). */
export function contrast(x: Rgba, y: Rgba): number {
  const a = luminance(x);
  const b = luminance(y);
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

/** `top` painted over an opaque `under`. */
function over(top: Rgba, under: Rgba): Rgba {
  const a = top.a;
  const mix = (t: number, u: number) => Math.round(t * a + u * (1 - a));
  return { r: mix(top.r, under.r), g: mix(top.g, under.g), b: mix(top.b, under.b), a: 1 };
}

/** OKLab lightness at or below which a background already reads as dark. */
const BG_DARK = 0.5;
/** At or above this, text already reads as light. */
const FG_LIGHT = 0.75;
/** Black text becomes this light. */
const FG_TOP = 0.93;
/** Text below this contrast with its background was hidden on purpose (preheaders). */
const HIDDEN = 1.25;
/** Text gets at least this contrast with its new background. */
const READABLE = 4.5;

/** A background for the dark page: light ones turn dark, white becomes `surface`. */
export function darkBackground(c: Rgba, surface: Rgba): Rgba {
  const [L, A, B] = oklab(c);
  if (L <= BG_DARK) return c;
  if (L > 0.985 && Math.hypot(A, B) < 0.02) return { ...surface, a: c.a };
  const floor = Math.min(oklab(surface)[0], 0.35);
  return withLightness(c, floor + ((1 - L) / (1 - BG_DARK)) * (BG_DARK - floor));
}

/** Text that sat on a light background, for its new dark one `bg` (opaque). */
export function lightText(c: Rgba, bg: Rgba): Rgba {
  const L = oklab(c)[0];
  let out = L >= FG_LIGHT ? c : withLightness(c, FG_TOP - (L / FG_LIGHT) * (FG_TOP - FG_LIGHT));
  for (let l = oklab(out)[0]; contrast(over(out, bg), bg) < READABLE && l < 0.98; ) {
    l = Math.min(0.98, l + 0.03);
    out = withLightness(c, l);
  }
  return out;
}

/** A border that sat on a light background: still a line, softer than the text. */
export function darkBorder(c: Rgba, surface: Rgba): Rgba {
  const L = oklab(c)[0];
  return withLightness(c, Math.min(oklab(surface)[0], 0.35) + 0.04 + (1 - L) * 0.4);
}

const SKIP = new Set(["IMG", "PICTURE", "SOURCE", "VIDEO", "AUDIO", "CANVAS", "IFRAME", "OBJECT", "EMBED", "BR", "WBR", "STYLE", "SCRIPT", "TEMPLATE"]);
const SIDES = ["top", "right", "bottom", "left"] as const;

interface Ctx {
  /** The background showing behind this element as the email drew it. */
  was: Rgba;
  /** …and after recolouring. */
  now: Rgba;
  /** Colours here belong to a picture behind them: leave the whole branch. */
  keep: boolean;
}

/**
 * Recolour a parsed email for the dark theme, in place. `surface` is the
 * colour around the frame (the page's white turns into it). `remoteImages`:
 * whether web pictures load, i.e. whether background images from the web
 * will actually be behind the text drawn over them.
 */
export function darkenEmail(doc: Document, surface: Rgba, remoteImages: boolean) {
  const view = doc.defaultView;
  const body = doc.body;
  if (!view || !body) return;
  const white: Rgba = { r: 255, g: 255, b: 255, a: 1 };
  const base = { ...surface, a: 1 };
  const ctxs = new Map<Element, Ctx>();
  const writes: [HTMLElement, string, string][] = [];

  const picture = (bgImage: string) => {
    if (bgImage === "none" || !bgImage) return false;
    if (/gradient\(/.test(bgImage)) return true;
    for (const m of bgImage.matchAll(/url\(\s*["']?([^"')]*)/g)) {
      if (/^(data|blob):/i.test(m[1]) || (remoteImages && /^https?:/i.test(m[1]))) return true;
    }
    return false;
  };

  const visit = (el: HTMLElement, parent: Ctx) => {
    if (parent.keep) {
      ctxs.set(el, parent);
      return;
    }
    const cs = view.getComputedStyle(el);
    const ctx: Ctx = { was: parent.was, now: parent.now, keep: false };
    if (picture(cs.backgroundImage)) {
      ctx.keep = true;
      ctxs.set(el, ctx);
      return;
    }
    const bg = parseColor(cs.backgroundColor);
    if (bg && bg.a > 0.02) {
      const dark = darkBackground(bg, base);
      ctx.was = over(bg, parent.was);
      ctx.now = over(dark, parent.now);
      if (dark !== bg) writes.push([el, "background-color", formatColor(dark)]);
    }
    ctxs.set(el, ctx);
    // Only what sat on a light background changes; dark banners keep their colours.
    if (oklab(ctx.was)[0] <= BG_DARK) return;

    const fg = parseColor(cs.color);
    if (fg && fg.a > 0) {
      const hidden = contrast(over(fg, ctx.was), ctx.was) < HIDDEN;
      const text = hidden ? { ...ctx.now } : lightText(fg, ctx.now);
      if (text !== fg) writes.push([el, "color", formatColor(text)]);
    }
    for (const side of SIDES) {
      if (cs.getPropertyValue(`border-${side}-style`) === "none" || cs.getPropertyValue(`border-${side}-width`) === "0px") continue;
      const c = parseColor(cs.getPropertyValue(`border-${side}-color`));
      if (c && c.a > 0) writes.push([el, `border-${side}-color`, formatColor(darkBorder(c, base))]);
    }
  };

  // The email was drawn on white; its page now shows the surface.
  const root: Ctx = { was: white, now: base, keep: false };
  visit(doc.documentElement, root);
  const walker = doc.createTreeWalker(body, NodeFilter.SHOW_ELEMENT, {
    acceptNode: (n) => (SKIP.has((n as Element).tagName) ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
  });
  for (let n: Node | null = body; n; n = walker.nextNode()) {
    const el = n as HTMLElement;
    if (!el.style) continue;
    visit(el, ctxs.get(el.parentElement!) ?? root);
  }
  // Read everything first, then write (one style recalculation). Inline
  // "important" outranks the email's own !important rules.
  for (const [el, prop, value] of writes) el.style.setProperty(prop, value, "important");
}

/** The opaque colour behind an element of the app's own page. */
export function backgroundBehind(el: Element | null): Rgba | null {
  for (let n = el; n; n = n.parentElement) {
    const c = parseColor(getComputedStyle(n).backgroundColor);
    if (c && c.a > 0.5) return { ...c, a: 1 };
  }
  return null;
}
