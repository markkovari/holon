// E2E for the qr:encode component, run in-process via jco. Pure compute (no host
// shims): encode text/URLs to a scannable QR as an SVG, unicode blocks, or the
// raw module matrix. The `level` enum arrives as a string ("low" | "medium" |
// "quartile" | "high").

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { encoder as qr } from "../gen/qr.js";

const tagOf = (e: { payload?: { tag: string } }) => e?.payload?.tag;

describe("qr:encode svg", () => {
  it("produces a self-contained, scalable SVG", () => {
    const s = qr.svg("otpauth://totp/comp:ada?secret=JBSWY3DPEHPK3PXP&issuer=comp", "medium", 4);
    assert.ok(s.startsWith("<svg"));
    assert.ok(s.includes('viewBox="0 0'));
    assert.ok(s.includes('<path fill="#000"'));
    assert.ok(s.endsWith("</svg>"));
  });

  it("quiet-zone widens the viewBox", () => {
    const size = (svg: string) => Number(svg.match(/viewBox="0 0 (\d+)/)![1]);
    assert.equal(size(qr.svg("hi", "low", 8)) - size(qr.svg("hi", "low", 0)), 16);
  });
});

describe("qr:encode matrix", () => {
  it("is a square grid with dark modules", () => {
    const m = JSON.parse(qr.matrix("hello", "low"));
    assert.ok(m.size > 0);
    assert.equal(m.modules.length, m.size);
    assert.equal(m.modules[0].length, m.size);
    assert.ok(m.modules.flat().some((b: boolean) => b === true));
  });

  it("higher ECC is at least as dense", () => {
    const lo = JSON.parse(qr.matrix("payload", "low")).size;
    const hi = JSON.parse(qr.matrix("payload", "high")).size;
    assert.ok(hi >= lo);
  });
});

describe("qr:encode unicode", () => {
  it("renders block characters", () => {
    const u = qr.unicode("hi", "low");
    assert.ok(/[█▀▄]/u.test(u));
  });
});

describe("qr:encode errors", () => {
  it("input too large -> too-long", () => {
    assert.throws(
      () => qr.svg("x".repeat(8000), "high", 2),
      (e: { payload?: { tag: string } }) => tagOf(e) === "too-long",
    );
  });
});
