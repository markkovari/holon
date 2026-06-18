// E2E for the i18n:catalog component, run in-process via jco (in-memory kv shim
// + config shim). Covers message interpolation, base-language fallback,
// default-locale fallback, missing-message errors, pluralization, and locale
// negotiation.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { catalog as i18n } from "../gen/i18n_catalog.js";

describe("i18n:catalog component", () => {
  it("interpolates a stored message", () => {
    i18n.setMessage("en", "greeting", "Hello, {name}!");
    assert.equal(
      i18n.translate("en", "greeting", [{ name: "name", value: "Al" }]),
      "Hello, Al!",
    );
  });

  it("falls back to the base language (en-US -> en)", () => {
    i18n.setMessage("en", "x", "EN");
    assert.equal(i18n.translate("en-US", "x", []), "EN");
  });

  it("falls back to the configured default-locale", () => {
    // "greeting" exists only under "en"; "fr" has no entry, so it falls back to
    // the default-locale ("en" from config-shim).
    assert.equal(
      i18n.translate("fr", "greeting", [{ name: "name", value: "Z" }]),
      "Hello, Z!",
    );
  });

  it("throws missing-message for an unknown key", () => {
    assert.throws(
      () => i18n.translate("en", "nope", []),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "missing-message",
    );
  });

  it("pluralizes with auto-injected count", () => {
    i18n.setPlural("en", "items", [
      ["one", "{count} item"],
      ["other", "{count} items"],
    ]);
    assert.equal(i18n.translatePlural("en", "items", 1n, []), "1 item");
    assert.equal(i18n.translatePlural("en", "items", 5n, []), "5 items");
  });

  it("negotiates the best available locale", () => {
    assert.equal(i18n.negotiate(["fr-CA", "fr", "en"], ["en", "fr"]), "fr");
    // "de" not available -> default-locale ("en") which is in the available set.
    assert.equal(i18n.negotiate(["de"], ["en", "fr"]), "en");
  });
});
