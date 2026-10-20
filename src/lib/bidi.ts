// Right-to-left text: every block takes the direction of its own first
// letter, so Arabic (and Hebrew, Persian, Urdu…) starts on the right even in
// a left-to-right email, and English lines in an Arabic email start on the left.

export type Dir = "ltr" | "rtl";

// Hebrew, Arabic, Syriac, Thaana, N'Ko, Samaritan, Mandaic, Arabic Extended
// and presentation forms, plus the historic right-to-left planes.
const RTL_LETTER = /[\u{590}-\u{8FF}\u{FB1D}-\u{FDFF}\u{FE70}-\u{FEFC}\u{10800}-\u{10FFF}\u{1E800}-\u{1EFFF}]/u;
const LETTER = /\p{L}/u;
const SUBJECT_PREFIX = /^\s*(?:(?:re|fwd?|aw|wg|sv|vs|tr|rv|رد|إعادة توجيه|توجيه)(?:\s*\[\d+\])?\s*[:：]\s*)+/iu;

/** Direction of the first letter (the first "strong" character), or null if there is none. */
export function textDirection(text: string): Dir | null {
  const m = LETTER.exec(text);
  if (!m) return null;
  return RTL_LETTER.test(m[0]) ? "rtl" : "ltr";
}

/** A subject's direction, ignoring reply and forward prefixes ("Re: تأكيد…" is Arabic). */
export function subjectDir(subject: string): Dir | "auto" {
  return textDirection(subject.replace(SUBJECT_PREFIX, "")) ?? "auto";
}

// ---------------------------------------------------------------------------
// Arabic font (see the note in src/app.css)
// ---------------------------------------------------------------------------

const ARABIC_RANGE = "U+0600-06FF, U+0750-077F, U+0870-08FF, U+FB50-FDFF, U+FE70-FEFC";
const ARABIC_LETTER = /[\u{600}-\u{6FF}\u{750}-\u{77F}\u{870}-\u{8FF}\u{FB50}-\u{FDFF}\u{FE70}-\u{FEFC}]/u;

const ARABIC_SRC = 'local("Noto Sans Arabic"), local("Noto Naskh Arabic"), local("Noto Kufi Arabic"), local("DejaVu Sans")';

/**
 * The "Remail Arabic" faces for documents that don't load app.css. Email
 * frames keep mark positioning (`marks`: harakat land exactly, which matters
 * for fully vowelled text) and get `fixFinalMarks` instead.
 */
export function arabicFontFaces(marks: boolean): string {
  const features = marks ? "" : ';font-feature-settings:"mark" 0,"mkmk" 0';
  return ["100 500", "600 900"]
    .map((weight) => `@font-face{font-family:"Remail Arabic";src:${ARABIC_SRC};font-weight:${weight};unicode-range:${ARABIC_RANGE}${features}}`)
    .join("");
}

// Arabic combining marks (harakat, tanween, shadda, Quranic marks).
const MARK = "\\u{64B}-\\u{65F}\\u{670}\\u{6D6}-\\u{6DC}\\u{6DF}-\\u{6E4}\\u{6E7}\\u{6E8}\\u{6EA}-\\u{6ED}\\u{8D3}-\\u{8FF}";
const FINAL_MARK = new RegExp(`([${MARK}])(?![\\u{600}-\\u{6FF}\\u{750}-\\u{77F}\\u{870}-\\u{8FF}\\u{FB50}-\\u{FDFF}\\u{FE70}-\\u{FEFC}\\u{200C}\\u{200D}])`, "gu");

/**
 * Read-only views: put an Arabic letter mark (U+061C, invisible) after every
 * mark that ends an Arabic run (tanween in "مرحباً" followed by a space or
 * the line's end), so the run no longer starts with a raised glyph. It's a
 * strong right-to-left character from the Arabic block, so it stays in the
 * run (a ZWNJ falls out of it at the end of a `unicode-bidi: plaintext`
 * line) and changes neither shaping nor ordering after an Arabic letter.
 * Copies keep it; search ignores it.
 */
export function fixFinalMarks(root: HTMLElement) {
  if (!ARABIC_LETTER.test(root.textContent ?? "")) return;
  const walker = root.ownerDocument.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const text = n.nodeValue ?? "";
    FINAL_MARK.lastIndex = 0;
    if (FINAL_MARK.test(text)) n.nodeValue = text.replace(FINAL_MARK, "$1\u{61C}");
  }
}


/**
 * Received mail often sets its own fonts (Arial, Tahoma…), which aren't
 * installed here, so its Arabic would go to WebKit's fallback and words
 * ending in a mark would be drawn too high. Put "Remail Arabic" first for
 * every element holding Arabic text whose font list doesn't start with it.
 */
