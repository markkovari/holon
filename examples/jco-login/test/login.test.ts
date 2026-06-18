// E2E for the wac-COMPOSED login-app, run in-process via jco (in-memory kv +
// test config shims). Every call below drives all three plugged-in capabilities
// through the composition: session:store (create/get/revoke), config:store (the
// session-ttl read), and secrets:vault (the master-key / pepper fetch) all run
// inside login()/whoami()/logout().

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { auth } from "../gen/login_app.composed.js";

describe("composed login-app (session + config + secrets)", () => {
  it("login returns a token, csrf, and a future expiry", () => {
    const out = auth.login("alice", "pw");
    assert.ok(typeof out.token === "string" && out.token.length > 0);
    assert.ok(typeof out.csrf === "string" && out.csrf.length > 0);
    assert.ok(out.expires > 0n);
  });

  it("empty user is invalid-credentials", () => {
    assert.throws(
      () => auth.login("", "pw"),
      (e: { payload?: { tag: string } }) =>
        e?.payload?.tag === "invalid-credentials",
    );
  });

  it("empty password is invalid-credentials", () => {
    assert.throws(
      () => auth.login("alice", ""),
      (e: { payload?: { tag: string } }) =>
        e?.payload?.tag === "invalid-credentials",
    );
  });

  it("whoami resolves a live token to its user", () => {
    const { token, expires } = auth.login("alice", "pw");
    const who = auth.whoami(token);
    assert.equal(who.user, "alice");
    assert.equal(who.expires, expires);
  });

  it("whoami on an unknown token is no-session", () => {
    assert.throws(
      () => auth.whoami("bogus"),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "no-session",
    );
  });

  it("logout revokes the session (whoami then no-session)", () => {
    const { token } = auth.login("alice", "pw");
    auth.logout(token);
    assert.throws(
      () => auth.whoami(token),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "no-session",
    );
  });

  it("logout of an unknown token is idempotent (no throw)", () => {
    assert.doesNotThrow(() => auth.logout("bogus"));
  });
});
