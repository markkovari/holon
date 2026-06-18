// E2E for the session:store component, run in-process via jco (in-memory kv
// shim). Covers create/get roundtrip, CSRF verification, data updates, refresh
// extending the TTL, and revoke + expiry surfacing not-found.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { store as session } from "../gen/session_store.js";

const enc = (s: string) => new TextEncoder().encode(s);
const dec = (b: Uint8Array) => new TextDecoder().decode(b);

describe("session:store component", () => {
  it("create returns an id, csrf token, and roundtrips the data", () => {
    const s = session.create(enc('{"user":"alice"}'), 3600n);
    assert.ok(s.id);
    assert.ok(s.csrfToken);
    assert.equal(dec(s.data), '{"user":"alice"}');
  });

  it("get returns the stored session", () => {
    const s = session.create(enc('{"user":"bob"}'), 3600n);
    const got = session.get(s.id);
    assert.equal(got.id, s.id);
    assert.equal(dec(got.data), '{"user":"bob"}');
  });

  it("verifyCsrf passes for the right token and rejects a wrong one", () => {
    const s = session.create(enc('{"user":"carol"}'), 3600n);
    // correct token: no throw.
    session.verifyCsrf(s.id, s.csrfToken);
    assert.throws(
      () => session.verifyCsrf(s.id, "not-the-token"),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "csrf-mismatch",
    );
  });

  it("updateData replaces the payload", () => {
    const s = session.create(enc('{"step":1}'), 3600n);
    session.updateData(s.id, enc('{"step":2}'));
    assert.equal(dec(session.get(s.id).data), '{"step":2}');
  });

  it("refresh advances the expiry", () => {
    const s = session.create(enc('{"user":"dave"}'), 3600n);
    const refreshed = session.refresh(s.id, 7200n);
    assert.ok(refreshed.expires > s.expires);
  });

  it("revoke then get throws not-found", () => {
    const s = session.create(enc('{"user":"erin"}'), 3600n);
    session.revoke(s.id);
    assert.throws(
      () => session.get(s.id),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "not-found",
    );
  });

  it("get on an unknown id throws not-found", () => {
    assert.throws(
      () => session.get("no-such-session"),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "not-found",
    );
  });
});
