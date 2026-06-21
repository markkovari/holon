// E2E for the lock:mutex component, run in-process via jco. Covers acquire ->
// (held by another) -> release -> re-acquire (fence bumps), token-gated
// release/renew, renew keep-alive, and the invalid-ttl / not-holder error
// surface. TTLs are long so leases don't lapse mid-test.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { mutex } from "../gen/lock.js";

describe("lock:mutex component", () => {
  it("acquire grants a lease with a token and fence", () => {
    const lease = mutex.acquire("appt-1", "doctor-a", 60n);
    assert.equal(lease.key, "appt-1");
    assert.equal(lease.owner, "doctor-a");
    assert.ok(lease.token.length > 0, "lease has a token");
    assert.ok(lease.fence >= 1n, "fence starts at >= 1");
  });

  it("a second acquire while held throws held(current-lease)", () => {
    mutex.acquire("appt-2", "doctor-a", 60n);
    assert.throws(
      () => mutex.acquire("appt-2", "doctor-b", 60n),
      (e: { payload?: { tag?: string; val?: { owner?: string } } }) =>
        e?.payload?.tag === "held" && e?.payload?.val?.owner === "doctor-a",
    );
  });

  it("release frees the lock; another owner can then acquire (fence bumps)", () => {
    const a = mutex.acquire("appt-3", "doctor-a", 60n);
    mutex.release("appt-3", a.token);
    const b = mutex.acquire("appt-3", "doctor-b", 60n);
    assert.equal(b.owner, "doctor-b");
    assert.ok(b.fence > a.fence, "fence increments when the lock changes hands");
  });

  it("release with the wrong token throws not-holder", () => {
    mutex.acquire("appt-4", "doctor-a", 60n);
    assert.throws(
      () => mutex.release("appt-4", "not-the-real-token"),
      (e: { payload?: { tag?: string } }) => e?.payload?.tag === "not-holder",
    );
  });

  it("renew extends a held lease and keeps the same token + fence", () => {
    const a = mutex.acquire("appt-5", "doctor-a", 30n);
    const r = mutex.renew(a.token, 120n);
    assert.equal(r.token, a.token, "renew keeps the token");
    assert.equal(r.fence, a.fence, "renew does not bump the fence");
    assert.ok(r.expires > a.expires, "renew extends the expiry");
  });

  it("renew with a stale token throws not-holder", () => {
    assert.throws(
      () => mutex.renew("ghost-token", 60n),
      (e: { payload?: { tag?: string } }) => e?.payload?.tag === "not-holder",
    );
  });

  it("holder peeks the current lease but blanks the token", () => {
    mutex.acquire("appt-6", "doctor-a", 60n);
    const h = mutex.holder("appt-6");
    assert.ok(h, "lock is held");
    assert.equal(h!.owner, "doctor-a");
    assert.equal(h!.token, "", "peek never reveals the secret token");
    // a free key reads as none
    assert.equal(mutex.holder("never-locked"), undefined);
  });

  it("acquire with ttl 0 throws invalid-ttl", () => {
    assert.throws(
      () => mutex.acquire("appt-7", "doctor-a", 0n),
      (e: { payload?: { tag?: string } }) => e?.payload?.tag === "invalid-ttl",
    );
  });

  it("an expired lease is taken over by the next acquirer", () => {
    // ttl 1s; the in-process host clock advances in real time. Acquire, then
    // busy-wait just past expiry, then a different owner takes it over.
    const a = mutex.acquire("appt-8", "doctor-a", 1n);
    const deadline = Date.now() + 1300;
    while (Date.now() < deadline) {
      /* spin ~1.3s so the 1s lease lapses */
    }
    const b = mutex.acquire("appt-8", "doctor-b", 60n);
    assert.equal(b.owner, "doctor-b", "expired lease taken over");
    assert.ok(b.fence > a.fence, "takeover bumps the fence");
  });
});
