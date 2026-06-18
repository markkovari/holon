// E2E for the md:render component, run in-process via jco (pure-compute, no
// shims). The renderer is a CommonMark SUBSET and the exact HTML may vary, so
// these assertions check for KEY substrings / containment rather than exact
// full-document equality. The two SAFETY tests below are security-critical:
// raw HTML must be escaped, and dangerous link schemes (javascript:, data:)
// must be dropped.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { renderer as md } from "../gen/markdown.js";

describe("md:render component", () => {
  it("renders a heading", () => {
    const out = md.toHtml("# Hello");
    assert.ok(out.includes("<h1>"), out);
    assert.ok(out.includes("Hello"), out);
    assert.ok(out.includes("</h1>"), out);
  });

  it("renders bold and italic", () => {
    const out = md.toHtml("**bold** and *italic*");
    assert.ok(out.includes("<strong>bold</strong>"), out);
    assert.ok(out.includes("<em>italic</em>"), out);
  });

  it("renders inline code", () => {
    const out = md.toHtml("use `code` here");
    assert.ok(out.includes("<code>code</code>"), out);
  });

  it("renders a safe link", () => {
    const out = md.toHtml("[site](https://example.com)");
    assert.ok(out.includes('href="https://example.com"'), out);
    assert.ok(out.includes(">site<"), out);
  });

  // CRITICAL SAFETY 1: raw embedded HTML must be escaped, never passed through.
  it("escapes raw HTML (no live <script>)", () => {
    const out = md.toHtml("<script>alert(1)</script>");
    assert.ok(!out.includes("<script>"), `raw <script> leaked: ${out}`);
    assert.ok(out.includes("&lt;script&gt;"), out);
  });

  // CRITICAL SAFETY 2: javascript: link scheme must be neutralized.
  it("neutralizes javascript: links", () => {
    const out = md.toHtml("[click](javascript:alert(1))");
    assert.ok(!out.includes("javascript:"), `dangerous scheme leaked: ${out}`);
  });

  // CRITICAL SAFETY 3: data: scheme must also be dropped.
  it("drops data: link schemes", () => {
    const out = md.toHtml("[x](data:text/html,<script>)");
    assert.ok(!out.includes("data:text/html"), `dangerous scheme leaked: ${out}`);
  });

  it("renders a code fence with escaped content", () => {
    const out = md.toHtml("```\nlet x = 1;\n```");
    assert.ok(out.includes("<pre>"), out);
    assert.ok(out.includes("<code>"), out);
    assert.ok(out.includes("let x = 1;"), out);
  });

  it("renders an unordered list", () => {
    const out = md.toHtml("- one\n- two");
    assert.ok(out.includes("<ul>"), out);
    assert.ok(out.includes("<li>one</li>"), out);
  });

  it("strips formatting in toText", () => {
    const out = md.toText("# Title\n\n**bold** text");
    assert.ok(out.includes("Title"), out);
    assert.ok(out.includes("bold"), out);
    assert.ok(!out.includes("<"), `markup leaked into text: ${out}`);
    assert.ok(!out.includes("#"), `markup leaked into text: ${out}`);
    assert.ok(!out.includes("*"), `markup leaked into text: ${out}`);
  });

  it("applies safeLinks option (nofollow + _blank)", () => {
    const out = md.toHtmlWith("[s](https://x.com)", {
      hardBreaks: false,
      safeLinks: true,
    });
    assert.ok(out.includes('rel="nofollow'), out);
    assert.ok(out.includes('target="_blank"'), out);
  });
});
