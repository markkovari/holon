// E2E for the money:amount component, run in-process via jco. The component is
// pure-compute (imports no WASI host functions), so no shims/--map are needed.
// Covers parse/format round-trips for different exponents, add/subtract,
// scale, allocate (remainder distribution), currency-mismatch errors, and
// ordering via compare.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { arithmetic as money } from "../gen/money.js";

describe("money:amount component", () => {
  it("parses a 2-exponent currency into minor units", () => {
    const a = money.parse("10.99", "USD");
    assert.equal(a.units, 1099n);
    assert.equal(a.currency, "USD");
  });

  it("formats minor units back to a decimal string", () => {
    assert.equal(money.format(money.parse("10.99", "USD")), "10.99");
  });

  it("handles a 0-exponent currency (JPY)", () => {
    const jpy = money.parse("1000", "JPY");
    assert.equal(jpy.units, 1000n);
    assert.equal(money.format(jpy), "1000");
  });

  it("adds two same-currency amounts", () => {
    const sum = money.add(money.parse("10.99", "USD"), money.parse("0.01", "USD"));
    assert.equal(sum.units, 1100n);
    assert.equal(money.format(sum), "11.00");
  });

  it("subtracts two same-currency amounts", () => {
    const diff = money.subtract(money.parse("10.99", "USD"), money.parse("0.01", "USD"));
    assert.equal(diff.units, 1098n);
    assert.equal(money.format(diff), "10.98");
  });

  it("scales an amount by an integer factor", () => {
    const scaled = money.scale(money.parse("10.99", "USD"), 3n);
    assert.equal(scaled.units, 3297n);
    assert.equal(money.format(scaled), "32.97");
  });

  it("allocates a total across shares without losing pennies", () => {
    const parts = money.allocate({ units: 1000n, currency: "USD" }, 3);
    assert.equal(parts.length, 3);
    assert.deepEqual(
      parts.map((p) => p.units),
      [334n, 333n, 333n],
    );
    const total = parts.reduce((acc, p) => acc + p.units, 0n);
    assert.equal(total, 1000n);
  });

  it("rejects adding mismatched currencies", () => {
    assert.throws(
      () => money.add(money.parse("10.99", "USD"), money.parse("1000", "JPY")),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "currency-mismatch",
    );
  });

  it("rejects an unknown currency on parse", () => {
    assert.throws(
      () => money.parse("1.00", "ZZZ"),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "unknown-currency",
    );
  });

  it("orders amounts via compare", () => {
    const small = money.parse("0.01", "USD");
    const big = money.parse("10.99", "USD");
    assert.equal(money.compare(small, big), -1);
    assert.equal(money.compare(big, small), 1);
    assert.equal(money.compare(big, money.parse("10.99", "USD")), 0);
  });
});
