// E2E for the otp:totp component, run in-process via jco. The component is
// stateless: the caller supplies the shared secret on every call (store it in
// secrets:vault). It imports only wasi:clocks (for totp-now) and wasi:random
// (for provision / recovery-codes), both auto-shimmed by jco.
//
// The headline assertions are the official RFC 6238 (TOTP) and RFC 4226 (HOTP)
// known-answer vectors — they prove the component's HMAC-SHA1/base32 crypto is
// byte-for-byte correct, not just internally consistent.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { authenticator as otp } from "../gen/otp.js";

// RFC test secret: ASCII "12345678901234567890" in base32.
const SECRET = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

// Errors surface through jco as { payload: { tag } }.
const tagIs =
  (tag: string) =>
  (e: { payload?: { tag?: string } }) =>
    e?.payload?.tag === tag;

describe("otp:totp component", () => {
  // --- RFC 6238 Appendix B, SHA1, 8 digits, period 30 (CRITICAL) ---
  it("matches the RFC 6238 TOTP known-answer vectors", () => {
    assert.equal(otp.totpAt(SECRET, 59n, 30, 8), "94287082");
    assert.equal(otp.totpAt(SECRET, 1111111109n, 30, 8), "07081804");
    assert.equal(otp.totpAt(SECRET, 1234567890n, 30, 8), "89005924");
  });

  // --- RFC 4226 Appendix D, SHA1, 6 digits (CRITICAL) ---
  it("matches the RFC 4226 HOTP known-answer vectors", () => {
    assert.equal(otp.hotpAt(SECRET, 0n, 6), "755224");
    assert.equal(otp.hotpAt(SECRET, 1n, 6), "287082");
    assert.equal(otp.hotpAt(SECRET, 2n, 6), "359152");
  });

  it("provision returns a secret and an otpauth:// URI", () => {
    const p = otp.provision("Acme", "alice@example.com");
    assert.ok(p.secret.length > 0, "secret should be non-empty");
    assert.ok(
      p.uri.startsWith("otpauth://totp/"),
      `uri should be an otpauth totp URI, got: ${p.uri}`,
    );
  });

  it("verify accepts the freshly-generated current code and rejects garbage", () => {
    const code = otp.totpNow(SECRET);
    assert.equal(otp.verify(SECRET, code, 30, 6, 1), true);
    // wrong-length code is never valid.
    assert.equal(otp.verify(SECRET, "12345", 30, 6, 1), false);
  });

  it("recoveryCodes returns N codes in the documented format", () => {
    const codes = otp.recoveryCodes(5);
    assert.equal(codes.length, 5);
    const fmt = /^[a-z0-9]{4}-[a-z0-9]{4}$/;
    for (const c of codes) {
      assert.match(c, fmt);
    }
  });

  it("rejects a non-base32 secret with bad-secret", () => {
    assert.throws(() => otp.totpAt("not!base32!", 0n, 30, 6), tagIs("bad-secret"));
  });

  it("rejects an out-of-range digit count with bad-digits", () => {
    // digits is a u8; 9 fits the type but is outside the supported 6..8 range.
    assert.throws(() => otp.totpAt(SECRET, 0n, 30, 9), tagIs("bad-digits"));
  });
});
