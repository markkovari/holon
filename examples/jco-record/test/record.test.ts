// E2E for the records:store component, run in-process via jco (in-memory kv
// shim). Records are typed JSON objects kept in named collections with ULID
// ids, secondary indexes, and optimistic revision locking.
//
// The shim's backing store persists across tests in a single run, so each test
// uses a unique collection name to avoid cross-test interference.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { store } from "../gen/record_store.js";

type Entry = {
  id: string;
  data: string;
  revision: bigint;
  created: bigint;
  updated: bigint;
};

const tag = (e: unknown) => (e as { payload?: { tag?: string } })?.payload?.tag;

describe("records:store component", () => {
  it("create + get round-trip", () => {
    const pet = { name: "Rex", owner: "u1" };
    const created: Entry = store.create("pets", JSON.stringify(pet), ["owner"]);

    assert.equal(created.id.length, 26); // ULID
    assert.equal(created.revision, 1n);

    const got: Entry = store.get("pets", created.id);
    assert.equal(got.id, created.id);
    assert.deepEqual(JSON.parse(got.data), pet);
  });

  it("create rejects non-object JSON with invalid-json", () => {
    for (const bad of ['"hi"', "5", "not json"]) {
      assert.throws(
        () => store.create("pets", bad, []),
        (e) => tag(e) === "invalid-json",
        `expected invalid-json for ${bad}`,
      );
    }
  });

  it("update bumps revision and changes data", () => {
    const c: Entry = store.create("upd", JSON.stringify({ n: 1 }), []);
    const u: Entry = store.update(
      "upd",
      c.id,
      JSON.stringify({ n: 2 }),
      c.revision,
    );
    assert.ok(u.revision > c.revision);

    const got: Entry = store.get("upd", c.id);
    assert.deepEqual(JSON.parse(got.data), { n: 2 });
    assert.equal(got.revision, u.revision);
  });

  it("update with a wrong expectedRevision throws revision-conflict carrying the current revision", () => {
    const c: Entry = store.create("upd2", JSON.stringify({ n: 1 }), []);
    assert.throws(
      () => store.update("upd2", c.id, JSON.stringify({ n: 9 }), 99n),
      (e) => {
        const err = e as { payload?: { tag?: string; val?: bigint } };
        return (
          err?.payload?.tag === "revision-conflict" &&
          err?.payload?.val === c.revision
        );
      },
    );
  });

  it("update with expectedRevision 0 applies without a check", () => {
    const c: Entry = store.create("upd3", JSON.stringify({ n: 1 }), []);
    // bump once so current revision is no longer 1
    store.update("upd3", c.id, JSON.stringify({ n: 2 }), c.revision);
    // expectedRevision 0 -> no optimistic check, applies anyway
    const u: Entry = store.update("upd3", c.id, JSON.stringify({ n: 3 }), 0n);
    assert.deepEqual(JSON.parse(store.get("upd3", c.id).data), { n: 3 });
    assert.ok(u.revision > c.revision);
  });

  it("delete then get throws not-found; deleting an absent id does not throw", () => {
    const c: Entry = store.create("del", JSON.stringify({ x: 1 }), []);
    store.delete("del", c.id);
    assert.throws(
      () => store.get("del", c.id),
      (e) => tag(e) === "not-found",
    );
    // idempotent
    assert.doesNotThrow(() => store.delete("del", c.id));
    assert.doesNotThrow(() => store.delete("del", "nonexistent"));
  });

  it("findBy uses the secondary index for exact matches", () => {
    for (let i = 0; i < 3; i++) {
      store.create("pets2", JSON.stringify({ name: `a${i}`, owner: "u1" }), [
        "owner",
      ]);
    }
    for (let i = 0; i < 2; i++) {
      store.create("pets2", JSON.stringify({ name: `b${i}`, owner: "u2" }), [
        "owner",
      ]);
    }

    const u1: Entry[] = store.findBy("pets2", "owner", '"u1"');
    assert.equal(u1.length, 3);
    for (const e of u1) assert.equal(JSON.parse(e.data).owner, "u1");

    const none: Entry[] = store.findBy("pets2", "owner", '"nobody"');
    assert.deepEqual(none, []);
  });

  it("listRecords pages in ULID (sort) order", () => {
    const ids: string[] = [];
    for (let i = 0; i < 5; i++) {
      ids.push(store.create("page", JSON.stringify({ i }), []).id);
    }

    const p1 = store.listRecords("page", 2, "");
    assert.equal(p1.entries.length, 2);
    assert.ok(p1.next.length > 0);

    const p2 = store.listRecords("page", 2, p1.next);
    assert.equal(p2.entries.length, 2);

    const seen = [...p1.entries, ...p2.entries].map((e: Entry) => e.id);
    // ULID ids sort lexicographically; pages walk them in ascending order.
    const sorted = [...seen].sort();
    assert.deepEqual(seen, sorted, "ids should be ULID-sorted ascending");
    // pages do not overlap
    assert.equal(new Set(seen).size, seen.length);
    // the walked ids are the first four of all created ids in ULID order
    assert.deepEqual(seen, [...ids].sort().slice(0, 4));
  });

  it("count returns the number of records in a collection", () => {
    for (let i = 0; i < 4; i++) {
      store.create("cnt", JSON.stringify({ i }), []);
    }
    assert.equal(store.count("cnt"), 4n);
  });

  it("query matches on multiple indexed field filters", () => {
    const idx = ["kind", "owner"];
    store.create("q", JSON.stringify({ kind: "dog", owner: "u1" }), idx);
    store.create("q", JSON.stringify({ kind: "dog", owner: "u2" }), idx);
    store.create("q", JSON.stringify({ kind: "cat", owner: "u1" }), idx);
    store.create("q", JSON.stringify({ kind: "dog", owner: "u1" }), idx);

    const res: Entry[] = store.query(
      "q",
      [
        { field: "kind", value: '"dog"' },
        { field: "owner", value: '"u1"' },
      ],
      10,
    );
    assert.equal(res.length, 2);
    for (const e of res) {
      const o = JSON.parse(e.data);
      assert.equal(o.kind, "dog");
      assert.equal(o.owner, "u1");
    }
  });
});