export function useArabicFont(root: HTMLElement) {
  const doc = root.ownerDocument;
  const view = doc.defaultView;
  if (!view || !ARABIC_LETTER.test(root.textContent ?? "")) return;
  const walker = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const found = new Map<HTMLElement, string>();
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const el = n.parentElement;
    if (!el || found.has(el) || !ARABIC_LETTER.test(n.nodeValue ?? "")) continue;
    found.set(el, view.getComputedStyle(el).fontFamily);
  }
  // Read everything first, then write (one style recalculation).
  for (const [el, family] of found) {
    // "important": newsletters often set fonts with !important.
    if (!/^\s*"?Remail Arabic/.test(family)) el.style.setProperty("font-family", `"Remail Arabic", ${family}`, "important");
  }
}

/** Elements whose text forms its own paragraph. */
const BLOCK = new Set([
  "BODY", "P", "DIV", "LI", "DD", "DT", "TD", "TH", "H1", "H2", "H3", "H4", "H5", "H6", "PRE", "ADDRESS", "ARTICLE",
  "SECTION", "HEADER", "FOOTER", "CENTER", "FIGCAPTION", "CAPTION", "SUMMARY", "DETAILS", "ASIDE", "NAV", "FIGURE",
  "UL", "OL", "DL", "BLOCKQUOTE",
]);
/** Containers that follow their first line: bullets and the quote bar go on that side. */
const BY_CONTENT = new Set(["UL", "OL", "DL", "BLOCKQUOTE"]);
const SKIP = new Set(["STYLE", "SCRIPT", "TEMPLATE", "HEAD", "TITLE", "NOSCRIPT"]);

/** First strong direction in an element's own text (`deep`: including nested blocks). */
function firstStrong(el: Element, deep: boolean): Dir | null {
  for (const n of el.childNodes) {
    if (n.nodeType === 3) {
      const d = textDirection(n.nodeValue ?? "");
      if (d) return d;
    } else if (n.nodeType === 1) {
      const c = n as Element;
      if (SKIP.has(c.tagName) || (!deep && BLOCK.has(c.tagName))) continue;
      const d = firstStrong(c, deep);
      if (d) return d;
    }
  }
  return null;
}

/** Swap an element's inline left and right borders, padding and margins. */
function mirrorSides(el: HTMLElement) {
  for (const [l, r] of [["borderLeft", "borderRight"], ["paddingLeft", "paddingRight"], ["marginLeft", "marginRight"]] as const) {
    const [a, b] = [el.style[l], el.style[r]];
    if (a || b) {
      el.style[l] = b;
      el.style[r] = a;
    }
  }
}

/** Does a block hold lines (split by <br>) that go in different directions? */
function mixedLines(el: Element): boolean {
  const dirs = new Set<Dir>();
  let line: Dir | null = null;
  const end = () => {
    if (line) dirs.add(line);
    line = null;
  };
  const visit = (p: Element) => {
    for (const n of p.childNodes) {
      if (n.nodeType === 3) line ??= textDirection(n.nodeValue ?? "");
      else if (n.nodeType === 1) {
        const c = n as Element;
        if (c.tagName === "BR") end();
        else if (!SKIP.has(c.tagName) && !BLOCK.has(c.tagName)) visit(c);
      }
    }
  };
  visit(el);
  end();
  return dirs.size > 1;
}

/**
 * Give every block of text under `root` the direction of its content.
 *
 * - `view`: received mail. A sender's explicit right-to-left stays; their
 *   left-to-right does not, because most mail apps write `dir="ltr"` around
 *   everything, Arabic included. Blocks turned right-to-left also get their
 *   "align left" flipped.
 * - `edit`: the composer. Directions follow the text as it changes; a block
 *   without letters (a new empty line) keeps the one it has.
 *
 * In both, a block whose lines (split by <br>) mix Arabic and English gets a
 * direction per line, tables keep their columns where the sender put them,
 * and a list item going the other way than its list keeps its bullet visible.
 */
