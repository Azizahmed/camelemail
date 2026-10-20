// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { applyDirections, directHtml, fixFinalMarks, subjectDir, textDirection } from "./bidi";

const html = (s: string) => {
  const el = document.createElement("div");
  el.innerHTML = s;
  return el;
};

describe("textDirection", () => {
  it("follows the first letter", () => {
    expect(textDirection("Hello مرحبا")).toBe("ltr");
    expect(textDirection("مرحبا Hello")).toBe("rtl");
    expect(textDirection("123 — !! שלום")).toBe("rtl");
    expect(textDirection("123 — !!")).toBeNull();
    expect(textDirection("")).toBeNull();
  });
});

describe("subjectDir", () => {
  it("skips reply and forward prefixes", () => {
    expect(subjectDir("Re: تأكيد الموعد")).toBe("rtl");
    expect(subjectDir("RE: Fwd: [2]: hello")).toBe("ltr");
    expect(subjectDir("رد: Meeting")).toBe("ltr");
    expect(subjectDir("Re: ")).toBe("auto");
  });
});

describe("applyDirections", () => {
  it("gives each block the direction of its first letter in received mail", () => {
    const root = html("<p>مرحبا بك</p><p>Hello</p>");
    applyDirections(root, "view", "ltr");
    const [ar, en] = root.querySelectorAll("p");
    expect(ar.getAttribute("dir")).toBe("rtl");
    expect(en.hasAttribute("dir")).toBe(false);
  });

  it("ignores a sender's blanket ltr but keeps their rtl", () => {
    const root = html('<div dir="ltr"><p>مرحبا</p></div><p dir="rtl">Hello</p>');
    applyDirections(root, "view", "ltr");
    expect(root.querySelector("div p")!.getAttribute("dir")).toBe("rtl");
    expect(root.querySelector(":scope > p")!.getAttribute("dir")).toBe("rtl");
  });

  it("flips left alignment and quote bars of right-to-left blocks", () => {
    const root = html('<blockquote style="border-left: 2px solid red; padding-left: 8px">مرحبا</blockquote><p align="left">أهلا</p>');
    applyDirections(root, "view", "ltr");
    const q = root.querySelector("blockquote")!;
    expect(q.style.borderRight).toContain("2px");
    expect(q.style.borderLeft).toBe("");
    expect(q.style.paddingRight).toBe("8px");
    expect(root.querySelector("p")!.getAttribute("align")).toBe("right");
  });

  it("marks blocks whose <br> lines mix directions as plaintext", () => {
    const root = html("<div>Hello<br>مرحبا</div>");
    applyDirections(root, "view", "ltr");
    expect(root.querySelector("div")!.style.unicodeBidi).toBe("plaintext");
  });

  it("follows edits in the composer", () => {
    const root = html('<div dir="rtl">Hello</div>');
    applyDirections(root, "edit", "ltr");
    expect(root.querySelector("div")!.hasAttribute("dir")).toBe(false);
  });
});

describe("directHtml", () => {
  it("leaves left-to-right HTML untouched", () => {
    expect(directHtml("<p>Hello</p>")).toBe("<p>Hello</p>");
    expect(directHtml("")).toBe("");
  });

  it("adds dir to Arabic blocks", () => {
    const out = directHtml("<p>مرحبا</p><p>Hi</p>");
    expect(out).toContain('<p dir="rtl"');
    expect(out).toContain("<p>Hi</p>");
  });

  it("wraps text straight in the body when it is Arabic", () => {
    expect(directHtml("مرحبا<br>بك")).toMatch(/^<div dir="rtl">/);
  });
});

describe("fixFinalMarks", () => {
  it("puts an Arabic letter mark after a word-final mark only", () => {
    const root = html("<p>مرحباً بك، شكراً</p><p>كَتَبَ</p>");
    fixFinalMarks(root);
    const [a, b] = root.querySelectorAll("p");
    expect(a.textContent).toBe("مرحباً؜ بك، شكراً؜");
    // Marks inside a word are followed by a letter; the last one ends the word.
    expect(b.textContent).toBe("كَتَبَ؜");
  });

  it("does nothing without Arabic", () => {
    const root = html("<p>café</p>");
    fixFinalMarks(root);
    expect(root.textContent).toBe("café");
  });
});
