// E2E for the pii:redact component, run in-process via jco. Pure-compute: no
// host shims needed. Covers detect/redact/mask across email, credit-card (Luhn),
// ssn, phone, ip, plus the kinds filter. Assertions favour kind membership and
// redacted/masked substrings over fragile exact offsets.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { redactor as pii } from "../gen/pii_redact.js";

// Empty kinds == detect/redact every supported kind.
const ALL = { kinds: [] as string[] };

describe("pii:redact component", () => {
  it("detect finds email and phone, with correct email offsets", () => {
    const text = "Email me at john@example.com or call 555-123-4567.";
    const findings = pii.detect(text, ALL);
    const kinds = findings.map((f) => f.kind);
    assert.ok(kinds.includes("email"), "expected an email finding");
    assert.ok(kinds.includes("phone"), "expected a phone finding");

    const email = findings.find((f) => f.kind === "email");
    assert.ok(email);
    assert.equal(text.slice(email.start, email.start + email.length), "john@example.com");
  });

  it("redact replaces an email with the [EMAIL] token", () => {
    assert.equal(pii.redact("Contact john@example.com", ALL), "Contact [EMAIL]");
  });

  it("detects/redacts a Luhn-valid credit card, ignores a Luhn-invalid run", () => {
    // 4242 4242 4242 4242 is the canonical Luhn-valid Visa test card.
    const good = "card 4242 4242 4242 4242 end";
    assert.ok(
      pii.detect(good, ALL).some((f) => f.kind === "credit-card"),
      "expected a credit-card finding for the Luhn-valid number",
    );
    assert.equal(pii.redact(good, ALL), "card [CARD] end");

    // 1234 5678 9012 3456 is not Luhn-valid -> must not be flagged.
    const bad = "num 1234 5678 9012 3456 end";
    assert.ok(
      !pii.detect(bad, ALL).some((f) => f.kind === "credit-card"),
      "Luhn-invalid number must not be a credit-card finding",
    );
    assert.equal(pii.redact(bad, ALL), bad);
  });

  it("detects/redacts an SSN", () => {
    assert.ok(pii.detect("SSN 123-45-6789", ALL).some((f) => f.kind === "ssn"));
    assert.equal(pii.redact("SSN 123-45-6789", ALL), "SSN [SSN]");
  });

  it("detects/redacts an IP address", () => {
    assert.ok(pii.detect("from 192.168.1.1 here", ALL).some((f) => f.kind === "ip"));
    assert.equal(pii.redact("from 192.168.1.1 here", ALL), "from [IP] here");
  });

  it("mask partially obscures an email but keeps shape", () => {
    const masked = pii.mask("john@example.com", ALL);
    assert.notEqual(masked, "john@example.com", "must not be the raw email");
    assert.notEqual(masked, "[EMAIL]", "mask is partial, not full redaction");
    assert.ok(masked.startsWith("j"), "keeps the first character");
    assert.ok(masked.includes("@"), "keeps the @ separator");
    assert.ok(masked.includes("***"), "obscures with ***");
    assert.ok(masked.endsWith(".com"), "keeps the TLD");
  });

  it("mask keeps the last 4 digits of a credit card", () => {
    const masked = pii.mask("4242 4242 4242 4242", ALL);
    assert.notEqual(masked, "4242 4242 4242 4242", "must not be the raw card");
    assert.ok(masked.includes("***") || masked.includes("****"), "obscures earlier digits");
    assert.ok(masked.endsWith("4242"), "last 4 digits survive");
  });

  it("kinds filter restricts detection to the requested kind", () => {
    const findings = pii.detect("john@example.com 192.168.1.1", { kinds: ["ip"] });
    const kinds = findings.map((f) => f.kind);
    assert.ok(kinds.includes("ip"), "expected the ip finding");
    assert.ok(!kinds.includes("email"), "email must be excluded by the filter");
  });
});