export function applyDirections(root: HTMLElement, mode: "view" | "edit", inherited?: Dir) {
  const view = mode === "view";
  const walk = (el: Element, parentDir: Dir, parentOrig: Dir) => {
    if (SKIP.has(el.tagName)) return;
    const html = el as HTMLElement;
    const attr = el.getAttribute("dir")?.toLowerCase();
    const styled = html.style?.direction;
    const explicit: Dir | null = attr === "rtl" || attr === "ltr" ? attr : styled === "rtl" || styled === "ltr" ? styled : null;
    // `orig`: the direction the sender's own markup gives; `dir`: ours.
    const orig = explicit ?? parentOrig;
    let dir: Dir = explicit ?? parentDir;
    if (el.tagName === "TABLE") {
      if (!explicit && dir !== orig) {
        el.setAttribute("dir", orig);
        dir = orig;
      }
    } else if (BLOCK.has(el.tagName) && !(view && (attr === "rtl" || styled === "rtl"))) {
      const own = firstStrong(el, BY_CONTENT.has(el.tagName));
      if (own && !view && own === parentDir) {
        if (attr) el.removeAttribute("dir");
        if (styled) html.style.direction = "";
        dir = own;
      } else if (own && own !== dir) {
        el.setAttribute("dir", own);
        // In received mail, also beat a `direction` from the sender's stylesheet.
        if (styled || view) html.style.direction = own;
        if (own === "rtl" && view) {
          if (html.style.textAlign === "left") html.style.textAlign = "right";
          if (el.getAttribute("align")?.toLowerCase() === "left") el.setAttribute("align", "right");
          // Quote bars drawn with inline left-side styles (Gmail) move to the right.
          if (el.tagName === "BLOCKQUOTE") mirrorSides(html);
        }
        dir = own;
      }
      if (mixedLines(el)) html.style.unicodeBidi = "plaintext";
      else if (!view && html.style.unicodeBidi === "plaintext") html.style.unicodeBidi = "";
    }
    if (el.tagName === "LI") {
      if (dir !== parentDir) html.style.listStylePosition = "inside";
      else if (!view && html.style.listStylePosition === "inside") html.style.listStylePosition = "";
    }
    for (const c of el.children) walk(c, dir, orig);
  };

  const rootAttr = root.getAttribute("dir")?.toLowerCase();
  let start: Dir =
    rootAttr === "rtl" || rootAttr === "ltr"
      ? rootAttr
      : (inherited ?? (root.ownerDocument.defaultView?.getComputedStyle(root).direction === "rtl" ? "rtl" : "ltr"));
  const rootOrig = start;
  if (view && rootAttr !== "rtl") {
    // Text straight in <body> (plain-text mail as <br> lines) sets what its blocks inherit.
    const own = firstStrong(root, false);
    if (own && own !== start) {
      root.setAttribute("dir", own);
      start = own;
    }
    if (mixedLines(root)) root.style.unicodeBidi = "plaintext";
  }
  for (const c of root.children) walk(c, start, rootOrig);
}

/** A caret position that survives moving nodes into a new block. */
type Point = { node: Node; offset: number } | { before: ChildNode | null };

/**
 * The composer: put text typed straight into the editor (WebKit leaves the
 * first line outside any block) into its own block so it can carry a
 * direction, then set directions. Keeps the caret where it was.
 */
export function directEditor(editor: HTMLElement) {
  // Undoing the first typed text leaves an empty block; drop it so the placeholder shows.
  if (editor.childNodes.length === 1 && editor.firstChild?.nodeType === 1 && (editor.firstChild as Element).tagName === "DIV" && !editor.firstChild.hasChildNodes()) {
    editor.replaceChildren();
    return;
  }
  const sel = editor.ownerDocument.getSelection();
  const save = (node: Node | null, offset: number): Point | null =>
    !node ? null : node === editor ? { before: editor.childNodes[offset] ?? null } : { node, offset };
  const inside = !!sel && sel.rangeCount > 0 && editor.contains(sel.anchorNode);
  const anchor = inside ? save(sel!.anchorNode, sel!.anchorOffset) : null;
  const focus = inside ? save(sel!.focusNode, sel!.focusOffset) : null;

  const wrappers = new Set<Node>();
  let run: ChildNode[] = [];
  const flush = (before: ChildNode | null) => {
    const hasText = run.some((n) => (n.nodeType === 3 ? (n.nodeValue ?? "").trim() !== "" : n.nodeType === 1 && (n as Element).tagName !== "BR"));
    if (hasText) {
      const div = editor.ownerDocument.createElement("div");
      editor.insertBefore(div, before);
      for (const n of run) div.appendChild(n);
      wrappers.add(div);
    }
    run = [];
  };
  for (const n of [...editor.childNodes]) {
    const tag = n.nodeType === 1 ? (n as Element).tagName : "";
    if (BLOCK.has(tag) || tag === "TABLE" || tag === "HR") flush(n);
    else run.push(n);
  }
  flush(null);

  if (wrappers.size && anchor && focus && sel) {
    const at = (p: Point): [Node, number] => {
      if ("node" in p) return [p.node, p.offset];
      if (p.before) {
        const parent = p.before.parentNode ?? editor;
        return [parent, Math.max(0, [...parent.childNodes].indexOf(p.before))];
      }
      const last = editor.lastChild;
      return last && wrappers.has(last) ? [last, last.childNodes.length] : [editor, editor.childNodes.length];
    };
    try {
      sel.setBaseAndExtent(...at(anchor), ...at(focus));
    } catch {
      /* the caret's node went away; leave it */
    }
  }
  applyDirections(editor, "edit", "ltr");
}

let directed: [string, string] = ["", ""];

/** Set directions in an HTML fragment (quoted text) before it is sent. */
export function directHtml(html: string): string {
  if (!html || !RTL_LETTER.test(html)) return html;
  if (directed[0] === html) return directed[1];
  const doc = new DOMParser().parseFromString(`<body>${html}</body>`, "text/html");
  applyDirections(doc.body, "view", "ltr");
  const b = doc.body;
  const attrs = (b.getAttribute("dir") === "rtl" ? ' dir="rtl"' : "") + (b.style.unicodeBidi === "plaintext" ? ' style="unicode-bidi:plaintext"' : "");
  const out = attrs ? `<div${attrs}>${b.innerHTML}</div>` : b.innerHTML;
  directed = [html, out];
  return out;
}
