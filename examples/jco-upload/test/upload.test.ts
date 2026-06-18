// E2E for the upload:policy component, run in-process via jco. Covers type/size
// validation (check), issuing a signed presigned ticket (authorize), and
// redeeming it (redeem) — including rejection of garbage and tampered tokens.
// wasi:clocks and wasi:random are auto-shimmed by jco; only wasi:config is
// supplied locally (src/config-shim.js).

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { gate as upload } from "../gen/upload_policy.js";

const hasTag =
  (tag: string) => (e: { payload?: { tag: string } }) => e?.payload?.tag === tag;

describe("upload:policy component", () => {
  it("check accepts an allowed type within the size limit", () => {
    assert.equal(upload.check("image/png", 1000n), undefined);
  });

  it("check rejects a disallowed content type", () => {
    assert.throws(
      () => upload.check("application/zip", 1000n),
      hasTag("type-not-allowed"),
    );
  });

  it("check rejects an over-sized upload", () => {
    assert.throws(
      () => upload.check("image/png", 99999999999n),
      hasTag("too-large"),
    );
  });

  it("authorize issues a signed ticket", () => {
    const ticket = upload.authorize("acme", "image/png", 2048n, 0n);
    assert.equal(typeof ticket.token, "string");
    assert.ok(ticket.token.length > 0);
    assert.ok(ticket.objectKey.startsWith("acme/"));
    assert.equal(typeof ticket.expires, "bigint");
    assert.ok(ticket.expires > 0n);
  });

  it("redeem returns the grant matching the authorized ticket", () => {
    const ticket = upload.authorize("acme", "image/png", 2048n, 0n);
    const grant = upload.redeem(ticket.token);
    assert.equal(grant.objectKey, ticket.objectKey);
    assert.equal(grant.contentType, "image/png");
    assert.equal(typeof grant.maxSize, "bigint");
    assert.equal(grant.maxSize, 2048n);
  });

  it("redeem rejects a garbage token", () => {
    assert.throws(
      () => upload.redeem("garbage.deadbeef"),
      hasTag("invalid-ticket"),
    );
  });

  it("redeem rejects a tampered token", () => {
    const ticket = upload.authorize("acme", "image/png", 2048n, 0n);
    // Flip one character to break the HMAC signature.
    const ch = ticket.token[5] === "a" ? "b" : "a";
    const tampered = ticket.token.slice(0, 5) + ch + ticket.token.slice(6);
    assert.notEqual(tampered, ticket.token);
    assert.throws(
      () => upload.redeem(tampered),
      hasTag("invalid-ticket"),
    );
  });
});
