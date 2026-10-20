// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { finalizeHtml, inlineImageBytes, slashAtCaret, tableHtml } from "./editor";

function caretIn(editor: HTMLElement, text: Text, offset: number) {
  document.body.replaceChildren(editor);
  const sel = document.getSelection()!;
  const r = document.createRange();
  r.setStart(text, offset);
  r.collapse(true);
  sel.removeAllRanges();
  sel.addRange(r);
}

describe("slashAtCaret", () => {
  const typed = (s: string) => {
    const editor = document.createElement("div");
    const t = document.createTextNode(s);
    editor.append(t);
    caretIn(editor, t, s.length);
    return slashAtCaret(editor);
  };

  it("finds a command at the start of a line or after a space", () => {
    expect(typed("/")?.query).toBe("");
    expect(typed("/head")?.query).toBe("head");
    expect(typed("Hello /tab")?.query).toBe("tab");
    expect(typed("Hello /to")?.query).toBe("to");
  });

  it("ignores slashes inside words, paths and finished commands", () => {
    expect(typed("and/or")).toBeNull();
    expect(typed("https://x")).toBeNull();
    expect(typed("/table done")).toBeNull();
    expect(typed("/a/b")).toBeNull();
  });

  it("covers the slash and the query", () => {
    const m = typed("Hi /li")!;
    expect(m.range.toString()).toBe("/li");
  });
});

describe("finalizeHtml", () => {
  it("turns to-do items into boxes every client shows", () => {
    const out = finalizeHtml('<ul class="todo"><li data-checked="true">Done</li><li data-checked="false">Open</li></ul>');
    expect(out).toContain("☑ Done");
    expect(out).toContain("☐ Open");
    expect(out).not.toContain("todo");
    expect(out).not.toContain("data-checked");
    expect(out).toContain("list-style: none");
  });

  it("leaves other HTML alone", () => {
    expect(finalizeHtml("<ul><li>a</li></ul>")).toBe("<ul><li>a</li></ul>");
  });
});

describe("helpers", () => {
  it("builds a table of the asked size", () => {
    const doc = new DOMParser().parseFromString(tableHtml(2, 4), "text/html");
    expect(doc.querySelectorAll("tr").length).toBe(2);
    expect(doc.querySelectorAll("td, th").length).toBe(8);
    expect(doc.querySelectorAll("th").length).toBe(4);
  });

  it("counts embedded picture bytes", () => {
    expect(inlineImageBytes('<img src="data:image/png;base64,AAAA"><img src="https://x/y.png">')).toBe(3);
  });
});

describe("tables", () => {
  it("puts the caret in the first cell, tabs between cells and grows at the end", async () => {
    const { enterNewTable, tabInTable } = await import("./editor");
    const editor = document.createElement("div");
    editor.innerHTML = tableHtml(1, 2);
    document.body.replaceChildren(editor);
    enterNewTable(editor);
    const cell = () => (document.getSelection()!.anchorNode as Element).closest("td, th");
    const tds = () => [...editor.querySelectorAll("td, th")];
    expect(cell()).toBe(tds()[0]);
    expect(tabInTable(editor, false)).toBe(true);
    expect(cell()).toBe(tds()[1]);
    expect(tabInTable(editor, false)).toBe(true);
    expect(editor.querySelectorAll("tr").length).toBe(2);
    expect(cell()).toBe(tds()[2]);
    tabInTable(editor, true);
    expect(cell()).toBe(tds()[1]);
    expect(editor.querySelector("[data-new]")).toBeNull();
  });
});
