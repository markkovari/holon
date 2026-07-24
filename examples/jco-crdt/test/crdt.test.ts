// E2E for the crdt:merge component, run in-process via jco. Pure compute (no
// host shims). Beyond per-type behavior, the real subject is the CRDT
// guarantee: merge is commutative + associative + idempotent, so replicas
// converge no matter what order state arrives in. We prove that by folding
// many random permutations of the same states and asserting the merged bytes
// are identical every time (output is canonical, so string equality == state
// equality). This is the objective, property-based check — not just examples.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { merger as c } from "../gen/crdt.js";

const tagOf = (e: { payload?: { tag: string } }) => e?.payload?.tag;
const val = (s: string) => JSON.parse(c.value(s));

// Deterministic PRNG (mulberry32) so failures reproduce.
function rng(seed: number) {
  return () => {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
function shuffle<T>(xs: T[], r: () => number): T[] {
  const a = xs.slice();
  for (let i = a.length - 1; i > 0; i--) {
    const j = Math.floor(r() * (i + 1));
    [a[i], a[j]] = [a[j], a[i]];
  }
  return a;
}
const fold = (states: string[]) => states.reduce((acc, s) => c.merge(acc, s));

// The core property: every permutation of the same states folds to identical
// bytes (commutativity + associativity), and re-merging a state changes
// nothing (idempotence).
function assertConverges(states: string[], seed: number) {
  const r = rng(seed);
  const expected = fold(states);
  for (let i = 0; i < 40; i++) {
    assert.equal(fold(shuffle(states, r)), expected, "permutation diverged");
  }
  assert.equal(c.merge(expected, expected), expected, "merge not idempotent");
  assert.equal(c.merge(expected, states[0]), expected, "re-merging a part changed state");
}

describe("LWW-Register", () => {
  it("higher timestamp wins, either merge order", () => {
    const a = c.lwwNew('"old"', 1, "A");
    const b = c.lwwNew('"new"', 2, "B");
    assert.equal(val(c.merge(a, b)), "new");
    assert.equal(c.merge(a, b), c.merge(b, a)); // commutative, byte-equal
  });

  it("equal timestamp breaks on higher replica id", () => {
    const a = c.lwwNew('"fromA"', 5, "A");
    const b = c.lwwNew('"fromB"', 5, "B");
    assert.equal(val(c.merge(a, b)), "fromB");
  });

  it("lww-set only advances on a newer stamp", () => {
    let s = c.lwwNew('"v1"', 10, "A");
    s = c.lwwSet(s, '"stale"', 3, "A"); // older -> ignored
    assert.equal(val(s), "v1");
    s = c.lwwSet(s, '"v2"', 11, "A"); // newer -> wins
    assert.equal(val(s), "v2");
  });
});

describe("PN-Counter", () => {
  it("sums increments minus decrements across replicas", () => {
    let a = c.counterAdd(c.counterNew(), "A", 5);
    let b = c.counterAdd(c.counterNew(), "B", -2);
    b = c.counterAdd(b, "B", 4);
    assert.equal(val(c.merge(a, b)), 7); // 5 + (-2 + 4)
  });

  it("converges over random increments in any merge order", () => {
    const r = rng(99);
    const replicas = ["A", "B", "C"].map((id) => {
      let s = c.counterNew();
      for (let i = 0; i < 8; i++) {
        s = c.counterAdd(s, id, Math.floor(r() * 21) - 10);
      }
      return s;
    });
    assertConverges(replicas, 1234);
  });
});

describe("OR-Set (add wins)", () => {
  it("concurrent add survives a concurrent remove", () => {
    // Shared history: x was added with tag a1 and observed by both.
    const base = c.orsetAdd(c.orsetNew(), "x", "A:1");
    // Replica A removes x (tombstones the tag it saw: A:1).
    const removed = c.orsetRemove(base, "x");
    // Replica B concurrently re-adds x with a NEW tag B:1 (unseen by A).
    const readded = c.orsetAdd(base, "x", "B:1");
    const merged = c.merge(removed, readded);
    assert.deepEqual(val(merged), ["x"]); // add wins
    assert.equal(c.merge(removed, readded), c.merge(readded, removed));
  });

  it("removing every observed tag drops the element", () => {
    let s = c.orsetAdd(c.orsetNew(), "y", "A:1");
    s = c.orsetRemove(s, "y");
    assert.deepEqual(val(s), []);
  });
});

describe("LWW-Map (scribe's per-field convergence)", () => {
  it("independent keys both survive; same key resolves by stamp", () => {
    const a = c.lwwmapSet(c.lwwmapNew(), "title", '"Draft"', 1, "A");
    let b = c.lwwmapSet(c.lwwmapNew(), "body", '"hello"', 1, "B");
    b = c.lwwmapSet(b, "title", '"Final"', 2, "B"); // newer -> wins over A's title
    assert.deepEqual(val(c.merge(a, b)), { title: "Final", body: "hello" });
  });

  it("tombstone beats an older concurrent set", () => {
    const setter = c.lwwmapSet(c.lwwmapNew(), "k", '"v"', 1, "A");
    const remover = c.lwwmapRemove(c.lwwmapNew(), "k", 2, "B");
    assert.deepEqual(val(c.merge(setter, remover)), {}); // deleted
  });

  it("converges over interleaved edits from three replicas", () => {
    const r = rng(7);
    const keys = ["title", "body", "status"];
    const replicas = ["A", "B", "C"].map((id, ri) => {
      let s = c.lwwmapNew();
      for (let i = 0; i < 6; i++) {
        const k = keys[Math.floor(r() * keys.length)];
        const ts = i * 3 + ri + 1;
        s = r() < 0.25 ? c.lwwmapRemove(s, k, ts, id) : c.lwwmapSet(s, k, `"${id}${i}"`, ts, id);
      }
      return s;
    });
    assertConverges(replicas, 555);
  });
});

describe("RGA (text sequence — concurrent typing interleaves)", () => {
  const ins = (s: string, i: number, t: string, id: string) => c.rgaInsert(s, i, t, id);

  it("builds and edits text", () => {
    let s = ins(c.rgaNew(), 0, "hello", "0001-a");
    assert.equal(val(s), "hello");
    s = ins(s, 5, " world", "0002-a");
    assert.equal(val(s), "hello world");
    s = c.rgaDelete(s, 0, 6); // drop "hello "
    assert.equal(val(s), "world");
  });

  it("concurrent inserts at the same spot BOTH survive and converge", () => {
    const base = ins(c.rgaNew(), 0, "AC", "0000-seed");
    const rx = ins(base, 1, "X", "0001-x");
    const ry = ins(base, 1, "Y", "0002-y"); // higher id sorts first
    assert.equal(val(c.merge(rx, ry)), "AYXC");
    assert.equal(c.merge(rx, ry), c.merge(ry, rx)); // commutative, byte-equal
    assertConverges([rx, ry, base], 42);
  });

  it("a delete and a concurrent edit both apply", () => {
    const base = ins(c.rgaNew(), 0, "abc", "0000-s");
    const deleted = c.rgaDelete(base, 1, 1); // "ac"
    const edited = ins(base, 3, "d", "0001-e"); // "abcd"
    assert.equal(val(c.merge(deleted, edited)), "acd");
  });
});

describe("errors", () => {
  it("merging different types throws 'type-mismatch'", () => {
    assert.throws(
      () => c.merge(c.counterNew(), c.orsetNew()),
      (e: { payload?: { tag: string } }) => tagOf(e) === "type-mismatch",
    );
  });
  it("malformed state throws 'invalid-json'", () => {
    assert.throws(
      () => c.value("not json"),
      (e: { payload?: { tag: string } }) => tagOf(e) === "invalid-json",
    );
  });
  it("wrong-shape state throws 'invalid-state'", () => {
    assert.throws(
      () => c.counterAdd('{"type":"lww","v":1,"ts":1,"replica":"A"}', "A", 1),
      (e: { payload?: { tag: string } }) => tagOf(e) === "invalid-state",
    );
  });
});
