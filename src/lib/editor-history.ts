// Undo / redo for the composer's body.
//
// WebKit's own undo stack breaks as soon as the page changes the editable DOM
// itself (text directions on every keystroke, to-do boxes, table edits), and
// then Ctrl+Z does nothing. So the body keeps its own history of snapshots.

interface Snapshot {
  html: string;
  /** Selection as child-index paths from the editor root. */
  sel: { a: number[]; ao: number; f: number[]; fo: number } | null;
}

/** Typing within this long after the previous keystroke joins the same undo step. */
const GROUP_MS = 1000;
const LIMIT = 300;

function pathOf(root: Node, node: Node): number[] | null {
  const path: number[] = [];
  for (let n: Node | null = node; n && n !== root; n = n.parentNode) {
    const parent: Node | null = n.parentNode;
    if (!parent) return null;
    path.unshift(Array.prototype.indexOf.call(parent.childNodes, n));
  }
  return path;
}

function nodeAt(root: Node, path: number[]): Node | null {
  let n: Node | null = root;
  for (const i of path) n = n?.childNodes[i] ?? null;
  return n;
}

export class EditorHistory {
  private stack: Snapshot[] = [];
  private at = -1;
  private lastTyping = 0;
  private typingStep = false;

  constructor(private readonly el: HTMLElement) {}

  private snap(): Snapshot {
    const sel = this.el.ownerDocument.getSelection();
    let s: Snapshot["sel"] = null;
    if (sel && sel.rangeCount && sel.anchorNode && sel.focusNode && this.el.contains(sel.anchorNode) && this.el.contains(sel.focusNode)) {
      const a = pathOf(this.el, sel.anchorNode);
      const f = pathOf(this.el, sel.focusNode);
      if (a && f) s = { a, ao: sel.anchorOffset, f, fo: sel.focusOffset };
    }
    return { html: this.el.innerHTML, sel: s };
  }

  /** Start over from what the body holds now (a new draft, a template). */
  reset() {
    this.stack = [this.snap()];
    this.at = 0;
    this.typingStep = false;
  }

  /**
   * Remember the body after a change. `typing`: a typed character or a deleted
   * one, which joins the step before it while the user keeps typing a word.
   */
  record(typing = false) {
    if (this.at < 0) return this.reset();
    const s = this.snap();
    const top = this.stack[this.at];
    if (top.html === s.html) {
      top.sel = s.sel;
      return;
    }
    const now = Date.now();
    const join = typing && this.typingStep && now - this.lastTyping < GROUP_MS && this.at > 0;
    this.stack.length = this.at + 1;
    if (join) this.stack[this.at] = s;
    else this.stack.push(s);
    if (this.stack.length > LIMIT) this.stack.shift();
    this.at = this.stack.length - 1;
    this.typingStep = typing;
    this.lastTyping = now;
  }

  /** Fold a follow-up change (sizing a picture, tidying a table) into the last step. */
  amend() {
    if (this.at < 0) return this.reset();
    this.stack[this.at] = this.snap();
  }

  /** A word ended (space, Enter): the next typing is a new step. */
  breakStep() {
    this.typingStep = false;
  }

  canUndo() {
    return this.at > 0;
  }

  canRedo() {
    return this.at < this.stack.length - 1;
  }

  undo(): boolean {
    // Unrecorded edits (rare) are kept so redo can bring them back.
    const now = this.snap();
    if (this.at >= 0 && now.html !== this.stack[this.at].html) {
      this.stack.length = this.at + 1;
      this.stack.push(now);
      this.at++;
    }
    if (!this.canUndo()) return false;
    this.at--;
    this.apply(this.stack[this.at], this.stack[this.at + 1]);
    return true;
  }

  redo(): boolean {
    if (!this.canRedo()) return false;
    this.at++;
    this.apply(this.stack[this.at], this.stack[this.at]);
    return true;
  }

  /** Show `s`, with the caret where the change being undone or redone happened. */
  private apply(s: Snapshot, caretFrom: Snapshot) {
    this.typingStep = false;
    this.el.innerHTML = s.html;
    const doc = this.el.ownerDocument;
    const sel = doc.getSelection();
    if (!sel) return;
    const want = caretFrom === s ? s.sel : (s.sel ?? caretFrom.sel);
    const r = doc.createRange();
    try {
      const a = want && nodeAt(this.el, want.a);
      const f = want && nodeAt(this.el, want.f);
      if (!want || !a || !f) throw new Error("gone");
      sel.setBaseAndExtent(a, Math.min(want.ao, len(a)), f, Math.min(want.fo, len(f)));
      return;
    } catch {
      r.selectNodeContents(this.el);
      r.collapse(false);
    }
    sel.removeAllRanges();
    sel.addRange(r);
  }
}

const len = (n: Node) => (n.nodeType === 3 ? (n.nodeValue ?? "").length : n.childNodes.length);
