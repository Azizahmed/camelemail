// Minimal, safe Markdown → HTML for assistant output (everything is escaped
// first; only http(s)/mailto links survive).

import { api } from "./api";
import { textDirection } from "./bidi";

// NUL marks placeholders in `inline`, so input can't contain it.
const esc = (s: string) => s.replace(/\u0000/g, "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

function inline(s: string): string {
  // Finished pieces (code, links) are parked as placeholders so later
  // patterns can never match inside the HTML they produce.
  const parked: string[] = [];
  const park = (html: string) => `\u0000${parked.push(html) - 1}\u0000`;
  const t = esc(s)
    .replace(/`([^`]+)`/g, (_, c) => park(`<code>${c}</code>`))
    .replace(/\[([^\]\u0000]+)\]\(((?:https?:\/\/|mailto:)[^\s)"<\u0000]+)\)/g, (_, text, url) => park(`<a href="${url}">${text}</a>`))
    .replace(/(^|[\s(])(https?:\/\/[^\s<)"\u0000]+[^\s<).,;:!?"\u0000])/g, (_, pre, url) => pre + park(`<a href="${url}">${url}</a>`))
    .replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>")
    .replace(/(^|[^*\w])\*([^*\s][^*]*)\*/g, "$1<i>$2</i>")
    .replace(/(^|\W)_([^_\s][^_]*)_(?=\W|$)/g, "$1<i>$2</i>");
  let out = t;
  // Links may contain parked code; restore until nothing is left.
  for (let i = 0; i < 3 && out.includes("\u0000"); i++) out = out.replace(/\u0000(\d+)\u0000/g, (_, n) => parked[Number(n)] ?? "");
  return out;
}

export function renderMarkdown(src: string): string {
  const out: string[] = [];
  let para: string[] = [];
  let list: "ul" | "ol" | null = null;
  let code: string[] | null = null;

  const flushPara = () => {
    if (para.length) out.push(`<p dir="auto">${para.map(inline).join("<br>")}</p>`);
    para = [];
  };
  const closeList = () => {
    if (list) out.push(`</${list}>`);
    list = null;
  };

  for (const line of src.replace(/\r/g, "").split("\n")) {
    if (code !== null) {
      if (line.trim().startsWith("```")) {
        out.push(`<pre><code>${esc(code.join("\n"))}</code></pre>`);
        code = null;
      } else code.push(line);
      continue;
    }
    if (line.trim().startsWith("```")) {
      flushPara();
      closeList();
      code = [];
      continue;
    }
    const h = /^(#{1,4})\s+(.*)$/.exec(line);
    const ul = /^\s*[-*•]\s+(.*)$/.exec(line);
    const ol = /^\s*\d+[.)]\s+(.*)$/.exec(line);
    if (h) {
      flushPara();
      closeList();
      out.push(`<h4 dir="auto">${inline(h[2])}</h4>`);
    } else if (ul || ol) {
      flushPara();
      const kind = ul ? "ul" : "ol";
      if (list !== kind) {
        closeList();
        // Not dir="auto": that skips the items (they carry their own dir), so an
        // Arabic list would come out left-to-right with its bullets outside on the right.
        out.push(`<${kind} dir="${textDirection((ul ?? ol)![1]) ?? "auto"}">`);
        list = kind;
      }
      out.push(`<li dir="auto">${inline((ul ?? ol)![1])}</li>`);
    } else if (!line.trim()) {
      flushPara();
      closeList();
    } else {
      closeList();
      para.push(line.trim());
    }
  }
  if (code !== null) out.push(`<pre><code>${esc((code as string[]).join("\n"))}</code></pre>`);
  flushPara();
  closeList();
  return out.join("");
}

/** Click handler for containers with rendered Markdown: open links in the browser. */
export function openLinks(e: MouseEvent) {
  const a = (e.target as HTMLElement).closest("a");
  if (!a) return;
  e.preventDefault();
  const href = a.getAttribute("href");
  if (href) void api.openUrl(href);
}
