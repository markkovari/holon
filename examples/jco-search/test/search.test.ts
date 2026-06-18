// E2E for the search:index component, run in-process via jco (in-memory kv
// shim). Covers indexing, doc-count, any/all query modes, tag faceting,
// removal, and re-index replacement. The TF-IDF inverted index lives entirely
// in the KV store; we only assert membership + ordering, never exact scores.
//
// Note: the component drops tokens shorter than 2 chars, so tests use words
// of >= 2 chars.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { index as search } from "../gen/search_index.js";

const ids = (hits: Array<{ id: string; score: number }>) => hits.map((h) => h.id);

describe("search:index component", () => {
  // Seed the corpus once; later tests mutate it (remove / re-index).
  search.indexDoc("d1", "the quick brown fox", ["kind:note"]);
  search.indexDoc("d2", "quick fox jumps", ["kind:note"]);
  search.indexDoc("d3", "lazy dog sleeps", []);

  it("docCount reflects the indexed corpus", () => {
    assert.equal(search.docCount(), 3n);
  });

  it("any-mode query returns docs containing the term, score-sorted desc", () => {
    const hits = search.query("quick", "any", [], 10);
    const got = ids(hits);
    assert.ok(got.includes("d1"));
    assert.ok(got.includes("d2"));
    assert.ok(!got.includes("d3"));
    // scores are positive numbers and sorted descending
    for (const h of hits) {
      assert.equal(typeof h.score, "number");
      assert.ok(h.score > 0);
    }
    for (let i = 1; i < hits.length; i++) {
      assert.ok(hits[i - 1].score >= hits[i].score);
    }
  });

  it("all-mode query requires every term to be present", () => {
    const hits = search.query("quick fox", "all", [], 10);
    const got = ids(hits);
    assert.ok(got.includes("d1")); // "the quick brown fox"
    assert.ok(got.includes("d2")); // "quick fox jumps"
    assert.ok(!got.includes("d3"));
  });

  it("tag filter narrows results to matching facets", () => {
    const tagged = ids(search.query("quick", "any", ["kind:note"], 10));
    assert.ok(tagged.includes("d1"));
    assert.ok(tagged.includes("d2"));

    const none = search.query("quick", "any", ["kind:missing"], 10);
    assert.equal(none.length, 0);
  });

  it("remove drops a doc from the count and from results", () => {
    search.remove("d1");
    assert.equal(search.docCount(), 2n);
    const got = ids(search.query("brown", "any", [], 10));
    assert.ok(!got.includes("d1"));
  });

  it("re-indexing an id replaces its old text", () => {
    search.indexDoc("d2", "totally different words", []);
    const got = ids(search.query("quick", "any", [], 10));
    assert.ok(!got.includes("d2"));
    // the new text is searchable
    const fresh = ids(search.query("different", "any", [], 10));
    assert.ok(fresh.includes("d2"));
  });
});
