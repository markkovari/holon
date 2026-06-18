// E2E for the email:template component, run in-process via jco (in-memory kv
// shim). Covers template store + round-trip, placeholder rendering, the
// HTML-escape-vs-text-raw split, and the two render errors.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { renderer as email } from "../gen/email_render.js";

describe("email:template component", () => {
  it("putTemplate then getTemplate round-trips all three fields", () => {
    email.putTemplate("welcome", {
      subject: "Welcome {name}",
      text: "Hi {name}, code {code}",
      html: "<p>Hi {name}</p>",
    });
    const t = email.getTemplate("welcome");
    assert.equal(t.subject, "Welcome {name}");
    assert.equal(t.text, "Hi {name}, code {code}");
    assert.equal(t.html, "<p>Hi {name}</p>");
  });

  it("render substitutes placeholders across subject, text, and html", () => {
    const m = email.render("welcome", [
      { name: "name", value: "Al" },
      { name: "code", value: "123" },
    ]);
    assert.equal(m.subject, "Welcome Al");
    assert.equal(m.text, "Hi Al, code 123");
    assert.equal(m.html, "<p>Hi Al</p>");
  });

  it("HTML placeholder values are escaped; text values stay raw", () => {
    email.putTemplate("x", {
      subject: "S",
      text: "T {v}",
      html: "<b>{v}</b>",
    });
    const m = email.render("x", [{ name: "v", value: '<script>&"' }]);
    // html: dangerous chars HTML-escaped to prevent injection.
    assert.ok(m.html.includes("&lt;"), `html should escape <: ${m.html}`);
    assert.ok(m.html.includes("&amp;"), `html should escape &: ${m.html}`);
    assert.ok(m.html.includes("&quot;"), `html should escape ": ${m.html}`);
    assert.ok(!m.html.includes("<script>"), `html must not contain raw tag: ${m.html}`);
    // text: left verbatim — the sender treats it as plain text.
    assert.equal(m.text, 'T <script>&"');
    assert.ok(m.text.includes("<script>"), `text should stay raw: ${m.text}`);
  });

  it("render throws missing-variable when a placeholder has no binding", () => {
    // "welcome" needs {name} and {code}; supply only {name}.
    assert.throws(
      () => email.render("welcome", [{ name: "name", value: "Al" }]),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "missing-variable",
    );
  });

  it("getTemplate throws unknown-template for an unstored name", () => {
    assert.throws(
      () => email.getTemplate("nope"),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "unknown-template",
    );
  });
});
