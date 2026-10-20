<script lang="ts">
  import {
    Clock3,
    Code,
    ExternalLink,
    FileText,
    GripHorizontal,
    GripVertical,
    Heading1,
    Heading2,
    Heading3,
    Image,
    Link,
    List,
    ListOrdered,
    ListTodo,
    Minus,
    Paperclip,
    PenLine,
    Quote,
    Send,
    Sparkles,
    Square,
    Table,
    Trash2,
    Type,
    X,
  } from "@lucide/svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { onDestroy, onMount } from "svelte";
  import { api, errorText, newRequestId } from "../../lib/api";
  import { directEditor, directHtml, subjectDir } from "../../lib/bidi";
  import {
    caretRect,
    escapeHtml,
    fileToDataUrl,
    finalizeHtml,
    imageHtml,
    inlineImageBytes,
    isImage,
    listAtCaret,
    enterNewTable,
    sizeNewImages,
    slashAtCaret,
    tabInTable,
    todoBoxAt,
    toggleTodo,
  } from "../../lib/editor";
  import { EditorHistory } from "../../lib/editor-history";
  import { base64Size, fileSize, wakeDate } from "../../lib/format";
  import { cellPosition, newTableHtml, normalizeTable, partState, tableAction, type Part, type TableAction } from "../../lib/table";
  import type { Account, AiSettingsView, AppSettings, Draft, OutgoingAttachment, Signature, Template } from "../../lib/types";
  import { confirmDialog, toast, toastError } from "../../lib/ui.svelte";
  import EmailFrame from "../thread/EmailFrame.svelte";
  import Popover from "../menus/Popover.svelte";
  import SendLaterMenu from "../menus/SendLaterMenu.svelte";
  import BubbleToolbar, { type FormatAction, type FormatState } from "./BubbleToolbar.svelte";
  import RecipientInput from "./RecipientInput.svelte";
  import SlashMenu, { filterItems, type SlashItem } from "./SlashMenu.svelte";
  import TableMenu from "./TableMenu.svelte";

  const MAX_TOTAL = 25 * 1024 * 1024;
  /** Highlighter colour: light enough to read dark text on in any client. */
  const HIGHLIGHT = "#ffe08a";

  let {
    draft,
    variant,
    onclose,
    onpopout,
    onsubject,
  }: {
    draft: Draft;
    variant: "window" | "inline";
    onclose: (reason: "sent" | "discarded" | "saved") => void;
    onpopout?: (d: Draft) => void;
    onsubject?: (subject: string) => void;
  } = $props();

  // svelte-ignore state_referenced_locally
  let d: Draft = $state(structuredClone($state.snapshot(draft)) as Draft);
  let accounts: Account[] = $state([]);
  let signatures: Signature[] = $state([]);
  let templates: Template[] = $state([]);
  let settings: AppSettings | null = $state(null);
  let showCc = $state(d.cc.length > 0 || d.bcc.length > 0);
  let showQuote = $state(false);
  let scheduleAt: number | null = $state(null);
  let menu = $state<null | { kind: "schedule" | "signature" | "template" | "ai"; anchor: DOMRect }>(null);
  /** The "/" block menu while a command is being typed. */
  let slash = $state<null | { anchor: DOMRect; items: SlashItem[]; active: number }>(null);
  /** A "/" was typed: follow what comes after it. */
  let slashArmed = false;
  /** The formatting bar over selected text (or over the caret while a link is typed). */
  let bubble = $state<null | { rect: DOMRect }>(null);
  let fmt = $state<FormatState>({ bold: false, italic: false, underline: false, strike: false, highlight: false, block: "", list: "" });
  let linking = $state(false);
  /** A mouse selection in progress: the bar waits for the button to come up. */
  let mouseSelecting = false;
  /** Where the caret last was in the body, for inserts after a file picker took the focus. */
  let lastRange: Range | null = null;
  let unlistenDrop: (() => void) | undefined;
  /** The body's own undo history (WebKit's breaks on the changes the page makes). */
  let history: EditorHistory | undefined;
  /** The table cell under the pointer: places the row and column handles. */
  let tgrip = $state<null | { table: HTMLTableElement; row: number; col: number; rowRect: DOMRect; colRect: DOMRect; tableRect: DOMRect; rtl: boolean }>(null);
  let tmenu = $state<null | { table: HTMLTableElement; part: Part; index: number; anchor: DOMRect; state: ReturnType<typeof partState> }>(null);
  let fromMenu = $state<DOMRect | null>(null);
  let ai = $state<AiSettingsView | null>(null);
  let aiPrompt = $state("");
  let aiRunning = $state(false);
  let aiPreview = $state("");
  let aiRequest = "";
  let destroyed = false;
  /** Sent, discarded or saved by the user: nothing left to keep on unmount. */
  let closed = false;
  /** Edits not yet autosaved. */
  let dirty = false;

  onDestroy(() => {
    // A late AI result must not re-save a draft that was sent or discarded.
    destroyed = true;
    clearTimeout(saveTimer);
    unlistenDrop?.();
    document.removeEventListener("selectionchange", onSelectionChange);
    // An inline reply goes away without a prompt when another thread opens: keep the last edits.
    if (!closed && dirty) void api.saveDraft(outgoing(), false).catch(() => {});
    if (aiRunning) void api.aiCancel(aiRequest);
  });
  let aiRange = $state<Range | null>(null);
  let sending = $state(false);
  let dragging = $state(false);
  let root: HTMLDivElement | undefined = $state();
  let editor: HTMLDivElement | undefined = $state();
  /** The editor element, kept for the save on unmount. */
  let editorEl: HTMLDivElement | undefined;
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  let savedRange: Range | null = null;

  const totalSize = $derived(d.attachments.reduce((n, a) => n + base64Size(a.dataBase64), 0));
  const slashItems = $derived<SlashItem[]>([
    ...(ai?.settings.enabled ? [{ id: "ai", label: "AI assistance", hint: "Draft or rewrite with the assistant", icon: Sparkles, keywords: "assistant write" }] : []),
    { id: "text", label: "Text", hint: "Plain paragraph", icon: Type, keywords: "paragraph normal" },
    { id: "h1", label: "Heading 1", hint: "Large section heading", icon: Heading1, keywords: "title h1" },
    { id: "h2", label: "Heading 2", hint: "Medium section heading", icon: Heading2, keywords: "subtitle h2" },
    { id: "h3", label: "Heading 3", hint: "Small section heading", icon: Heading3, keywords: "h3" },
    { id: "todo", label: "To-do list", hint: "Track tasks with checkboxes", icon: ListTodo, keywords: "checkbox checklist task" },
    { id: "ul", label: "Bulleted list", hint: "A simple bulleted list", icon: List, keywords: "bullet unordered" },
    { id: "ol", label: "Numbered list", hint: "A list with numbers", icon: ListOrdered, keywords: "ordered number" },
    { id: "quote", label: "Quote", hint: "Set a passage apart", icon: Quote, keywords: "blockquote citation" },
    { id: "code", label: "Code", hint: "Monospaced block", icon: Code, keywords: "pre snippet" },
    { id: "divider", label: "Divider", hint: "A line between sections", icon: Minus, keywords: "line hr separator" },
    { id: "table", label: "Table", hint: "Rows and columns", icon: Table, keywords: "grid" },
    { id: "image", label: "Image", hint: "Put a picture in the message", icon: Image, keywords: "photo picture" },
    { id: "attach", label: "Attachment", hint: "Attach files", icon: Paperclip, keywords: "file upload" },
    { id: "link", label: "Link", hint: "Add a web link", icon: Link, keywords: "url" },
    { id: "template", label: "Template", hint: "Start from a template", icon: FileText, keywords: "snippet" },
  ]);
  const fromEmail = $derived(accounts.find((a) => a.id === d.accountId)?.email ?? "");
  const usableSignatures = $derived(signatures.filter((s) => !s.accountId || s.accountId === d.accountId));

  onMount(async () => {
    editorEl = editor;
    // Drafts from before (or replies) get their text directions too.
    setBody(d.htmlBody);
    if (editor) {
      history = new EditorHistory(editor);
      history.reset();
    }
    document.addEventListener("selectionchange", onSelectionChange);
    // Files dropped from the desktop arrive through the window, not as HTML5 drops.
    getCurrentWebview()
      .onDragDropEvent((ev) => onNativeDrag(ev.payload))
      .then((u) => (destroyed ? u() : (unlistenDrop = u)))
      .catch(() => {});
    [accounts, signatures, templates, settings] = await Promise.all([api.accounts(), api.signatures(), api.templates(), api.getSettings()]);
    ai = await api.aiSettings().catch(() => null);
    if (variant === "inline" || d.to.length > 0) placeCaretAtStart();
  });

  function placeCaretAtStart() {
    queueMicrotask(caretAtStart);
  }

  function caretAtStart() {
    if (!editor) return;
    editor.focus();
    const r = document.createRange();
    // Inside the first block (e.g. the empty line above the signature).
    const first = editor.firstElementChild;
    r.setStart(first && first.tagName !== "BR" ? first : editor, 0);
    r.collapse(true);
    const sel = window.getSelection();
    sel?.removeAllRanges();
    sel?.addRange(r);
  }

  /** Focus the body with the caret where it last was (a picker or menu took the focus). */
  function restoreCaret() {
    if (!editor) return;
    const sel = window.getSelection();
    if (sel && sel.rangeCount && editor.contains(sel.anchorNode)) return;
    if (lastRange && editor.contains(lastRange.startContainer)) {
      editor.focus();
      sel?.removeAllRanges();
      sel?.addRange(lastRange);
    } else {
      caretAtStart();
    }
  }

  /** The body as typed. The editor owns it; it is read only when saving, sending or rewriting. */
  function body(): string {
    const el = editor ?? editorEl;
    return el ? el.innerHTML : d.htmlBody;
  }

  /** Replace the whole body (signature, template, AI, undo). */
  function setBody(html: string) {
    d.htmlBody = html;
    const el = editor ?? editorEl;
    if (!el) return;
    el.innerHTML = html;
    directEditor(el);
    for (const t of el.querySelectorAll("table")) normalizeTable(t);
    history?.record();
  }

  /**
   * The draft as the engine gets it: quoted text marked right-to-left where it is Arabic.
   * `final`: leaving Remail (sent or saved to the server), so to-do boxes become characters.
   */
  function outgoing(final = false): Draft {
    const s = $state.snapshot(d) as Draft;
    s.htmlBody = final ? finalizeHtml(body()) : body();
    s.quotedHtml = directHtml(s.quotedHtml);
    return s;
  }

  /** Each paragraph takes the direction of what is typed in it (Arabic from the right). */
  function onEditorInput(e: Event) {
    const ie = e as InputEvent;
    if (editor && !ie.isComposing) {
      directEditor(editor);
      // New to-do items (Enter) start unticked.
      for (const li of editor.querySelectorAll<HTMLElement>("ul.todo > li")) {
        if (ie.inputType === "insertParagraph" ? li.contains(window.getSelection()?.anchorNode ?? null) : !li.dataset.checked) li.dataset.checked = "false";
      }
      // Header, lines and direction (Arabic tables run from the right).
      for (const t of editor.querySelectorAll("table")) normalizeTable(t);
      // Typing a word is one undo step; a space or a new line ends it.
      const typing = ie.inputType === "insertText" || ie.inputType === "deleteContentBackward" || ie.inputType === "deleteContentForward" || e.type === "compositionend";
      history?.record(typing);
      if (ie.inputType === "insertParagraph" || /\s/.test(ie.data ?? "")) history?.breakStep();
    }
    if (ie.inputType === "insertText" && ie.data === "/") slashArmed = true;
    updateSlash();
    touch();
  }

  function undoRedo(redo: boolean) {
    if (!history || !editor) return;
    if (!(redo ? history.redo() : history.undo())) return;
    closeSlash();
    bubble = null;
    tgrip = null;
    touch();
  }

  // Autosave locally while typing.
  function touch() {
    if (destroyed) return;
    dirty = true;
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => void saveLocal(), 1500);
  }

  $effect(() => {
    onsubject?.(d.subject);
  });

  $effect(() => {
    // Track structural edits (recipients, subject, attachments).
    void JSON.stringify([d.to, d.cc, d.bcc, d.subject, d.attachments.length, d.accountId, d.quotedHtml]);
    touch();
  });

  async function saveLocal() {
    dirty = false;
    try {
      const saved = await api.saveDraft(outgoing(), false);
      d.updatedAt = saved.updatedAt;
    } catch {
      /* autosave failures are retried on the next edit */
    }
  }

  export async function saveToServer() {
    clearTimeout(saveTimer);
    dirty = false;
    try {
      await api.saveDraft(outgoing(true), true);
      toast("Draft saved");
    } catch (e) {
      toastError(e, "Failed to save draft");
      throw e;
    }
  }

  function hasContent(): boolean {
    const text = (editor?.innerText ?? "").trim();
    const sig = usableSignatures.find((s) => s.id === d.signatureId);
    const sigText = sig ? new DOMParser().parseFromString(sig.html, "text/html").body.innerText.trim() : "";
    return d.to.length + d.cc.length + d.bcc.length > 0 || d.subject.trim() !== "" || (text !== "" && text !== sigText) || d.attachments.length > 0;
  }

  /** Called when the user closes: offer to keep the draft. */
  export async function requestClose(): Promise<boolean> {
    clearTimeout(saveTimer);
    if (!hasContent()) {
      closed = true;
      await api.deleteDraft(d.id).catch(() => {});
      onclose("discarded");
      return true;
    }
    const r = await confirmDialog({
      title: "Keep this draft?",
      message: "Save it to Drafts to finish it later.",
      confirmLabel: "Save",
      altLabel: "Discard",
    });
    if (r === "cancel") return false;
    if (r === "alt") {
      closed = true;
      await api.deleteDraft(d.id).catch(() => {});
      onclose("discarded");
      return true;
    }
    try {
      await saveToServer();
    } catch {
      return false;
    }
    closed = true;
    onclose("saved");
    return true;
  }

  async function discard() {
    if (hasContent()) {
      const r = await confirmDialog({ title: "Discard draft?", message: "It is deleted and can't be brought back.", confirmLabel: "Discard", danger: true });
      if (r !== "confirm") return;
    }
    clearTimeout(saveTimer);
    closed = true;
    await api.deleteDraft(d.id).catch(() => {});
    onclose("discarded");
  }

  async function send() {
    if (sending || aiRunning) return;
    if (d.to.length + d.cc.length + d.bcc.length === 0) {
      toast("Add at least one recipient", { kind: "error" });
      return;
    }
    if (totalSize + inlineImageBytes(body()) > MAX_TOTAL) {
      toast("Attachments can add up to 25 MB per email", { kind: "error" });
      return;
    }
    if (!d.subject.trim()) {
      const r = await confirmDialog({ title: "Send without a subject?", message: "This message has no subject.", confirmLabel: "Send anyway" });
      if (r !== "confirm") return;
    }
    if (settings?.confirmBeforeSend && !scheduleAt) {
      const who = [...d.to, ...d.cc, ...d.bcc].map((a) => a.email).join(", ");
      const r = await confirmDialog({ title: "Send email", message: `Send this email to ${who}?`, confirmLabel: "Send" });
      if (r !== "confirm") return;
    }
    sending = true;
    clearTimeout(saveTimer);
    closed = true;
    try {
      localStorage.setItem("lastComposeAccount", d.accountId);
      await api.send(outgoing(true), scheduleAt);
      if (scheduleAt) toast(`Scheduled for ${wakeDate(scheduleAt)}`);
      onclose("sent");
    } catch (e) {
      closed = false;
      toast(scheduleAt ? `Couldn't schedule this email: ${errorText(e)}` : errorText(e), { kind: "error", duration: 6000 });
    } finally {
      sending = false;
    }
  }

  // ---- signature -------------------------------------------------------

  function setSignature(sig: Signature | null) {
    menu = null;
    const doc = new DOMParser().parseFromString(`<div id="root">${body()}</div>`, "text/html");
    const root = doc.getElementById("root")!;
    const existing = root.querySelector("div.signature");
    if (sig) {
      if (existing) existing.innerHTML = sig.html;
      else root.insertAdjacentHTML("beforeend", `<br><br><div class="signature">${sig.html}</div>`);
    } else if (existing) {
      existing.remove();
    }
    setBody(root.innerHTML);
    d.signatureId = sig?.id ?? null;
    touch();
  }

  function changeAccount(id: string) {
    d.accountId = id;
    const def = signatures.find((s) => s.isDefault && s.accountId === id) ?? signatures.find((s) => s.isDefault && !s.accountId) ?? null;
    if (d.signatureId !== def?.id) setSignature(def);
    const acc = accounts.find((a) => a.id === id);
    if (acc?.autoBcc.length) {
      for (const b of acc.autoBcc) if (!d.bcc.some((x) => x.email === b)) d.bcc = [...d.bcc, { name: "", email: b }];
      showCc = true;
    }
  }

  // ---- templates -------------------------------------------------------

  async function applyTemplate(t: Template) {
    menu = null;
    const body = (editor?.innerText ?? "").trim();
    if (body) {
      const r = await confirmDialog({
        title: "Apply template?",
        message: `What you have written is replaced by "${t.name}".`,
        confirmLabel: "Apply",
      });
      if (r !== "confirm") return;
    }
    const sig = usableSignatures.find((s) => s.id === d.signatureId);
    setBody(t.html + (sig ? `<br><br><div class="signature">${sig.html}</div>` : ""));
    if (t.subject) d.subject = t.subject;
    touch();
  }

  // ---- AI ----------------------------------------------------------------

  const REWRITES: [string, string][] = [
    ["improve", "Improve writing"],
    ["fix", "Fix spelling & grammar"],
    ["shorter", "Make shorter"],
    ["longer", "Make longer"],
    ["formal", "More formal"],
    ["friendly", "More friendly"],
    ["translate:English", "Translate to English"],
    ["translate:Arabic", "Translate to Arabic"],
  ];

  function openAi(anchor: DOMRect) {
    if (!ai?.settings.enabled) {
      toast("Turn on the AI assistant in Settings", { action: { label: "Open Settings", run: () => void api.openSettings("ai") } });
      return;
    }
    const sel = window.getSelection();
    aiRange = sel && sel.rangeCount && !sel.isCollapsed && editor?.contains(sel.anchorNode) ? sel.getRangeAt(0).cloneRange() : null;
    menu = { kind: "ai", anchor };
  }

  /** Body text without the signature. */
  function editorText(): string {
    if (!editor) return "";
    let t = editor.innerText;
    const sig = (editor.querySelector(".signature") as HTMLElement | null)?.innerText ?? "";
    if (sig) {
      const i = t.lastIndexOf(sig);
      if (i >= 0) t = t.slice(0, i);
    }
    return t.trim();
  }

  /** Replace the text above the signature. */
  function setText(html: string) {
    const doc = new DOMParser().parseFromString(`<div id="r">${body()}</div>`, "text/html");
    const sig = doc.querySelector("div.signature");
    setBody(html + (sig ? `<br><br>${sig.outerHTML}` : ""));
  }

  async function runAi(mode: string | null) {
    menu = null;
    const range = aiRange;
    const selected = range ? range.toString().trim() : "";
    const before = { html: body(), subject: d.subject };
    aiRunning = true;
    aiPreview = "";
    aiRequest = newRequestId();
    try {
      const task = mode
        ? { type: "rewrite" as const, accountId: d.accountId, text: selected || editorText(), mode, instructions: "" }
        : selected
          ? { type: "rewrite" as const, accountId: d.accountId, text: selected, mode: "custom", instructions: aiPrompt }
          : { type: "compose" as const, accountId: d.accountId, instructions: aiPrompt, to: d.to, subject: d.subject, current: editorText() };
      const out = await api.aiRun(aiRequest, task, (t) => (aiPreview += t));
      if (destroyed) return;
      const html = out.html ?? "";
      if (range && selected) {
        editor?.focus();
        const sel = window.getSelection();
        sel?.removeAllRanges();
        sel?.addRange(range);
        document.execCommand("insertHTML", false, html);
        if (editor) directEditor(editor);
      } else {
        setText(html);
      }
      if (out.subject && !d.subject.trim()) d.subject = out.subject;
      aiPrompt = "";
      touch();
      toast("Done", {
        action: {
          label: "Undo",
          run: () => {
            setBody(before.html);
            d.subject = before.subject;
            touch();
          },
        },
      });
    } catch (e) {
      const msg = errorText(e);
      if (msg !== "Stopped") toast(msg, { kind: "error", duration: 6000 });
    } finally {
      aiRunning = false;
    }
  }

  // ---- formatting ------------------------------------------------------

  function exec(cmd: string, arg?: string) {
    editor?.focus();
    document.execCommand(cmd, false, arg);
    history?.record();
    touch();
  }

  /** The browser's own undo (menu, keys we didn't see) goes through ours too. */
  function onBeforeInput(e: InputEvent) {
    if (e.inputType === "historyUndo" || e.inputType === "historyRedo") {
      e.preventDefault();
      undoRedo(e.inputType === "historyRedo");
    }
  }

  // ---- tables: row and column handles --------------------------------------

  function onPointerMove(e: MouseEvent) {
    if (tmenu) return;
    const target = e.target as Element;
    if (target.closest?.(".tgrip")) return;
    const cell = target.closest?.("td, th") as HTMLTableCellElement | null;
    const pos = cell && editor?.contains(cell) ? cellPosition(cell) : null;
    if (!cell || !pos) {
      // The handles sit just outside the table: keep them while the pointer crosses over.
      if (tgrip) {
        const r = tgrip.tableRect;
        if (e.clientX > r.left - 30 && e.clientX < r.right + 30 && e.clientY > r.top - 20 && e.clientY < r.bottom) return;
      }
      tgrip = null;
      return;
    }
    if (tgrip && tgrip.table === pos.table && tgrip.row === pos.row && tgrip.col === pos.col) return;
    const tr = pos.table.rows[pos.row];
    const head = pos.table.rows[0]?.cells[pos.col] ?? cell;
    tgrip = {
      ...pos,
      rowRect: tr.getBoundingClientRect(),
      colRect: head.getBoundingClientRect(),
      tableRect: pos.table.getBoundingClientRect(),
      rtl: getComputedStyle(pos.table).direction === "rtl",
    };
  }

  function openTableMenu(part: Part, e: MouseEvent) {
    if (!tgrip) return;
    const index = part === "row" ? tgrip.row : tgrip.col;
    tmenu = {
      table: tgrip.table,
      part,
      index,
      anchor: (e.currentTarget as HTMLElement).getBoundingClientRect(),
      state: partState(tgrip.table, part, index),
    };
  }

  function onTableAction(a: TableAction) {
    const m = tmenu;
    if (!m || !editor) return;
    const cell = tableAction(m.table, m.part, m.index, a);
    if (cell) {
      editor.focus();
      const r = document.createRange();
      r.selectNodeContents(cell);
      r.collapse(true);
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(r);
    }
    history?.record();
    touch();
    tgrip = null;
    // Looks can be tried one after another; anything that moves rows or columns closes the menu.
    const stays = cell && ["background", "color", "align", "bold", "italic", "strike", "header", "clear"].includes(a.type);
    tmenu = stays ? { ...m, state: partState(m.table, m.part, m.index) } : null;
  }

  /** The block around the selection (heading, quote, code) or "". */
  function blockAt(node: Node | null): string {
    const el = node ? (node.nodeType === 1 ? (node as Element) : node.parentElement) : null;
    const b = el?.closest("h1, h2, h3, blockquote, pre");
    return b && editor?.contains(b) ? b.tagName : "";
  }

  function readFormat(): FormatState {
    const q = (c: string) => {
      try {
        return document.queryCommandState(c);
      } catch {
        return false;
      }
    };
    const sel = window.getSelection();
    const node = sel?.anchorNode ?? null;
    const el = node ? (node.nodeType === 1 ? (node as Element) : node.parentElement) : null;
    const marked = el?.closest<HTMLElement>("[style*='background-color']");
    const list = editor ? listAtCaret(editor) : null;
    return {
      bold: q("bold"),
      italic: q("italic"),
      underline: q("underline"),
      strike: q("strikeThrough"),
      highlight: !!marked && editor!.contains(marked) && !/transparent|rgba\(0, 0, 0, 0\)/.test(marked.style.backgroundColor),
      block: blockAt(node),
      list: !list ? "" : list.classList.contains("todo") ? "todo" : list.tagName === "OL" ? "ol" : "ul",
    };
  }

  function onSelectionChange() {
    if (!editor) return;
    const sel = window.getSelection();
    const inEditor = !!sel && sel.rangeCount > 0 && editor.contains(sel.anchorNode) && editor.contains(sel.focusNode);
    if (inEditor) lastRange = sel!.getRangeAt(0).cloneRange();
    if (slash) updateSlash();
    if (linking) return;
    if (!inEditor || sel!.isCollapsed || !sel!.toString().trim()) {
      bubble = null;
      return;
    }
    if (mouseSelecting) return;
    bubble = { rect: sel!.getRangeAt(0).getBoundingClientRect() };
    fmt = readFormat();
  }

  function onEditorMouseDown(e: MouseEvent) {
    const li = todoBoxAt(e);
    if (li) {
      e.preventDefault();
      toggleTodo(li);
      history?.record();
      touch();
      return;
    }
    mouseSelecting = true;
    window.addEventListener(
      "mouseup",
      () => {
        mouseSelecting = false;
        // After the click has placed the caret or finished the selection.
        setTimeout(onSelectionChange, 0);
      },
      { once: true },
    );
  }

  /** Keep the bar over the selection while the page scrolls. */
  function onScroll() {
    tgrip = null;
    if (!bubble || linking) return;
    const sel = window.getSelection();
    if (sel?.rangeCount) bubble = { rect: sel.getRangeAt(0).getBoundingClientRect() };
  }

  function toggleBlock(tag: string) {
    exec("formatBlock", blockAt(window.getSelection()?.anchorNode ?? null) === tag.toUpperCase() ? "div" : tag);
  }

  function toggleTodoList() {
    if (!editor) return;
    const list = listAtCaret(editor);
    if (list?.classList.contains("todo")) {
      exec("insertUnorderedList");
      return;
    }
    if (!list || list.tagName !== "UL") exec("insertUnorderedList");
    const ul = listAtCaret(editor);
    if (ul?.tagName === "UL") {
      ul.classList.add("todo");
      for (const li of ul.children) if (!(li as HTMLElement).dataset.checked) (li as HTMLElement).dataset.checked = "false";
    }
    history?.amend();
    touch();
  }

  /** A to-do list asked to be bullets just loses its boxes. */
  function bulletList() {
    const list = editor ? listAtCaret(editor) : null;
    if (list?.classList.contains("todo")) {
      list.classList.remove("todo");
      for (const li of list.children) li.removeAttribute("data-checked");
      history?.record();
      touch();
    } else {
      exec("insertUnorderedList");
    }
  }

  /** Run a block command or formatting from the "/" menu or the selection bar. */
  function command(id: FormatAction | string) {
    const anchor = bubble?.rect ?? (lastRange ? caretRect(lastRange) : (editor?.getBoundingClientRect() ?? new DOMRect()));
    switch (id) {
      case "h1":
      case "h2":
      case "h3":
        return toggleBlock(id);
      case "text":
        return exec("formatBlock", "div");
      case "quote":
        return toggleBlock("blockquote");
      case "code":
        return toggleBlock("pre");
      case "todo":
        return toggleTodoList();
      case "ul":
        return bulletList();
      case "ol":
        return exec("insertOrderedList");
      case "bold":
      case "italic":
      case "underline":
        return exec(id);
      case "strike":
        return exec("strikeThrough");
      case "highlight":
        exec("styleWithCSS", "true");
        exec("hiliteColor", readFormat().highlight ? "transparent" : HIGHLIGHT);
        fmt = readFormat();
        return;
      case "clear":
        return exec("removeFormat");
      case "divider":
        return exec("insertHTML", "<hr><div><br></div>");
      case "table": {
        // An Arabic line gets a right-to-left table.
        const at = window.getSelection()?.anchorNode;
        const el = at ? (at.nodeType === 1 ? (at as Element) : at.parentElement) : null;
        exec("insertHTML", newTableHtml(3, 3, el && getComputedStyle(el).direction === "rtl" ? "rtl" : "ltr"));
        if (!editor) return;
        for (const t of editor.querySelectorAll("table")) normalizeTable(t);
        enterNewTable(editor);
        history?.amend();
        return;
      }
      case "image":
        return void pickImages();
      case "attach":
        return void attach();
      case "link":
        return startLink();
      case "template":
        menu = { kind: "template", anchor };
        return;
      case "ai":
        bubble = null;
        return openAi(anchor);
    }
  }

  // ---- "/" block menu ---------------------------------------------------

  function updateSlash() {
    if (!editor || (!slashArmed && !slash)) return;
    const m = slashAtCaret(editor);
    const items = m ? filterItems(slashItems, m.query) : [];
    // Typing on past a command that doesn't exist puts the menu away.
    if (!m || (items.length === 0 && m.query.length > 3)) {
      closeSlash();
      return;
    }
    slash = { anchor: caretRect(m.range), items, active: Math.min(slash?.active ?? 0, Math.max(0, items.length - 1)) };
  }

  function closeSlash() {
    slash = null;
    slashArmed = false;
  }

  function pickSlash(item: SlashItem) {
    if (!editor) return;
    const m = slashAtCaret(editor);
    closeSlash();
    if (m) {
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(m.range);
      document.execCommand("delete");
    }
    command(item.id);
  }

  /** The menu's keys; true when the key was used. */
  function slashKey(e: KeyboardEvent): boolean {
    if (!slash) return false;
    const n = slash.items.length;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      if (n) slash.active = (slash.active + (e.key === "ArrowDown" ? 1 : n - 1)) % n;
    } else if (e.key === "Enter" || e.key === "Tab") {
      if (!n) return false;
      pickSlash(slash.items[slash.active]);
    } else if (e.key === "Escape") {
      closeSlash();
    } else {
      return false;
    }
    e.preventDefault();
    e.stopPropagation();
    return true;
  }

  // ---- links ---------------------------------------------------------------

  function startLink() {
    restoreCaret();
    const sel = window.getSelection();
    savedRange = sel && sel.rangeCount ? sel.getRangeAt(0).cloneRange() : null;
    if (!savedRange) return;
    bubble = { rect: savedRange.collapsed ? caretRect(savedRange) : savedRange.getBoundingClientRect() };
    linking = true;
  }

  function applyLink(url: string) {
    linking = false;
    editor?.focus();
    if (savedRange) {
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(savedRange);
    }
    if (!url) return onSelectionChange();
    if (!/^[a-z][a-z0-9+.-]*:/i.test(url)) url = (url.includes("@") ? "mailto:" : "https://") + url;
    if (savedRange && !savedRange.collapsed) exec("createLink", url);
    else exec("insertHTML", `<a href="${escapeHtml(url)}">${escapeHtml(url)}</a>&nbsp;`);
  }

  function onKey(e: KeyboardEvent) {
    if (slashKey(e)) return;
    const mod = e.ctrlKey || e.metaKey;
    // By key position, so Ctrl+Z, Ctrl+B… also work with the Arabic layout on.
    const letter = /^Key[A-Z]$/.test(e.code) ? e.code.slice(3).toLowerCase() : e.key.toLowerCase();
    const inBody = !!editor && editor.contains(e.target as Node);
    if (mod && e.key === "Enter") {
      e.preventDefault();
      void send();
    } else if (mod && inBody && !e.altKey && (letter === "z" || letter === "y")) {
      e.preventDefault();
      e.stopPropagation();
      undoRedo(letter === "y" || e.shiftKey);
    } else if (mod && inBody && letter === "b") {
      e.preventDefault();
      exec("bold");
    } else if (mod && inBody && letter === "i") {
      e.preventDefault();
      exec("italic");
    } else if (mod && inBody && letter === "u") {
      e.preventDefault();
      exec("underline");
    } else if (mod && letter === "k") {
      e.preventDefault();
      e.stopPropagation();
      if (editor?.contains(window.getSelection()?.anchorNode ?? null)) startLink();
    } else if (e.key === "Tab" && !mod && !e.altKey && editor && tabInTable(editor, e.shiftKey)) {
      e.preventDefault();
      history?.record();
      touch();
    } else if (e.key === "Escape" && bubble && !linking) {
      e.stopPropagation();
      bubble = null;
    } else if (e.key === "Escape" && variant === "inline") {
      e.stopPropagation();
      void requestClose();
    }
  }

  // ---- attachments -----------------------------------------------------

  async function attach() {
    try {
      const files = await api.pickAttachments();
      addAttachments(files);
    } catch (e) {
      toastError(e);
    }
  }

  function addAttachments(files: OutgoingAttachment[]) {
    const total = totalSize + files.reduce((n, f) => n + base64Size(f.dataBase64), 0);
    if (total > MAX_TOTAL) {
      toast("Attachments can add up to 25 MB per email", { kind: "error" });
      return;
    }
    d.attachments = [...d.attachments, ...files];
  }

  /** Pictures go into the body at the caret (or where they were dropped); other files are attached. */
  function addFiles(files: OutgoingAttachment[], at?: { x: number; y: number }) {
    const images = files.filter((f) => isImage(f.mimeType));
    const others = files.filter((f) => !isImage(f.mimeType));
    if (images.length) {
      const bytes = images.reduce((n, f) => n + base64Size(f.dataBase64), 0);
      if (totalSize + inlineImageBytes(body()) + bytes > MAX_TOTAL) {
        toast("Attachments can add up to 25 MB per email", { kind: "error" });
        return;
      }
      insertImages(images.map((f) => ({ src: `data:${f.mimeType};base64,${f.dataBase64}`, name: f.filename })), at);
    }
    if (others.length) addAttachments(others);
  }

  function insertImages(images: { src: string; name: string }[], at?: { x: number; y: number }) {
    if (!editor) return;
    const r = editor.getBoundingClientRect();
    const point = at && at.x >= r.left && at.x <= r.right && at.y >= r.top && at.y <= r.bottom ? document.caretRangeFromPoint(at.x, at.y) : null;
    if (point && editor.contains(point.startContainer)) {
      editor.focus();
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(point);
    } else {
      restoreCaret();
    }
    // A paragraph of their own, with a line after them to keep typing on.
    exec("insertHTML", `<div>${images.map((i) => imageHtml(i.src, i.name)).join(" ")}</div><div><br></div>`);
    sizeNewImages(editor);
    directEditor(editor);
    history?.amend();
  }

  async function pickImages() {
    try {
      addFiles(await api.pickAttachments(true));
    } catch (e) {
      toastError(e);
    }
  }

  function inComposer(x: number, y: number): boolean {
    // Hidden (the calendar is showing) or another composer's window.
    if (!root || !root.offsetParent) return false;
    const r = root.getBoundingClientRect();
    return x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
  }

  type DragPayload =
    | { type: "enter"; paths: string[]; position: { x: number; y: number } }
    | { type: "over"; position: { x: number; y: number } }
    | { type: "drop"; paths: string[]; position: { x: number; y: number } }
    | { type: "leave" };

  async function onNativeDrag(p: DragPayload) {
    if (p.type === "leave") {
      dragging = false;
      return;
    }
    // Positions come in device pixels.
    const x = p.position.x / window.devicePixelRatio;
    const y = p.position.y / window.devicePixelRatio;
    const inside = inComposer(x, y);
    if (p.type !== "drop") {
      dragging = inside;
      return;
    }
    dragging = false;
    if (!inside || p.paths.length === 0) return;
    try {
      addFiles(await api.readAttachments(p.paths), { x, y });
    } catch (e) {
      toastError(e);
    }
  }

  /** Files dropped as an HTML5 drag (the dev browser); text dragged inside the body is left to the editor. */
  async function onDrop(e: DragEvent) {
    if (!e.dataTransfer?.types.includes("Files")) return;
    e.preventDefault();
    dragging = false;
    const files = [...e.dataTransfer.files];
    const out: OutgoingAttachment[] = [];
    for (const f of files) {
      const buf = new Uint8Array(await f.arrayBuffer());
      let bin = "";
      for (let i = 0; i < buf.length; i += 0x8000) bin += String.fromCharCode(...buf.subarray(i, i + 0x8000));
      out.push({ filename: f.name, mimeType: f.type || "application/octet-stream", dataBase64: btoa(bin) });
    }
    if (out.length) addFiles(out, { x: e.clientX, y: e.clientY });
  }

  /** A pasted picture (a screenshot) goes into the body; pasted web content keeps its own images. */
  async function onPaste(e: ClipboardEvent) {
    const cd = e.clipboardData;
    if (!cd || cd.types.includes("text/html")) return;
    const files = [...cd.files].filter((f) => isImage(f.type));
    if (!files.length) return;
    e.preventDefault();
    try {
      insertImages(await Promise.all(files.map(async (f) => ({ src: await fileToDataUrl(f), name: f.name || "image" }))));
    } catch (err) {
      toastError(err);
    }
  }
