// E2E for the cron:expr component, run in-process via jco. Pure compute (no host
// shims): parse/normalize a cron expression, test whether a timestamp matches,
// and compute the next N fire times. All UTC; `next` returns Unix seconds as
// BigInt (WIT u64).

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { parser as cron } from "../gen/cron.js";

const tagOf = (e: { payload?: { tag: string } }) => e?.payload?.tag;
const EPOCH_2021 = 1609459200n; // 2021-01-01 00:00:00 UTC

describe("cron:expr parse (normalize)", () => {
  it("expands @macros", () => {
    assert.equal(cron.parse("@hourly"), "0 * * * *");
    assert.equal(cron.parse("@daily"), "0 0 * * *");
    assert.equal(cron.parse("@weekly"), "0 0 * * 0");
  });
  it("lowers names + steps to numbers", () => {
    assert.equal(cron.parse("0 0 * jan mon"), "0 0 * 1 1");
    assert.equal(cron.parse("*/15 * * * *"), "0,15,30,45 * * * *");
  });
});

describe("cron:expr next", () => {
  it("every 6 hours", () => {
    const got = Array.from(cron.next("0 */6 * * *", EPOCH_2021, 4));
    assert.deepEqual(got, [
      EPOCH_2021 + 6n * 3600n,
      EPOCH_2021 + 12n * 3600n,
      EPOCH_2021 + 18n * 3600n,
      EPOCH_2021 + 24n * 3600n,
    ]);
  });
  it("jumps years to a leap day (0 0 29 2 *)", () => {
    // next Feb 29 midnight after 2021-01-01 is 2024-02-29 00:00 UTC
    assert.deepEqual(Array.from(cron.next("0 0 29 2 *", EPOCH_2021, 1)), [1709164800n]);
  });
});

describe("cron:expr matches", () => {
  it("matches a Monday 09:30 for '30 9 * * mon'", () => {
    const mon0930 = 1609752600n; // 2021-01-04 09:30 UTC (a Monday)
    assert.equal(cron.matches("30 9 * * mon", mon0930), true);
    assert.equal(cron.matches("30 9 * * mon", mon0930 + 86400n), false); // Tuesday
  });
});

describe("cron:expr errors", () => {
  it("too few fields -> invalid-expression", () => {
    assert.throws(
      () => cron.parse("* * *"),
      (e: { payload?: { tag: string } }) => tagOf(e) === "invalid-expression",
    );
  });
  it("out-of-range minute -> invalid-expression", () => {
    assert.throws(
      () => cron.parse("60 * * * *"),
      (e: { payload?: { tag: string } }) => tagOf(e) === "invalid-expression",
    );
  });
});
