// E2E for the quota:meter component, run in-process via jco (in-memory kv +
// atomics shims). Covers fresh peek, cumulative reserve up to the limit, a
// reserve that would exceed the limit (rejected, nothing consumed), unconditional
// recordUsage past the limit, reset, and subject isolation.
//
// A generous period keeps the metering window from rolling mid-test. We use the
// component's internal default window (30 days = 2592000s) so that `reset`,
// which takes no period argument and clears only its own default window, lines
// up with the period passed to reserve/recordUsage/peek.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { meter as quota } from "../gen/quota.js";

const DAY = 2592000n; // 30 days — matches reset's internal window

describe("quota:meter component", () => {
  it("peek of a fresh subject is empty", () => {
    const b = quota.peek("fresh", 100n, DAY);
    assert.equal(b.used, 0n);
    assert.equal(b.remaining, 100n);
    assert.equal(b.limit, 100n);
  });

  it("reserve accumulates usage and reports remaining", () => {
    const b1 = quota.reserve("u1", 30n, 100n, DAY);
    assert.equal(b1.used, 30n);
    assert.equal(b1.remaining, 70n);

    const b2 = quota.reserve("u1", 50n, 100n, DAY);
    assert.equal(b2.used, 80n);
    assert.equal(b2.remaining, 20n);
  });

  it("a reserve that would exceed the limit is rejected and consumes nothing", () => {
    assert.throws(
      () => quota.reserve("u1", 50n, 100n, DAY), // 80 + 50 = 130 > 100
      (e: { payload?: { tag: string; val?: bigint } }) => {
        assert.equal(e?.payload?.tag, "exceeded");
        assert.equal(e?.payload?.val, 20n); // remaining at time of failure
        return true;
      },
    );
    // nothing was consumed by the failed reserve
    const b = quota.peek("u1", 100n, DAY);
    assert.equal(b.used, 80n);
    assert.equal(b.remaining, 20n);
  });

  it("recordUsage is unconditional and can drive usage past the limit", () => {
    const a = quota.recordUsage("u2", 5n, 10n, DAY);
    assert.equal(a.used, 5n);
    const b = quota.recordUsage("u2", 5n, 10n, DAY);
    assert.equal(b.used, 10n);
    assert.equal(b.remaining, 0n);

    // third record pushes past the limit; remaining floors at 0n
    const c = quota.recordUsage("u2", 5n, 10n, DAY);
    assert.equal(c.used, 15n);
    assert.equal(c.remaining, 0n);
  });

  it("reset clears a subject's accumulated usage", () => {
    quota.reset("u1");
    const b = quota.peek("u1", 100n, DAY);
    assert.equal(b.used, 0n);
    assert.equal(b.remaining, 100n);
  });

  it("subjects are isolated", () => {
    // u2 still carries its over-limit total; u1 was just reset
    const u2 = quota.peek("u2", 10n, DAY);
    assert.equal(u2.used, 15n);
    const u1 = quota.peek("u1", 100n, DAY);
    assert.equal(u1.used, 0n);
  });
});
