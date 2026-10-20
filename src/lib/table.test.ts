// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { HEADER_BG, newTableHtml, normalizeTable, partState, stripTableMarkers, tableAction } from "./table";

function make(rows = 3, cols = 3) {
  const root = document.createElement("div");
  root.innerHTML = newTableHtml(rows, cols);
  document.body.replaceChildren(root);
  const t = root.querySelector("table")!;
  normalizeTable(t);
  return { root, t };
}

const texts = (t: HTMLTableElement) => [...t.rows].map((r) => [...r.cells].map((c) => c.textContent).join(","));

function fill(t: HTMLTableElement) {
  [...t.rows].forEach((r, i) => [...r.cells].forEach((c, j) => (c.textContent = `${i}${j}`)));
  normalizeTable(t);
}

describe("tables", () => {
  it("starts with a tinted header row and inner lines only", () => {
    const { t } = make();
    expect(t.rows[0].cells[0].tagName).toBe("TH");
    expect(t.rows[0].cells[0].style.backgroundColor).not.toBe("");
    expect(t.rows[1].cells[0].tagName).toBe("TD");
    expect(t.rows[0].cells[0].style.borderTop).toBe("");
    expect(t.rows[1].cells[0].style.borderTop).not.toBe("");
    expect(t.rows[1].cells[1].style.borderLeft).not.toBe("");
    expect(t.getAttribute("dir")).toBe("ltr");
  });

  it("turns right-to-left with Arabic text, lines on the right side", () => {
    const { t } = make();
    t.rows[1].cells[0].textContent = "مرحبا";
    normalizeTable(t);
    expect(t.getAttribute("dir")).toBe("rtl");
    expect(t.rows[1].cells[1].style.borderRight).not.toBe("");
    expect(t.rows[1].cells[1].style.borderLeft).toBe("");
    expect(t.rows[1].cells[0].style.textAlign).toBe("right");
  });

  it("inserts, duplicates, moves and deletes rows", () => {
    const { t } = make();
    fill(t);
    tableAction(t, "row", 1, { type: "insertAfter" });
    expect(t.rows.length).toBe(4);
    expect(t.rows[2].textContent).toBe("");
    tableAction(t, "row", 1, { type: "duplicate" });
    expect(texts(t)[2]).toBe("10,11,12");
    tableAction(t, "row", 1, { type: "moveAfter" });
    tableAction(t, "row", 0, { type: "delete" });
    expect(t.rows[0].cells[0].tagName).toBe("TH");
    expect(t.rows.length).toBe(4);
  });

  it("works on columns", () => {
    const { t } = make(2, 2);
    fill(t);
    tableAction(t, "col", 0, { type: "insertBefore" });
    expect(texts(t)).toEqual([",00,01", ",10,11"]);
    tableAction(t, "col", 1, { type: "moveAfter" });
    expect(texts(t)).toEqual([",01,00", ",11,10"]);
    tableAction(t, "col", 0, { type: "delete" });
    expect(texts(t)).toEqual(["01,00", "11,10"]);
  });

  it("colours, aligns and styles a row, and clears it", () => {
    const { t } = make();
    tableAction(t, "row", 1, { type: "background", color: "#fbf3db" });
    tableAction(t, "row", 1, { type: "color", color: "#d44c47" });
    tableAction(t, "row", 1, { type: "align", align: "center" });
    tableAction(t, "row", 1, { type: "bold" });
    const s = partState(t, "row", 1);
    expect(s).toMatchObject({ background: "#fbf3db", color: "#d44c47", align: "center", bold: true });
    expect(t.rows[1].cells[2].style.textAlign).toBe("center");
    tableAction(t, "row", 1, { type: "clear" });
    expect(partState(t, "row", 1)).toMatchObject({ background: "", color: "", align: "", bold: false });
  });

  it("toggles the header row and a header column", () => {
    const { t } = make();
    tableAction(t, "row", 0, { type: "header" });
    expect(t.rows[0].cells[0].tagName).toBe("TD");
    expect(t.rows[0].cells[0].style.backgroundColor).toBe("");
    tableAction(t, "col", 0, { type: "header" });
    expect(t.rows[2].cells[0].tagName).toBe("TH");
    expect(partState(t, "col", 0).header).toBe(true);
    expect(HEADER_BG).toMatch(/^#/);
  });

  it("deleting the last row removes the table", () => {
    const { root, t } = make(1, 2);
    expect(tableAction(t, "row", 0, { type: "delete" })).toBeNull();
    expect(root.querySelector("table")).toBeNull();
  });

  it("sends without the editor's markers", () => {
    const { root, t } = make();
    tableAction(t, "row", 1, { type: "bold" });
    stripTableMarkers(root);
    expect(root.innerHTML).not.toContain("data-");
    expect(root.querySelector("td")!.style.fontWeight).toBe("700");
  });
});
