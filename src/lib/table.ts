// Tables in the composer body: building them, the row and column menus, and
// keeping their look email-safe (every style is inline) and right-to-left
// when their text is Arabic.

import { textDirection } from "./bidi";

const BORDER = "1px solid #e3ded8";
/** The header row (or column) tint. */
export const HEADER_BG = "#efe6dd";

export const BACKGROUNDS: [string, string][] = [
  ["", "Default"],
  ["#f1f1ef", "Gray"],
  ["#f4eeee", "Brown"],
  ["#fbecdd", "Orange"],
  ["#fbf3db", "Yellow"],
  ["#edf3ec", "Green"],
  ["#e7f3f8", "Blue"],
  ["#f6f3f9", "Purple"],
  ["#faf1f5", "Pink"],
  ["#fdebec", "Red"],
];

export const TEXT_COLORS: [string, string][] = [
  ["", "Default"],
  ["#787774", "Gray"],
  ["#9f6b53", "Brown"],
  ["#d9730d", "Orange"],
  ["#cb912f", "Yellow"],
  ["#448361", "Green"],
  ["#337ea9", "Blue"],
  ["#9065b0", "Purple"],
  ["#c14c8a", "Pink"],
  ["#d44c47", "Red"],
];

export type Align = "left" | "center" | "right";
export type Part = "row" | "col";

export type TableAction =
  | { type: "insertBefore" }
  | { type: "insertAfter" }
  | { type: "moveBefore" }
  | { type: "moveAfter" }
  | { type: "duplicate" }
  | { type: "delete" }
  | { type: "header" }
  | { type: "background"; color: string }
  | { type: "color"; color: string }
  | { type: "align"; align: Align }
  | { type: "bold" }
  | { type: "italic" }
  | { type: "strike" }
  | { type: "clear" };

const CELL_STYLE = "padding:8px 10px;vertical-align:top;min-width:60px";

/** A new table: a header row and two rows, as wide as the message. */
export function newTableHtml(rows = 3, cols = 3, dir: "ltr" | "rtl" = "ltr"): string {
  const head = `<tr>${`<th style="${CELL_STYLE}"><br></th>`.repeat(cols)}</tr>`;
  const row = `<tr>${`<td style="${CELL_STYLE}"><br></td>`.repeat(cols)}</tr>`;
  const d = dir === "rtl" ? ' dir="rtl"' : "";
  return `<table data-new data-header="row"${d} style="border-collapse:separate;border-spacing:0;border:${BORDER};border-radius:8px;width:100%;margin:6px 0"><tbody>${head}${row.repeat(rows - 1)}</tbody></table><div><br></div>`;
}

export const rowsOf = (t: HTMLTableElement) => [...t.rows];
const cellsOfRow = (r: HTMLTableRowElement) => [...r.cells];
const columnCells = (t: HTMLTableElement, c: number) => rowsOf(t).map((r) => r.cells[c]).filter(Boolean);

export function cellPosition(cell: HTMLTableCellElement): { table: HTMLTableElement; row: number; col: number } | null {
  const tr = cell.parentElement as HTMLTableRowElement | null;
  const table = cell.closest("table");
  if (!tr || !table) return null;
  return { table, row: tr.rowIndex, col: cell.cellIndex };
}

/** The cells a row or column menu works on. */
export function partCells(t: HTMLTableElement, part: Part, index: number): HTMLTableCellElement[] {
  return part === "row" ? cellsOfRow(t.rows[index]) : columnCells(t, index);
}

function setStyle(el: HTMLElement, prop: string, value: string) {
  // Only touch what changes: every write is a DOM mutation (and an undo step's worth of noise).
  if (el.style.getPropertyValue(prop) !== value) {
    if (value) el.style.setProperty(prop, value);
    else el.style.removeProperty(prop);
  }
}

function retag(cell: HTMLTableCellElement, tag: "TD" | "TH"): HTMLTableCellElement {
  if (cell.tagName === tag) return cell;
  const n = cell.ownerDocument.createElement(tag) as HTMLTableCellElement;
  for (const a of cell.attributes) n.setAttribute(a.name, a.value);
  while (cell.firstChild) n.appendChild(cell.firstChild);
  cell.replaceWith(n);
  return n;
}

