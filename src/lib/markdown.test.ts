import { describe, expect, it, vi } from "vitest";

vi.mock("./api", () => ({ api: { openUrl: vi.fn() } }));
const { renderMarkdown } = await import("./markdown");

describe("renderMarkdown", () => {
  it("escapes raw HTML", () => {
    expect(renderMarkdown("<script>alert(1)</script>")).toBe('<p dir="auto">&lt;script&gt;alert(1)&lt;/script&gt;</p>');
    const img = renderMarkdown('<img src=x onerror="alert(1)">');
    expect(img).not.toContain("<img");
    expect(img).toContain("&lt;img src=x onerror=&quot;alert(1)&quot;&gt;");
  });

  it("only links http(s) and mailto", () => {
    expect(renderMarkdown("[ok](https://example.com/a?b=1&c=2)")).toBe('<p dir="auto"><a href="https://example.com/a?b=1&amp;c=2">ok</a></p>');
    expect(renderMarkdown("[mail](mailto:a@b.c)")).toContain('<a href="mailto:a@b.c">mail</a>');
    for (const bad of ["[x](javascript:alert(1))", "[x](data:text/html,<b>)", "[x](vbscript:msgbox)"]) {
      expect(renderMarkdown(bad)).not.toContain("<a");
    }
  });

  it("can't break out of a link's href", () => {
    const out = renderMarkdown('[x](https://e.com/"onmouseover="alert(1))');
    // The quote stays escaped inside the href, so the tag has no other attribute.
    expect(out).toMatch(/^<p dir="auto"><a href="[^"]*">x<\/a>/);
    expect(out).not.toContain('"onmouseover');
  });

  it("auto-links bare URLs without trailing punctuation", () => {
    expect(renderMarkdown("See https://example.com/x.")).toBe('<p dir="auto">See <a href="https://example.com/x">https://example.com/x</a>.</p>');
  });

  it("doesn't format inside code or links", () => {
    expect(renderMarkdown("`**a**`")).toBe('<p dir="auto"><code>**a**</code></p>');
    expect(renderMarkdown("https://e.com/a_b_c")).toBe('<p dir="auto"><a href="https://e.com/a_b_c">https://e.com/a_b_c</a></p>');
  });

  it("renders emphasis, headings, lists and code blocks", () => {
    expect(renderMarkdown("**bold** and *it* and _it_")).toBe('<p dir="auto"><b>bold</b> and <i>it</i> and <i>it</i></p>');
    expect(renderMarkdown("## Title")).toBe('<h4 dir="auto">Title</h4>');
    expect(renderMarkdown("- a\n- b\n\n1. c")).toBe('<ul dir="ltr"><li dir="auto">a</li><li dir="auto">b</li></ul><ol dir="ltr"><li dir="auto">c</li></ol>');
    expect(renderMarkdown("```\n<b>x</b>\n```")).toBe("<pre><code>&lt;b&gt;x&lt;/b&gt;</code></pre>");
    expect(renderMarkdown("```\nunclosed")).toBe("<pre><code>unclosed</code></pre>");
  });

  it("joins paragraph lines with <br>", () => {
    expect(renderMarkdown("one\ntwo\r\n\r\nthree")).toBe('<p dir="auto">one<br>two</p><p dir="auto">three</p>');
  });

  it("keeps Arabic text and lets each block pick its direction", () => {
    expect(renderMarkdown("**مرحباً** بك")).toBe('<p dir="auto"><b>مرحباً</b> بك</p>');
  });
});

describe("renderMarkdown hostile input", () => {
  it("ignores case tricks and images", () => {
    expect(renderMarkdown("[x](JAVASCRIPT:alert(1))")).not.toContain("<a");
    expect(renderMarkdown("![i](https://t.example/p.png)")).not.toContain("<img");
  });

  it("keeps escaped text inside hrefs and code", () => {
    expect(renderMarkdown("[a](https://x.com/<script>)")).not.toContain("<script>");
    expect(renderMarkdown("`<img src=x onerror=1>`")).toBe('<p dir="auto"><code>&lt;img src=x onerror=1&gt;</code></p>');
  });

  it("can't refer to its own placeholders", () => {
    const out = renderMarkdown("`<b>` \u00000\u0000 [x](https://e.com) \u00001\u0000");
    expect(out).not.toMatch(/<b>|<code>[^<]*<\/code>[^<]*<code>/);
    expect(out.match(/<code>/g)?.length).toBe(1);
    expect(out.match(/<a /g)?.length).toBe(1);
  });

  it("gives an Arabic list a right-to-left direction of its own", () => {
    expect(renderMarkdown("- أرسل الملف\n- ok")).toContain('<ul dir="rtl">');
  });
});
