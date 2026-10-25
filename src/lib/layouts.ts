// Branded layouts, UI side: picking a logo (shrunk and measured here), working
// out its colours, and the empty form. The markup itself is written by the Rust
// renderer (crates/remail-core/src/layout.rs); nothing here produces HTML.

import type { BrandedLayout, LayoutLogo, LayoutStyle } from "./types";

/** Layouts per user, the size of a decoded logo, and the text limits; the same as the engine's. */
export const MAX_LAYOUTS = 6;
export const MAX_ABOUT = 240;
export const MAX_BUTTON = 30;
export const MAX_LOGO_BYTES = 24 * 1024;
/** The logo is shown at most this big (CSS pixels); the picture holds up to twice the pixels. */
const BOX = { width: 200, height: 80 };

export const DEFAULT_STYLE: LayoutStyle = {
  accent: "#0b5cad",
  headerBg: "#ffffff",
  textColor: "#1f2933",
  mutedColor: "#52606d",
  alignment: "left",
  divider: "line",
  logoPosition: "left",
  footerAlign: "left",
  footerLogo: "beside",
  buttonStyle: "filled",
  tagline: null,
  template: "classic",
};

/** The ready-made templates (names in i18n as `layout.tpl.<id>`); the engine builds each in the logo's colours. */
export const TEMPLATES = ["classic", "band", "tint", "centered", "minimal", "spotlight"] as const;

export function emptyLayout(opts: { accountId?: string | null; senderName?: string; email?: string; isDefault: boolean }): BrandedLayout {
  return {
    id: "",
    accountId: opts.accountId ?? null,
    name: "",
    orgName: "",
    about: "",
    logo: null,
    button: null,
    senderName: opts.senderName ?? "",
    position: "",
    phone: "",
    email: opts.email ?? "",
    style: { ...DEFAULT_STYLE },
    useOnReplies: false,
    isDefault: opts.isDefault,
    updatedAt: 0,
  };
}

/** The size a logo is shown at: its own size, scaled down to fit the box (never up). */
export function fitLogo(width: number, height: number): { width: number; height: number } {
  const w = Math.max(1, width);
  const h = Math.max(1, height);
  const scale = Math.min(BOX.width / w, BOX.height / h, 1);
  return { width: Math.max(1, Math.round(w * scale)), height: Math.max(1, Math.round(h * scale)) };
}

const hex = (n: number) => n.toString(16).padStart(2, "0");

/**
 * The logo's main colours (`#rrggbb`, up to `max`), brand colour first. Near-white
 * and transparent pixels don't count; saturated colours rank above greys of the
 * same area, so a blue mark on a grey plate gives blue.
 */
export function dominantColors(rgba: ArrayLike<number>, max = 3): string[] {
  const buckets = new Map<number, { n: number; r: number; g: number; b: number }>();
  for (let i = 0; i + 3 < rgba.length; i += 4) {
    const [r, g, b, a] = [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]];
    if (a < 128 || (r > 238 && g > 238 && b > 238)) continue;
    const key = ((r >> 4) << 8) | ((g >> 4) << 4) | (b >> 4);
    const e = buckets.get(key) ?? { n: 0, r: 0, g: 0, b: 0 };
    e.n++;
    e.r += r;
    e.g += g;
    e.b += b;
    buckets.set(key, e);
  }
  const ranked = [...buckets.values()]
    .map((e) => {
      const [r, g, b] = [e.r / e.n, e.g / e.n, e.b / e.n];
      const top = Math.max(r, g, b);
      const sat = top === 0 ? 0 : (top - Math.min(r, g, b)) / top;
      return { r, g, b, score: e.n * (1 + 3 * sat) };
    })
    .sort((x, y) => y.score - x.score);
  const out: { r: number; g: number; b: number }[] = [];
  for (const c of ranked) {
    if (out.every((o) => Math.hypot(o.r - c.r, o.g - c.g, o.b - c.b) > 80)) out.push(c);
    if (out.length === max) break;
  }
  return out.map((c) => `#${hex(Math.round(c.r))}${hex(Math.round(c.g))}${hex(Math.round(c.b))}`);
}

/** What the preview shows when remote pictures are off: every `src` dropped, so the alt text stands in. */
export function blockImages(html: string): string {
  return html.replace(/(<img\b[^>]*?)\s+src\s*=\s*(?:"[^"]*"|'[^']*')/gi, "$1");
}

/** `https://…` only, like the renderer. */
export const isPublicUrl = (u: string) => /^https:\/\/[^\s"'<>]+$/i.test(u.trim()) && u.trim().length <= 500;

/** Where the header's button may lead: a public page or a `mailto:` address. */
export const isButtonUrl = (u: string) => isPublicUrl(u) || /^mailto:[^\s"'<>&?#@]+@[^\s"'<>&?#@]+$/i.test(u.trim());

export class LogoError extends Error {
  constructor(readonly reason: "unreadable" | "tooBig") {
    super(reason);
  }
}

function toBlob(canvas: HTMLCanvasElement, type: string, quality?: number): Promise<Blob | null> {
  return new Promise((resolve) => canvas.toBlob(resolve, type, quality));
}

function toBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).split(",", 2)[1] ?? "");
    r.onerror = () => reject(r.error);
    r.readAsDataURL(blob);
  });
}

/**
 * A chosen picture as a logo: measured, shrunk to at most twice its display
 * size, and written as PNG (or JPEG when PNG stays over the limit), so the
 * result is small enough to sync and to send inline.
 */
export async function readLogo(file: Blob, alt = ""): Promise<LayoutLogo> {
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(file);
  } catch {
    throw new LogoError("unreadable");
  }
  try {
    if (!bitmap.width || !bitmap.height) throw new LogoError("unreadable");
    const shown = fitLogo(bitmap.width, bitmap.height);
    const canvas = document.createElement("canvas");
    const ctx = canvas.getContext("2d", { willReadFrequently: true });
    if (!ctx) throw new LogoError("unreadable");

    // Colours from a small copy.
    canvas.width = canvas.height = 48;
    ctx.drawImage(bitmap, 0, 0, 48, 48);
    const colors = dominantColors(ctx.getImageData(0, 0, 48, 48).data);

    const attempts: { type: string; quality?: number; scale: number }[] = [
      ...[2, 1.5, 1].map((scale) => ({ type: "image/png", scale })),
      ...[2, 1.5, 1].flatMap((scale) => [0.9, 0.75, 0.55].map((quality) => ({ type: "image/jpeg", quality, scale }))),
    ];
    for (const a of attempts) {
      canvas.width = Math.max(1, Math.round(shown.width * a.scale));
      canvas.height = Math.max(1, Math.round(shown.height * a.scale));
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      if (a.type === "image/jpeg") {
        ctx.fillStyle = "#ffffff"; // JPEG has no transparency
        ctx.fillRect(0, 0, canvas.width, canvas.height);
      }
      ctx.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
      const blob = await toBlob(canvas, a.type, a.quality);
      if (blob && blob.type === a.type && blob.size <= MAX_LOGO_BYTES) {
        return { data: await toBase64(blob), mime: a.type, width: shown.width, height: shown.height, alt, url: null, colors };
      }
    }
    throw new LogoError("tooBig");
  } finally {
    bitmap.close();
  }
}
