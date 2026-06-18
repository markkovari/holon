// E2E for the csv:codec component, run in-process via jco. Pure-compute: the
// component has no WASI runtime dependencies, so no shims/--map are needed.
// Covers RFC 4180 parsing (quoting, embedded commas/quotes/newlines), trimming,
// custom delimiters, header records, ragged-row + malformed errors, and a
// format -> parse round-trip.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { codec as csv } from "../gen/csv.js";

type Dialect = { delimiter: string; hasHeader: boolean; trim: boolean };

const opts: Dialect = { delimiter: "", hasHeader: false, trim: false };

describe("csv:codec component", () => {
  it("parses a simple two-row document", () => {
    const rows = csv.parse("a,b,c\n1,2,3", opts);
    assert.equal(rows.length, 2);
    assert.deepEqual(rows[0].fields, ["a", "b", "c"]);
    assert.deepEqual(rows[1].fields, ["1", "2", "3"]);
  });

  it("parses quoted fields (embedded comma, newline, escaped quote)", () => {
    const rows = csv.parse('"hello, world","line\nbreak","say ""hi"""', opts);
    assert.equal(rows.length, 1);
    assert.deepEqual(rows[0].fields, [
      "hello, world",
      "line\nbreak",
      'say "hi"',
    ]);
  });

  it("does not add an empty row for a trailing newline", () => {
    const rows = csv.parse("a\nb\n", opts);
    assert.equal(rows.length, 2);
    assert.deepEqual(rows[0].fields, ["a"]);
    assert.deepEqual(rows[1].fields, ["b"]);
  });

  it("trims field whitespace when trim is enabled", () => {
    const rows = csv.parse("  a , b ", {
      delimiter: "",
      hasHeader: false,
      trim: true,
    });
    assert.equal(rows.length, 1);
    assert.deepEqual(rows[0].fields, ["a", "b"]);
  });

  it("honors a custom delimiter (TSV)", () => {
    const rows = csv.parse("a\tb", {
      delimiter: "\t",
      hasHeader: false,
      trim: false,
    });
    assert.equal(rows.length, 1);
    assert.deepEqual(rows[0].fields, ["a", "b"]);
  });

  it("parseRecords pairs header keys with row values", () => {
    const records = csv.parseRecords("name,age\nAlice,30\nBob,25", {
      delimiter: "",
      hasHeader: true,
      trim: false,
    });
    assert.equal(records.length, 2);
    const first = records[0].pairs;
    const find = (k: string) => first.find((p) => p[0] === k);
    assert.deepEqual(find("name"), ["name", "Alice"]);
    assert.deepEqual(find("age"), ["age", "30"]);
  });

  it("throws ragged-row when a data row has the wrong arity", () => {
    let thrown: { payload?: { tag: string; val?: unknown } } | undefined;
    try {
      csv.parseRecords("a,b\n1,2,3", {
        delimiter: "",
        hasHeader: true,
        trim: false,
      });
    } catch (e) {
      thrown = e as typeof thrown;
    }
    assert.ok(thrown, "expected parseRecords to throw");
    assert.equal(thrown!.payload?.tag, "ragged-row");
    assert.equal(thrown!.payload?.val, 0);
  });

  it("throws malformed on an unterminated quoted field", () => {
    let thrown: { payload?: { tag: string } } | undefined;
    try {
      csv.parse('"unterminated', opts);
    } catch (e) {
      thrown = e as typeof thrown;
    }
    assert.ok(thrown, "expected parse to throw");
    assert.equal(thrown!.payload?.tag, "malformed");
  });

  it("format -> parse round-trips comma/quote/newline content", () => {
    const rows = [{ fields: ["a", "b,c", 'd"e', "f\ng"] }];
    const out = csv.format(rows, opts);
    const reparsed = csv.parse(out, opts);
    assert.equal(reparsed.length, 1);
    assert.deepEqual(reparsed[0].fields, rows[0].fields);
  });

  it("format quotes fields that contain the delimiter", () => {
    const out = csv.format([{ fields: ["plain", "has,comma"] }], opts);
    assert.ok(
      out.includes('"has,comma"'),
      `expected quoted field in output, got: ${out}`,
    );
  });
});
