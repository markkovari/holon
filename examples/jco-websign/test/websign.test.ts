// E2E for the webhook:sign component, run in-process via jco. This is the SEND
// side of outbound webhooks (Stripe / GitHub style signatures) and mirrors the
// verify performed by webhook:ingest on the receiving end. The HMAC clock is
// auto-shimmed by jco (wasi:clocks), so no manual shim is needed.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import { signer } from "../gen/webhook_sign.js";

const enc = (s: string) => new TextEncoder().encode(s);

describe("webhook:sign component", () => {
  const secret = "whsec_test";
  const body = enc('{"id":"evt_1"}');

  it("signAt produces a deterministic stripe header at a fixed timestamp", () => {
    const sig = signer.signAt(body, secret, "stripe", 1700000000n);
    assert.match(sig.header, /^t=1700000000,v1=[0-9a-f]{64}$/);
    assert.equal(sig.timestamp, 1700000000n);
  });

  it("round-trips: verify accepts a freshly signed stripe payload", () => {
    const sig = signer.signAt(body, secret, "stripe", 1700000000n);
    // tolerance 0 = skip the time-window check, just validate the MAC.
    assert.doesNotThrow(() => signer.verify(body, sig.header, secret, "stripe", 0n));
  });

  it("rejects a tampered body with signature-mismatch", () => {
    const sig = signer.signAt(body, secret, "stripe", 1700000000n);
    assert.throws(
      () => signer.verify(enc('{"id":"evt_2"}'), sig.header, secret, "stripe", 0n),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "signature-mismatch",
    );
  });

  it("rejects a wrong secret with signature-mismatch", () => {
    const sig = signer.signAt(body, secret, "stripe", 1700000000n);
    assert.throws(
      () => signer.verify(body, sig.header, "whsec_wrong", "stripe", 0n),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "signature-mismatch",
    );
  });

  it("rejects a malformed header with malformed-signature", () => {
    assert.throws(
      () => signer.verify(body, "garbage", secret, "stripe", 0n),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "malformed-signature",
    );
  });

  it("enforces the timestamp tolerance window", () => {
    // Signed far in the past; with a 60s tolerance the real clock is way past
    // it, so verify must reject it.
    const sig = signer.signAt(body, secret, "stripe", 1000n);
    assert.throws(
      () => signer.verify(body, sig.header, secret, "stripe", 60n),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "timestamp-out-of-tolerance",
    );
  });

  it("supports the github signature scheme", () => {
    const sig = signer.signAt(body, secret, "github", 0n);
    assert.match(sig.header, /^sha256=[0-9a-f]{64}$/);
    assert.equal(sig.timestamp, 0n);
    assert.doesNotThrow(() => signer.verify(body, sig.header, secret, "github", 0n));
    assert.throws(
      () => signer.verify(enc('{"id":"evt_2"}'), sig.header, secret, "github", 0n),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "signature-mismatch",
    );
  });

  it("matches a known-answer HMAC-SHA256 computed by node crypto", () => {
    // Proves the component's HMAC-SHA256 is correct, byte-for-byte, against the
    // platform crypto implementation.
    const want = "sha256=" + createHmac("sha256", "key").update("hello").digest("hex");
    const got = signer.sign(enc("hello"), "key", "github").header;
    assert.equal(got, want);
  });
});
