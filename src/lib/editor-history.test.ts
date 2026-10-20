// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";
import { EditorHistory } from "./editor-history";

function setup(html: string) {
  const el = document.createElement("div");
  el.innerHTML = html;
  document.body.replaceChildren(el);
  const h = new EditorHistory(el);
  h.reset();
  return { el, h };
}

describe("EditorHistory", () => {
  it("undoes and redoes changes", () => {
    const { el, h } = setup("<div>a</div>");
    el.innerHTML = "<div>ab</div>";
    h.record();
    el.innerHTML = "<h1>ab</h1>";
    h.record();
    expect(h.undo()).toBe(true);
    expect(el.innerHTML).toBe("<div>ab</div>");
    expect(h.undo()).toBe(true);
    expect(el.innerHTML).toBe("<div>a</div>");
    expect(h.undo()).toBe(false);
    expect(h.redo()).toBe(true);
    expect(h.redo()).toBe(true);
    expect(el.innerHTML).toBe("<h1>ab</h1>");
    expect(h.redo()).toBe(false);
  });

  it("joins quick typing into one step, until a word ends", () => {
    vi.useFakeTimers();
    const { el, h } = setup("");
    for (const t of ["H", "He", "Hel"]) {
      el.textContent = t;
      h.record(true);
      vi.advanceTimersByTime(100);
    }
    h.breakStep();
    el.textContent = "Hel w";
    h.record(true);
    h.undo();
    expect(el.textContent).toBe("Hel");
    h.undo();
    expect(el.textContent).toBe("");
    vi.useRealTimers();
  });

  it("a new change drops the redo steps", () => {
    const { el, h } = setup("x");
    el.textContent = "y";
    h.record();
    h.undo();
    el.textContent = "z";
    h.record();
    expect(h.redo()).toBe(false);
    h.undo();
    expect(el.textContent).toBe("x");
  });

  it("puts the caret back inside the restored text", () => {
    const { el, h } = setup("<div>one</div>");
    el.innerHTML = "<div>one two</div>";
    const t = el.firstChild!.firstChild!;
    document.getSelection()!.setBaseAndExtent(t, 7, t, 7);
    h.record();
    el.innerHTML = "<div>gone</div>";
    h.record();
    h.undo();
    const sel = document.getSelection()!;
    expect(el.contains(sel.anchorNode)).toBe(true);
    expect(sel.anchorOffset).toBe(7);
  });
});