function emptyCellLike(cell: HTMLTableCellElement): HTMLTableCellElement {
  const n = cell.cloneNode(false) as HTMLTableCellElement;
  n.removeAttribute("dir");
  n.innerHTML = "<br>";
  return n;
}

/** The table's direction: from its text, else from what it sits in. */
function tableDir(t: HTMLTableElement): "ltr" | "rtl" {
  const fromText = textDirection(t.textContent ?? "");
  if (fromText) return fromText;
  const set = t.getAttribute("dir");
  if (set === "rtl" || set === "ltr") return set;
  return t.parentElement && getComputedStyle(t.parentElement).direction === "rtl" ? "rtl" : "ltr";
}

/**
 * Header cells, inner borders and direction after any change. Idempotent and
 * writes only what differs, so it can run on every keystroke.
 */
export function normalizeTable(t: HTMLTableElement) {
  const dir = tableDir(t);
  if (t.getAttribute("dir") !== dir) t.setAttribute("dir", dir);
  const headerRow = t.dataset.header?.includes("row");
  const headerCol = t.dataset.header?.includes("col");
  rowsOf(t).forEach((tr, r) => {
    cellsOfRow(tr).forEach((c, ci) => {
      const header = (headerRow && r === 0) || (headerCol && ci === 0);
      const cell = retag(c, header ? "TH" : "TD");
      // Inner lines only: the table draws the outside (and its rounded corners).
      setStyle(cell, "border-top", r > 0 ? BORDER : "");
      // The column line is on the side toward the first column, which is on the right in RTL.
      setStyle(cell, dir === "rtl" ? "border-right" : "border-left", ci > 0 ? BORDER : "");
      setStyle(cell, dir === "rtl" ? "border-left" : "border-right", "");
      setStyle(cell, "background-color", cell.dataset.bg ?? (header ? HEADER_BG : ""));
      setStyle(cell, "font-weight", cell.dataset.bold ? "700" : header ? "600" : "");
      setStyle(cell, "text-align", cell.dataset.align ?? (dir === "rtl" ? "right" : "left"));
      // Corner cells follow the table's rounded corners, so a coloured cell doesn't poke out.
      const lastRow = r === t.rows.length - 1;
      const start = ci === 0;
      const end = ci === tr.cells.length - 1;
      const [left, right] = dir === "rtl" ? [end, start] : [start, end];
      setStyle(cell, "border-top-left-radius", r === 0 && left ? "7px" : "");
      setStyle(cell, "border-top-right-radius", r === 0 && right ? "7px" : "");
      setStyle(cell, "border-bottom-left-radius", lastRow && left ? "7px" : "");
      setStyle(cell, "border-bottom-right-radius", lastRow && right ? "7px" : "");
    });
  });
}

/** What the menu shows as already on. */
export function partState(t: HTMLTableElement, part: Part, index: number) {
  const cells = partCells(t, part, index);
  const first = cells[0];
  const rows = t.rows.length;
  const cols = t.rows[0]?.cells.length ?? 0;
  return {
    background: first?.dataset.bg ?? "",
    color: first?.style.color ? (first.dataset.color ?? "") : "",
    align: (first?.dataset.align ?? "") as Align | "",
    bold: cells.length > 0 && cells.every((c) => !!c.dataset.bold),
    italic: cells.length > 0 && cells.every((c) => c.style.fontStyle === "italic"),
    strike: cells.length > 0 && cells.every((c) => c.style.textDecoration.includes("line-through")),
    header: !!t.dataset.header?.includes(part),
    canHeader: index === 0,
    first: index === 0,
    last: index === (part === "row" ? rows : cols) - 1,
  };
}

function toggleHeader(t: HTMLTableElement, part: Part) {
  const parts = new Set((t.dataset.header ?? "").split(" ").filter(Boolean));
  if (parts.has(part)) parts.delete(part);
  else parts.add(part);
  if (parts.size) t.dataset.header = [...parts].join(" ");
  else delete t.dataset.header;
}

function unwrapFormatting(cell: HTMLElement) {
  for (const el of [...cell.querySelectorAll("b, strong, i, em, u, s, strike, font, span")]) el.replaceWith(...el.childNodes);
}

