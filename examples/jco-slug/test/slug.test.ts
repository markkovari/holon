// E2E for the slug:generate component, run in-process via jco. Pure compute -
// no host shims, the component imports only standard WASI which preview2-shim
// satisfies. Covers slugify (transliteration, run-collapsing), slugifyWith
// (custom separator, word-boundary truncation) and uniquify (collision suffix).

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { generator as slug } from "../gen/slug.js";

describe("slug:generate component", () => {
  it("slugify lowercases and hyphenates", () => {
    assert.equal(slug.slugify("Hello, World!"), "hello-world");
  });

  it("slugify transliterates accents to ascii", () => {
    const out = slug.slugify("Café déjà vu");
    // Tolerant: must be lowercase ascii, hyphen-separated, no accents.
    assert.match(out, /^[a-z0-9]+(?:-[a-z0-9]+)*$/);
    assert.equal(out, "cafe-deja-vu"); // observed deterministic output
  });

  it("slugify collapses runs of separators", () => {
    assert.equal(slug.slugify("a   b---c"), "a-b-c");
  });

  it("slugifyWith honours a custom separator", () => {
    assert.equal(
      slug.slugifyWith("a b c", { separator: "_", maxLength: 0 }),
      "a_b_c",
    );
  });

  it("slugifyWith truncates on a word boundary", () => {
    // "one-two" is 7 chars; with maxLength 8 the next boundary fits "one-two"
    // but not "one-two-three", so it stops cleanly at the word boundary.
    assert.equal(
      slug.slugifyWith("one two three four", { separator: "-", maxLength: 8 }),
      "one-two",
    );
  });

  it("uniquify appends an incrementing suffix on collision", () => {
    assert.equal(slug.uniquify("post", ["post"]), "post-2");
    assert.equal(slug.uniquify("post", ["post", "post-2"]), "post-3");
  });

  it("uniquify returns the desired slug when free", () => {
    assert.equal(slug.uniquify("fresh", []), "fresh");
  });
});
