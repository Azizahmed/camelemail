import { describe, expect, it } from "vitest";
import { blockImages, dominantColors, emptyLayout, fitLogo, isButtonUrl, isPublicUrl, TEMPLATES } from "./layouts";

/** Flat RGBA pixels: [r, g, b, a, count] runs. */
const pixels = (...runs: [number, number, number, number, number][]) => Uint8ClampedArray.from(runs.flatMap(([r, g, b, a, n]) => Array.from({ length: n }).flatMap(() => [r, g, b, a])));

describe("fitLogo", () => {
  it("scales down to the 200x80 box, never up", () => {
    expect(fitLogo(400, 100)).toEqual({ width: 200, height: 50 });
    expect(fitLogo(100, 400)).toEqual({ width: 20, height: 80 });
    expect(fitLogo(120, 40)).toEqual({ width: 120, height: 40 });
    expect(fitLogo(0, 0)).toEqual({ width: 1, height: 1 });
  });
});

describe("dominantColors", () => {
  it("skips white and transparent, and ranks saturated above grey", () => {
    const px = pixels([255, 255, 255, 255, 500], [10, 10, 10, 0, 500], [128, 128, 128, 255, 120], [210, 105, 30, 255, 100]);
    expect(dominantColors(px)[0]).toBe("#d2691e");
  });

  it("gives several distinct colours, brand first", () => {
    const px = pixels([200, 30, 30, 255, 300], [30, 30, 200, 255, 200], [205, 35, 35, 255, 100]);
    const colors = dominantColors(px);
    expect(colors).toHaveLength(2);
    expect(colors[0].startsWith("#c")).toBe(true);
  });

  it("keeps a black mark and gives nothing for an empty picture", () => {
    expect(dominantColors(pixels([0, 0, 0, 255, 50]))).toEqual(["#000000"]);
    expect(dominantColors(pixels([255, 255, 255, 255, 50]))).toEqual([]);
  });
});

describe("blockImages", () => {
  it("drops every src so the alt text stands in", () => {
    const html = `<p>x</p><img src="data:image/png;base64,AAA" width="10" alt="Acme logo"><img alt='b' src='https://x.test/a.png'>`;
    const out = blockImages(html);
    expect(out).not.toMatch(/src=/);
    expect(out).toContain('alt="Acme logo"');
    expect(out).toContain('width="10"');
  });
});

describe("isPublicUrl", () => {
  it("accepts https only", () => {
    expect(isPublicUrl("https://acme.test/logo.png")).toBe(true);
    expect(isPublicUrl("http://acme.test/logo.png")).toBe(false);
    expect(isPublicUrl("javascript:alert(1)")).toBe(false);
    expect(isPublicUrl("https://a.test/x y")).toBe(false);
  });
});

describe("isButtonUrl", () => {
  it("takes a public page or a mailto: address, nothing else", () => {
    expect(isButtonUrl("https://acme.test/shop")).toBe(true);
    expect(isButtonUrl("mailto:hello@acme.test")).toBe(true);
    expect(isButtonUrl("http://acme.test")).toBe(false);
    expect(isButtonUrl("javascript:alert(1)")).toBe(false);
    expect(isButtonUrl("mailto:not an address")).toBe(false);
    expect(isButtonUrl("tel:+15550100")).toBe(false);
  });
});

describe("TEMPLATES", () => {
  it("matches the engine's six ready-made templates", () => {
    expect([...TEMPLATES]).toEqual(["classic", "band", "tint", "centered", "minimal", "spotlight"]);
  });
});

describe("emptyLayout", () => {
  it("starts with the sender's details and its own copy of the style", () => {
    const a = emptyLayout({ senderName: "Dana", email: "d@x.test", isDefault: true });
    const b = emptyLayout({ isDefault: false });
    a.style.accent = "#000000";
    expect(b.style.accent).toBe("#0b5cad");
    expect([a.senderName, a.email, a.isDefault]).toEqual(["Dana", "d@x.test", true]);
    expect([a.about, a.button]).toEqual(["", null]);
  });
});