/**
 * Run a row or column menu action. Returns the cell to put the caret in
 * afterwards, or null when the table went away.
 */
export function tableAction(t: HTMLTableElement, part: Part, index: number, a: TableAction): HTMLTableCellElement | null {
  const rows = rowsOf(t);
  const cells = partCells(t, part, index);
  switch (a.type) {
    case "insertBefore":
    case "insertAfter":
    case "duplicate": {
      const after = a.type !== "insertBefore";
      const make = (c: HTMLTableCellElement) => (a.type === "duplicate" ? (c.cloneNode(true) as HTMLTableCellElement) : emptyCellLike(c));
      if (part === "row") {
        const tr = rows[index];
        const n = tr.cloneNode(false) as HTMLTableRowElement;
        for (const c of cellsOfRow(tr)) n.appendChild(make(c));
        if (after) tr.after(n);
        else tr.before(n);
        // A new first row isn't meant to inherit the header's look; the header stays the first row.
        normalizeTable(t);
        return n.cells[0];
      }
      for (const c of cells) {
        const n = make(c);
        if (after) c.after(n);
        else c.before(n);
      }
      normalizeTable(t);
      return t.rows[0].cells[after ? index + 1 : index];
    }
    case "moveBefore":
    case "moveAfter": {
      const to = a.type === "moveBefore" ? index - 1 : index + 1;
      if (part === "row") {
        if (to < 0 || to >= rows.length) return cells[0];
        if (to < index) rows[to].before(rows[index]);
        else rows[to].after(rows[index]);
      } else {
        const other = partCells(t, "col", to);
        if (to < 0 || other.length === 0) return cells[0];
        cells.forEach((c, i) => (to < index ? other[i].before(c) : other[i].after(c)));
      }
      normalizeTable(t);
      return partCells(t, part, to)[0] ?? null;
    }
    case "delete": {
      const count = part === "row" ? rows.length : (t.rows[0]?.cells.length ?? 0);
      if (count <= 1) {
        const next = t.nextElementSibling;
        t.remove();
        if (next && next.tagName === "DIV" && !next.textContent) next.remove();
        return null;
      }
      if (part === "row") rows[index].remove();
      else cells.forEach((c) => c.remove());
      normalizeTable(t);
      return part === "row" ? t.rows[Math.min(index, t.rows.length - 1)].cells[0] : t.rows[0].cells[Math.min(index, t.rows[0].cells.length - 1)];
    }
    case "header":
      toggleHeader(t, part);
      break;
    case "background":
      for (const c of cells) {
        if (a.color) c.dataset.bg = a.color;
        else delete c.dataset.bg;
      }
      break;
    case "color":
      for (const c of cells) {
        if (a.color) {
          c.dataset.color = a.color;
          c.style.color = a.color;
        } else {
          delete c.dataset.color;
          c.style.removeProperty("color");
        }
      }
      break;
    case "align":
      for (const c of cells) c.dataset.align = a.align;
      break;
    case "bold": {
      const on = !cells.every((c) => c.dataset.bold);
      for (const c of cells) {
        if (on) c.dataset.bold = "1";
        else delete c.dataset.bold;
      }
      break;
    }
    case "italic": {
      const on = !cells.every((c) => c.style.fontStyle === "italic");
      for (const c of cells) setStyle(c, "font-style", on ? "italic" : "");
      break;
    }
    case "strike": {
      const on = !cells.every((c) => c.style.textDecoration.includes("line-through"));
      for (const c of cells) setStyle(c, "text-decoration", on ? "line-through" : "");
      break;
    }
    case "clear":
      for (const c of cells) {
        for (const k of ["bg", "color", "align", "bold"]) delete c.dataset[k];
        for (const p of ["color", "font-style", "text-decoration"]) c.style.removeProperty(p);
        unwrapFormatting(c);
      }
      break;
  }
  normalizeTable(t);
  return partCells(t, part, index)[0] ?? null;
}

/** Copy-free markers the editor uses; the email doesn't need them. */
export function stripTableMarkers(root: HTMLElement) {
  for (const el of root.querySelectorAll("table, td, th")) {
    for (const k of ["header", "bg", "color", "align", "bold", "new"]) el.removeAttribute(`data-${k}`);
  }
}
