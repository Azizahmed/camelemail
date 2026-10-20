// Helpers for the composer's rich-text editor: the "/" command menu, to-do
// lists, tables and pictures in the body.

import { newTableHtml, normalizeTable, stripTableMarkers } from "./table";

/** A "/" typed at the start of a line or after a space, and what follows it up to the caret. */
export interface SlashMatch {
  /** Covers the "/" and the query, to be deleted when a command runs. */
  range: Range;
  query: string;
}

const SLASH = /(?:^|[\s ])\/([^\s /]{0,24})$/;

/** The "/command" being typed at the caret, if any. */
export function slashAtCaret(editor: HTMLElement): SlashMatch | null {
  const sel = editor.ownerDocument.getSelection();
  if (!sel || sel.rangeCount === 0 || !sel.isCollapsed) return null;
  const node = sel.anchorNode;
  if (!node || node.nodeType !== 3 || !editor.contains(node)) return null;
  const before = (node.nodeValue ?? "").slice(0, sel.anchorOffset);
  const m = SLASH.exec(before);
  if (!m) return null;
  const range = editor.ownerDocument.createRange();
  range.setStart(node, sel.anchorOffset - m[1].length - 1);
  range.setEnd(node, sel.anchorOffset);
  return { range, query: m[1] };
}

/** Where the caret (or a collapsed range) is on screen. */
export function caretRect(range: Range): DOMRect {
  const rects = range.getClientRects();
  if (rects.length) return rects[rects.length - 1];
  const r = range.getBoundingClientRect();
  if (r.width || r.height || r.top) return r;
  // An empty line has no rects: use its block.
  const el = range.startContainer.nodeType === 1 ? (range.startContainer as Element) : range.startContainer.parentElement;
  return el?.getBoundingClientRect() ?? new DOMRect();
}

export function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

/** An empty table (see table.ts for its look). */
export const tableHtml = newTableHtml;

function caretInto(cell: Element) {
  const sel = cell.ownerDocument.getSelection();
  const r = cell.ownerDocument.createRange();
  r.selectNodeContents(cell);
  r.collapse(true);
  sel?.removeAllRanges();
  sel?.addRange(r);
}

/** After inserting a table: the caret goes into its first cell. */
export function enterNewTable(editor: HTMLElement) {
  const t = editor.querySelector("table[data-new]");
  if (!t) return;
  t.removeAttribute("data-new");
  const cell = t.querySelector("td, th");
  if (cell) caretInto(cell);
}

/** Tab / Shift+Tab between table cells; Tab in the last cell adds a row. False when the caret isn't in a table. */
export function tabInTable(editor: HTMLElement, back: boolean): boolean {
  const n = editor.ownerDocument.getSelection()?.anchorNode;
  const el = n ? (n.nodeType === 1 ? (n as Element) : n.parentElement) : null;
  const cell = el?.closest("td, th");
  const table = cell?.closest("table");
  if (!cell || !table || !editor.contains(table)) return false;
  const cells = [...table.querySelectorAll("td, th")];
  const i = cells.indexOf(cell);
  let next = cells[back ? i - 1 : i + 1];
  if (!next && !back) {
    const last = cell.closest("tr")!;
    const row = last.cloneNode(true) as HTMLTableRowElement;
    for (const c of row.children) c.innerHTML = "<br>";
    last.after(row);
    normalizeTable(table);
    next = row.children[0];
  }
  if (next) caretInto(next);
  return true;
}

export function imageHtml(src: string, name: string): string {
  return `<img data-new src="${escapeHtml(src)}" alt="${escapeHtml(name)}">`;
}

/** Widest a picture goes out (Outlook ignores max-width, so wide ones get a width). */
const MAX_IMAGE_WIDTH = 640;

/**
 * Size just-inserted pictures. Set here, not in the inserted HTML: WebKit drops
 * inline styles that repeat what the editor's CSS already says.
 */
export function sizeNewImages(editor: HTMLElement) {
  for (const img of editor.querySelectorAll<HTMLImageElement>("img[data-new]")) {
    img.removeAttribute("data-new");
    img.style.maxWidth = "100%";
    img.style.height = "auto";
    const cap = () => {
      if (img.naturalWidth > MAX_IMAGE_WIDTH) img.width = MAX_IMAGE_WIDTH;
    };
    if (img.complete) cap();
    else img.addEventListener("load", cap, { once: true });
  }
}

export const isImage = (mimeType: string) => /^image\/(png|jpe?g|gif|webp)$/i.test(mimeType);

/** The to-do list item a click landed on the box of (the box sits at the start edge). */
export function todoBoxAt(e: MouseEvent): HTMLLIElement | null {
  const li = (e.target as Element | null)?.closest?.("ul.todo > li") as HTMLLIElement | null;
  if (!li) return null;
  const r = li.getBoundingClientRect();
  const rtl = getComputedStyle(li).direction === "rtl";
  const into = rtl ? r.right - e.clientX : e.clientX - r.left;
  return into >= -4 && into <= 22 ? li : null;
}

export function toggleTodo(li: HTMLElement) {
  li.dataset.checked = li.dataset.checked === "true" ? "false" : "true";
}

/** The list item holding the caret, when it is in a to-do list. */
export function todoItemAtCaret(editor: HTMLElement): HTMLLIElement | null {
  const sel = editor.ownerDocument.getSelection();
  const n = sel?.anchorNode;
  const el = n ? (n.nodeType === 1 ? (n as Element) : n.parentElement) : null;
  const li = el?.closest("li") as HTMLLIElement | null;
  return li && editor.contains(li) && li.parentElement?.classList.contains("todo") ? li : null;
}

/** The list around the caret, if any. */
export function listAtCaret(editor: HTMLElement): HTMLUListElement | HTMLOListElement | null {
  const sel = editor.ownerDocument.getSelection();
  const n = sel?.anchorNode;
  const el = n ? (n.nodeType === 1 ? (n as Element) : n.parentElement) : null;
  const list = el?.closest("ul, ol") as HTMLUListElement | HTMLOListElement | null;
  return list && editor.contains(list) ? list : null;
}

/**
 * The body as it is sent: to-do boxes become ☐/☑ characters, which every
 * client shows (the editor draws them with CSS).
 */
export function finalizeHtml(html: string): string {
  if (!html.includes("todo") && !html.includes("data-")) return html;
  const doc = new DOMParser().parseFromString(`<div id="r">${html}</div>`, "text/html");
  const root = doc.getElementById("r")!;
  for (const ul of root.querySelectorAll("ul.todo")) {
    (ul as HTMLElement).style.listStyle = "none";
    (ul as HTMLElement).style.paddingInlineStart = "4px";
    for (const li of ul.children) {
      if (li.tagName !== "LI") continue;
      const done = (li as HTMLElement).dataset.checked === "true";
      li.prepend(doc.createTextNode(done ? "☑ " : "☐ "));
      li.removeAttribute("data-checked");
    }
    ul.classList.remove("todo");
    if (!ul.classList.length) ul.removeAttribute("class");
  }
  stripTableMarkers(root);
  return root.innerHTML;
}

/** Bytes of the pictures embedded in the body (they count toward the 25 MB limit). */
export function inlineImageBytes(html: string): number {
  let n = 0;
  for (const m of html.matchAll(/src="data:image\/[a-z0-9.+-]+;base64,([A-Za-z0-9+/=]+)"/gi)) n += Math.floor((m[1].length * 3) / 4);
  return n;
}

export function fileToDataUrl(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result));
    r.onerror = () => reject(r.error);
    r.readAsDataURL(file);
  });
}
