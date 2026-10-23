// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { contrast, darkBackground, darkBorder, darkenEmail, lightText, oklab, parseColor, type Rgba } from "./darkmail";

const rgb = (hex: string): Rgba => {
  const n = parseInt(hex.slice(1), 16);
  return { r: n >> 16, g: (n >> 8) & 255, b: n & 255, a: 1 };
};
const L = (c: Rgba) => oklab(c)[0];
const hue = (c: Rgba) => {
  const [, a, b] = oklab(c);
  return (Math.atan2(b, a) * 180) / Math.PI;
};
const SURFACE = rgb("#1e1f20");

describe("parseColor", () => {
  it("reads computed colours", () => {
    expect(parseColor("rgb(1, 2, 3)")).toEqual({ r: 1, g: 2, b: 3, a: 1 });
    expect(parseColor("rgba(1, 2, 3, 0.5)")).toEqual({ r: 1, g: 2, b: 3, a: 0.5 });
    expect(parseColor("rgb(1 2 3 / 50%)")).toEqual({ r: 1, g: 2, b: 3, a: 0.5 });
    expect(parseColor("transparent")?.a).toBe(0);
    expect(parseColor("oklch(0.5 0.1 200)")).toBeNull();
  });
});

describe("darkBackground", () => {
  it("turns white into the surface around the email", () => {
    expect(darkBackground(rgb("#ffffff"), SURFACE)).toEqual(SURFACE);
  });

  it("keeps light greys apart, a little lighter than the surface", () => {
    const grey = darkBackground(rgb("#f4f4f4"), SURFACE);
    const darker = darkBackground(rgb("#dddddd"), SURFACE);
    expect(L(grey)).toBeGreaterThan(L(SURFACE));
    expect(L(darker)).toBeGreaterThan(L(grey));
    expect(L(darker)).toBeLessThan(0.35);
  });

  it("keeps the hue of tinted boxes", () => {
    const blue = rgb("#e8f0fe");
    const out = darkBackground(blue, SURFACE);
    expect(L(out)).toBeLessThan(0.3);
    expect(Math.abs(hue(out) - hue(blue))).toBeLessThan(8);
  });

  it("darkens yellow as much as its lightness asks", () => {
    expect(L(darkBackground(rgb("#ffcc00"), SURFACE))).toBeLessThan(0.35);
  });

  it("leaves backgrounds that are already dark", () => {
    const banner = rgb("#222831");
    expect(darkBackground(banner, SURFACE)).toBe(banner);
  });
});

describe("lightText", () => {
  it("turns black text light and readable", () => {
    const out = lightText(rgb("#000000"), SURFACE);
    expect(L(out)).toBeGreaterThan(0.9);
    expect(contrast(out, SURFACE)).toBeGreaterThan(12);
  });

  it("keeps grey text dimmer than black text", () => {
    expect(L(lightText(rgb("#888888"), SURFACE))).toBeLessThan(L(lightText(rgb("#333333"), SURFACE)));
  });

  it("keeps link colours as light versions of themselves", () => {
    const link = rgb("#1a0dab");
    const out = lightText(link, SURFACE);
    expect(contrast(out, SURFACE)).toBeGreaterThanOrEqual(4.5);
    expect(Math.abs(hue(out) - hue(link))).toBeLessThan(10);
  });

  it("leaves text that is already light", () => {
    const white = rgb("#ffffff");
    expect(lightText(white, SURFACE)).toBe(white);
  });

  it("lifts text until it reads on a mid-tone background", () => {
    const bg = darkBackground(rgb("#4caf50"), SURFACE);
    expect(contrast(lightText(rgb("#555555"), bg), bg)).toBeGreaterThanOrEqual(4.5);
  });
});

describe("darkBorder", () => {
  it("keeps light dividers visible but soft", () => {
    const out = darkBorder(rgb("#dddddd"), SURFACE);
    expect(L(out)).toBeGreaterThan(L(SURFACE));
    expect(L(out)).toBeLessThan(0.4);
  });
});

describe("darkenEmail", () => {
  // The test page itself (documents made with createHTMLDocument have no window).
  const frame = (html: string) => {
    document.body.innerHTML = html;
    return document;
  };
  /** What darkenEmail wrote (it writes "important"; the email's own inline styles aren't). */
  const style = (doc: Document, sel: string, prop: string) => {
    const s = (doc.querySelector(sel) as HTMLElement).style;
    return s.getPropertyPriority(prop) === "important" ? s.getPropertyValue(prop) : "";
  };

  it("recolours a light newsletter and leaves its dark banner", () => {
    const doc = frame(`
      <table id="page" style="background-color:#ffffff;color:#222222"><tr><td>
        <p id="text" style="color:#333333">Hello</p>
        <div id="banner" style="background-color:#111111;color:#ffffff"><span id="on-banner" style="color:#eeeeee">Sale</span></div>
        <div id="line" style="border-top:1px solid #dddddd"></div>
      </td></tr></table>`);
    darkenEmail(doc, SURFACE, false);
    expect(parseColor(style(doc, "#page", "background-color"))).toEqual(SURFACE);
    expect(L(parseColor(style(doc, "#text", "color"))!)).toBeGreaterThan(0.85);
    expect(style(doc, "#banner", "background-color")).toBe("");
    expect(style(doc, "#on-banner", "color")).toBe("");
    expect(style(doc, "#line", "border-top-color")).not.toBe("");
  });

  it("keeps hidden preheader text hidden", () => {
    const doc = frame(`<div id="pre" style="color:#ffffff;font-size:1px">Preview</div>`);
    darkenEmail(doc, SURFACE, false);
    expect(parseColor(style(doc, "#pre", "color"))).toEqual(SURFACE);
  });

  it("leaves text over a picture that loads, recolours it when web pictures are blocked", () => {
    const html = `<div id="hero" style="background-image:url(https://example.com/a.png);background-color:#ffffff"><p id="t" style="color:#000000">Hi</p></div>`;
    const shown = frame(html);
    darkenEmail(shown, SURFACE, true);
    expect(style(shown, "#t", "color")).toBe("");
    expect(style(shown, "#hero", "background-color")).toBe("");
    const blocked = frame(html);
    darkenEmail(blocked, SURFACE, false);
    expect(style(blocked, "#t", "color")).not.toBe("");
  });

  it("does nothing to pictures", () => {
    const doc = frame(`<img id="i" src="data:image/png;base64,AAAA" style="background-color:#ffffff">`);
    darkenEmail(doc, SURFACE, false);
    expect(style(doc, "#i", "background-color")).toBe("");
  });
});
