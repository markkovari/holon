// E2E for the diff:text component, run in-process via jco. Pure compute (no
// host shims): a line-level edit script (diffLines), unified-diff output
// (unified), and apply (applyUnified). The headline is the round-trip
// property — applyUnified(a, unified(a,b)) === b — checked over many edit
// shapes and context sizes against the actual compiled wasm.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { differ as d } from "../gen/textdiff.js";

const tagOf = (e: { payload?: { tag: string } }) => e?.payload?.tag;
// diffLines returns [{ tag: "equal"|"insert"|"delete", val: string }]
const sym = (op: { tag: string }) => ({ equal: "=", insert: "+", delete: "-" })[op.tag];

describe("diff:text diffLines (edit script)", () => {
  it("classifies equal / delete / insert lines", () => {
    const ops = d.diffLines("a\nb\nc", "a\nB\nc");
    assert.deepEqual(ops.map(sym), ["=", "-", "+", "="]);
  });

  it("identical input is all-equal", () => {
    const ops = d.diffLines("x\ny", "x\ny");
    assert.deepEqual(ops.map(sym), ["=", "="]);
  });
});

describe("diff:text unified", () => {
  it("empty for identical texts", () => {
    assert.equal(d.unified("same\ntext", "same\ntext", "a", "b", 3), "");
  });

  it("emits a header, hunk marker, and +/- lines", () => {
    const patch = d.unified("a\nb\nc", "a\nB\nc", "old.txt", "new.txt", 1);
    assert.match(patch, /^--- old\.txt\n\+\+\+ new\.txt\n/);
    assert.match(patch, /@@ -\d+,\d+ \+\d+,\d+ @@/);
    assert.ok(patch.includes("-b\n") && patch.includes("+B\n"));
  });
});

describe("diff:text round-trip (the property)", () => {
  const cases: [string, string][] = [
    ["", "hello"],
    ["hello", ""],
    ["a\nb\nc\nd\ne", "a\nB\nc\nd\ne"],
    ["a\nb\nc\nd\ne", "a\nc\nd\ne\nf"],
    ["one\ntwo\nthree", "zero\none\ntwo\nthree"],
    ["keep\ndrop\nkeep", "keep\nkeep"],
    ["x\ny\nz\n", "x\nY\nz\n"], // trailing newline preserved
    ["line", "totally\ndifferent\ntext"],
  ];
  for (const [a, b] of cases) {
    for (const ctx of [0, 1, 3]) {
      it(`applyUnified(a, unified(a,b)) === b  [${JSON.stringify(a)} -> ${JSON.stringify(b)}, ctx=${ctx}]`, () => {
        const patch = d.unified(a, b, "a", "b", ctx);
        assert.equal(d.applyUnified(a, patch), b);
      });
    }
  }
});

describe("diff:text errors", () => {
  it("applying a patch whose context doesn't fit throws 'context-mismatch'", () => {
    const patch = d.unified("a\nb\nc", "a\nB\nc", "a", "b", 1);
    assert.throws(
      () => d.applyUnified("a\nDIFFERENT\nc", patch),
      (e: { payload?: { tag: string } }) => tagOf(e) === "context-mismatch",
    );
  });

  it("a malformed hunk header throws 'malformed-patch'", () => {
    assert.throws(
      () => d.applyUnified("a\nb", "@@ garbage @@\n x\n"),
      (e: { payload?: { tag: string } }) => tagOf(e) === "malformed-patch",
    );
  });
});