</script>

<!-- The window, not the composer: the row handles sit outside its edge. -->
<svelte:window onscrollcapture={onScroll} onresize={onScroll} onmousemove={onPointerMove} />

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  bind:this={root}
  class="composer {variant}"
  role="region"
  aria-label="Compose"
  data-inline-composer={variant === "inline" ? "" : undefined}
  onkeydown={onKey}
  ondragover={(e) => {
    if (!e.dataTransfer?.types.includes("Files")) return;
    e.preventDefault();
    dragging = true;
  }}
  ondragleave={(e) => { if (e.currentTarget === e.target) dragging = false; }}
  ondrop={onDrop}
>
  <div class="toprow">
    <div class="from">
      <span class="label">From</span>
      <!-- Reads as plain text; a click offers the other addresses. -->
      {#if accounts.length > 1}
        <button class="from-addr" onclick={(e) => (fromMenu = e.currentTarget.getBoundingClientRect())}>{fromEmail}</button>
      {:else}
        <span class="from-addr static">{fromEmail}</span>
      {/if}
    </div>
    <div class="tools">
      {#if variant === "inline" && onpopout}
        <button class="icon-btn" title="Open in new window" onclick={() => onpopout?.(outgoing())}><ExternalLink size={15} /></button>
      {/if}
      <button class="icon-btn" title="Attach files" onclick={attach}><Paperclip size={15} /></button>
      <button class="icon-btn" title="Signatures" onclick={(e) => (menu = { kind: "signature", anchor: e.currentTarget.getBoundingClientRect() })}><PenLine size={15} /></button>
      <button class="icon-btn" title="Templates" onclick={(e) => (menu = { kind: "template", anchor: e.currentTarget.getBoundingClientRect() })}><FileText size={15} /></button>
      {#if ai?.settings.enabled}
        <button class="icon-btn ai-btn" title="AI assistance" disabled={aiRunning} onclick={(e) => openAi(e.currentTarget.getBoundingClientRect())}><Sparkles size={15} /></button>
      {/if}
      <button class="icon-btn" title="Schedule send" class:active={!!scheduleAt} onclick={(e) => (menu = { kind: "schedule", anchor: e.currentTarget.getBoundingClientRect() })}><Clock3 size={15} /></button>
      {#if scheduleAt}
        <span class="sched-chip"><Clock3 size={12} /> {wakeDate(scheduleAt)} <button title="Clear schedule" onclick={() => (scheduleAt = null)}><X size={11} /></button></span>
      {/if}
      <button class="icon-btn" title="Discard" disabled={aiRunning} onclick={discard}><Trash2 size={15} /></button>
      <button class="btn primary send" disabled={sending || aiRunning} onclick={send} title="Send (Ctrl+Enter)">
        {#if sending}<span class="spinner"></span>{:else}<Send size={14} />{/if}
        {scheduleAt ? (sending ? "Scheduling…" : "Schedule") : "Send"}
      </button>
    </div>
  </div>

  <RecipientInput label="To" bind:value={d.to} autofocus={variant === "window" && d.to.length === 0}>
    {#snippet trailing()}
      {#if !showCc}<button class="cc-toggle" onclick={() => (showCc = true)}>Cc/Bcc</button>{/if}
    {/snippet}
  </RecipientInput>
  {#if showCc}
    <RecipientInput label="Cc" bind:value={d.cc} />
    <RecipientInput label="Bcc" bind:value={d.bcc} />
  {/if}
  <input class="subject" placeholder="Subject" dir={subjectDir(d.subject)} bind:value={d.subject} spellcheck="true" />

  {#if aiRunning}
    <div class="ai-progress">
      <Sparkles size={13} />
      <span class="ai-text" dir="auto">{aiPreview ? aiPreview.slice(-140) : "Writing…"}</span>
      <button class="btn" onclick={() => api.aiCancel(aiRequest)}><Square size={11} /> Stop</button>
    </div>
  {/if}

  <div
    class="editor selectable"
    class:ai-busy={aiRunning}
    contenteditable="true"
    role="textbox"
    aria-multiline="true"
    tabindex="0"
    spellcheck="true"
    bind:this={editor}
    oninput={onEditorInput}
    oncompositionend={onEditorInput}
    onmousedown={onEditorMouseDown}
    onbeforeinput={onBeforeInput}
    onpaste={onPaste}
    onblur={closeSlash}
    data-placeholder="Your message here… Type / for headings, lists, tables and more"
  ></div>

  {#if d.quotedHtml}
    <div class="quoted">
      <button class="quote-btn" title={showQuote ? "Hide quoted text" : "Show quoted text"} onclick={() => (showQuote = !showQuote)}>•••</button>
      {#if showQuote}
        <button class="link-btn" onclick={() => { d.quotedHtml = ""; showQuote = false; }}>Remove quoted text</button>
        <div class="quote-preview"><EmailFrame html={d.quotedHtml} text={null} collapseQuotes={false} /></div>
      {/if}
    </div>
  {/if}

  {#if d.attachments.length > 0}
    <div class="attachments">
      {#each d.attachments as a, i (i)}
        <span class="att">
          <Paperclip size={12} />
          <span class="att-name">{a.filename}</span>
          <span class="att-size">{fileSize(base64Size(a.dataBase64))}</span>
          <button title="Remove" onclick={() => (d.attachments = d.attachments.filter((_, j) => j !== i))}><X size={11} /></button>
        </span>
      {/each}
      <span class="total">{fileSize(totalSize)} / 25 MB</span>
    </div>
  {/if}

  {#if dragging}
    <div class="drop">Drop photos to put them in the message, other files to attach them</div>
  {/if}
</div>

{#if slash}
  <SlashMenu anchor={slash.anchor} items={slash.items} active={slash.active} onpick={pickSlash} onhover={(i) => slash && (slash.active = i)} />
{/if}
{#if tgrip && !tmenu}
  <!-- Row handle on the table's start side, column handle on its top edge. -->
  <button
    class="tgrip row-grip"
    title="Row options"
    aria-label="Row options"
    style:left="{tgrip.rtl ? tgrip.tableRect.right + 4 : tgrip.tableRect.left - 22}px"
    style:top="{tgrip.rowRect.top + tgrip.rowRect.height / 2 - 11}px"
    onmousedown={(e) => e.preventDefault()}
    onclick={(e) => openTableMenu("row", e)}><GripVertical size={13} /></button
  >
  <button
    class="tgrip col-grip"
    title="Column options"
    aria-label="Column options"
    style:left="{tgrip.colRect.left + tgrip.colRect.width / 2 - 11}px"
    style:top="{tgrip.tableRect.top - 9}px"
    onmousedown={(e) => e.preventDefault()}
    onclick={(e) => openTableMenu("col", e)}><GripHorizontal size={13} /></button
  >
{/if}
{#if tmenu}
  <TableMenu anchor={tmenu.anchor} part={tmenu.part} state={tmenu.state} rtl={tmenu.table.getAttribute("dir") === "rtl"} onaction={onTableAction} onclose={() => (tmenu = null)} />
{/if}
{#if bubble && !slash && !menu}
  <BubbleToolbar
    rect={bubble.rect}
    state={fmt}
    ai={!!ai?.settings.enabled}
    {linking}
    onaction={command}
    onstartlink={startLink}
    onlink={applyLink}
  />
{/if}

{#if fromMenu}
  <Popover anchor={fromMenu} width={280} height={Math.min(320, accounts.length * 32 + 12)} align="left" onclose={() => (fromMenu = null)}>
    {#each accounts as a (a.id)}
      <button class="menu-item from-item" class:selected={a.id === d.accountId} onclick={() => { fromMenu = null; if (a.id !== d.accountId) changeAccount(a.id); }}>{a.email}</button>
    {/each}
  </Popover>
{/if}

{#if menu?.kind === "ai"}
  <Popover anchor={menu.anchor} width={300} height={400} onclose={() => (menu = null)}>
    <div class="ai-box">
      <!-- svelte-ignore a11y_autofocus -->
      <input
        class="input"
        autofocus
        dir="auto"
        placeholder={aiRange ? "How should AI change the selection?" : editorText() ? "Tell AI what to change or write…" : "Tell AI what to write…"}
        bind:value={aiPrompt}
        onkeydown={(e) => {
          if (e.key === "Enter" && aiPrompt.trim()) { e.preventDefault(); void runAi(null); }
        }}
      />
      <button class="btn primary" disabled={!aiPrompt.trim()} onclick={() => runAi(null)}>Write</button>
    </div>
    {#if editorText() || aiRange}
      <div class="menu-title">{aiRange ? "Selected text" : "Whole message"}</div>
      {#each REWRITES as [mode, label] (mode)}
        <button class="menu-item" onclick={() => runAi(mode)}>{label}</button>
      {/each}
    {/if}
  </Popover>
{:else if menu?.kind === "schedule"}
  <SendLaterMenu anchor={menu.anchor} onclose={() => (menu = null)} onpick={(at) => { menu = null; scheduleAt = at; }} />
{:else if menu?.kind === "signature"}
  <Popover anchor={menu.anchor} width={240} height={220} onclose={() => (menu = null)}>
    <div class="menu-title">Signature</div>
    <button class="menu-item" class:selected={!d.signatureId} onclick={() => setSignature(null)}>No signature</button>
    {#each usableSignatures as s (s.id)}
      <button class="menu-item" class:selected={d.signatureId === s.id} onclick={() => setSignature(s)}>{s.name}{#if s.isDefault}<span class="detail">default</span>{/if}</button>
    {/each}
    <div class="menu-sep"></div>
    <button class="menu-item" onclick={() => { menu = null; api.openSettings("signatures"); }}>Manage signatures…</button>
  </Popover>
{:else if menu?.kind === "template"}
  <Popover anchor={menu.anchor} width={240} height={220} onclose={() => (menu = null)}>
    <div class="menu-title">Templates</div>
    {#each templates as t (t.id)}
      <button class="menu-item" onclick={() => applyTemplate(t)}>{t.name}</button>
    {:else}
      <div class="none">No templates yet</div>
    {/each}
    <div class="menu-sep"></div>
    <button class="menu-item" onclick={() => { menu = null; api.openSettings("templates"); }}>Manage templates…</button>
  </Popover>
{/if}

<style>
  .composer {
    position: relative;
    display: flex;
    flex-direction: column;
    background: var(--surface-overlay);
  }
  .composer.inline {
    border: 0.5px solid var(--separator);
    border-radius: 12px;
    padding: 6px 16px 12px;
    box-shadow: var(--shadow-panel);
    margin-top: 8px;
  }
  .toprow {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 0;
    border-bottom: 1px solid var(--separator-subtle);
  }
  .from {
    display: flex;
    align-items: center;
    gap: 10px;
    flex: 1;
    min-width: 0;
  }
  .label {
    width: 34px;
    color: var(--content-subtle);
    flex: none;
  }
  .from-addr {
    border: none;
    background: none;
    padding: 4px 0;
    font: inherit;
    font-size: 13px;
    color: var(--content);
    text-align: start;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
  }
  .from-addr.static {
    cursor: default;
  }
  .from-item {
    font-size: 13px;
    padding: 6px 8px;
  }
  .tools {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .send {
    margin-left: 6px;
  }
  .sched-chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 11.5px;
    padding: 2px 4px 2px 8px;
    border-radius: 999px;
    background: var(--accent-soft);
    color: var(--accent);
  }
  .sched-chip button {
    border: none;
    background: none;
    display: flex;
    color: inherit;
    padding: 2px;
  }
  .cc-toggle {
    border: none;
    background: none;
    color: var(--content-subtle);
    font-size: 12px;
    padding: 4px;
  }
  .cc-toggle:hover {
    color: var(--content);
  }
  .subject {
    border: none;
    outline: none;
    background: none;
    font-size: 20px;
    line-height: 28px;
    font-weight: 600;
    padding: 14px 0 8px;
    user-select: text;
  }
  .inline .subject {
    font-size: 14px;
    font-weight: 500;
    padding: 10px 0 4px;
  }
  .editor {
    min-height: 160px;
    outline: none;
    font-size: 14px;
    line-height: 1.45;
    padding: 4px 0 12px;
    overflow-wrap: anywhere;
  }
  .inline .editor {
    min-height: 110px;
  }
  .editor:empty::before {
    content: attr(data-placeholder);
    color: var(--content-muted);
  }
  .editor :global(blockquote) {
    margin: 0;
    margin-inline-start: 0.8ex;
    border-inline-start: 2px solid var(--separator);
    padding-inline-start: 1ex;
    color: var(--content-subtle);
  }
  /* Pasted text keeps its font in the email, but shows in ours here (with the Arabic fix). */
  .editor :global([style*="font-family"]) {
    font-family: var(--font-family) !important;
  }
  .editor :global(h1) {
    font-size: 24px;
    line-height: 1.25;
    margin: 10px 0 4px;
  }
  .editor :global(h2) {
    font-size: 19px;
    line-height: 1.3;
    margin: 8px 0 4px;
  }
  .editor :global(h3) {
    font-size: 16px;
    line-height: 1.35;
    margin: 6px 0 2px;
  }
  .editor :global(pre) {
    font-family: ui-monospace, "SFMono-Regular", Menlo, monospace;
    font-size: 12.5px;
    background: var(--surface-hover);
    border-radius: 6px;
    padding: 8px 10px;
    white-space: pre-wrap;
  }
  .editor :global(hr) {
    border: none;
    border-top: 1px solid var(--separator);
    margin: 10px 0;
  }
  .editor :global(img) {
    max-width: 100%;
    height: auto;
    border-radius: 4px;
  }
  /* Lines in the theme's colour while writing; the email keeps its own. */
  .editor :global(table),
  .editor :global(td),
  .editor :global(th) {
    border-color: var(--separator) !important;
  }
  .tgrip {
    position: fixed;
    z-index: 340;
    width: 22px;
    height: 22px;
    padding: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: 6px;
    border: 0.5px solid var(--separator);
    background: var(--surface-elevated);
    color: var(--icon);
    box-shadow: var(--shadow-panel);
  }
  .tgrip:hover {
    color: var(--icon-hover);
    background: var(--surface-elevated-hover);
  }
  .col-grip {
    height: 18px;
  }
  /* Highlighted text keeps dark letters on its light marker in the dark theme too. */
  .editor :global([style*="background-color"]:not([style*="transparent"]):not([style*="rgba(0, 0, 0, 0)"])) {
    color: #1d1d1f;
  }
  .editor :global(ul.todo) {
    list-style: none;
    padding-inline-start: 2px;
  }
  .editor :global(ul.todo > li) {
    position: relative;
    padding-inline-start: 26px;
    margin: 2px 0;
  }
  .editor :global(ul.todo > li::before) {
    content: "";
    position: absolute;
    inset-inline-start: 2px;
    top: 2px;
    width: 14px;
    height: 14px;
    border: 1.5px solid var(--content-muted);
    border-radius: 4px;
    cursor: pointer;
  }
  .editor :global(ul.todo > li[data-checked="true"]) {
    color: var(--content-subtle);
    text-decoration: line-through;
  }
  .editor :global(ul.todo > li[data-checked="true"]::before) {
    border-color: var(--accent);
    background: var(--accent)
      url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'%3E%3Cpath d='M3.5 8.5l3 3 6-7' fill='none' stroke='white' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E")
      center / 12px no-repeat;
  }
  .editor :global(.signature) {
    color: var(--content-subtle);
  }
  .editor :global(a) {
    color: var(--accent);
  }
  .quoted {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 6px;
  }
  .quote-btn {
    border: 0.5px solid var(--separator);
    background: var(--surface-hover);
    border-radius: 6px;
    padding: 0 8px;
    font-size: 11px;
    letter-spacing: 1px;
    color: var(--content-subtle);
  }
  .quote-preview {
    width: 100%;
    opacity: 0.85;
    pointer-events: none;
  }
  .link-btn {
    border: none;
    background: none;
    color: var(--accent);
    font-size: 12px;
    padding: 0;
  }
  .attachments {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding-top: 10px;
  }
  .att {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 4px 4px 8px;
    border-radius: 8px;
    border: 0.5px solid var(--separator);
    font-size: 12px;
  }
  .att-name {
    max-width: 200px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .att-size,
  .total {
    color: var(--content-subtle);
    font-size: 11px;
  }
  .att button {
    border: none;
    background: none;
    display: flex;
    padding: 2px;
    color: var(--icon);
  }
  .drop {
    position: absolute;
    inset: 8px;
    border: 2px dashed var(--interactive);
    border-radius: 8px;
    background: color-mix(in srgb, var(--surface-overlay) 85%, transparent);
    display: flex;
    align-items: center;
    justify-content: center;
    font-weight: 500;
    color: var(--interactive);
    pointer-events: none;
  }
  .none {
    padding: 6px 8px;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .ai-btn {
    color: var(--accent);
  }
  .ai-box {
    display: flex;
    gap: 6px;
    padding: 4px 4px 8px;
  }
  .ai-box .input {
    flex: 1;
    min-width: 0;
    height: 30px;
  }
  .ai-progress {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    margin-bottom: 6px;
    border-radius: 8px;
    background: var(--accent-soft);
    color: var(--accent);
    font-size: 12px;
  }
  .ai-text {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--content-subtle);
  }
  .editor.ai-busy {
    opacity: 0.55;
    pointer-events: none;
  }
</style>
