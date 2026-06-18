// E2E for the paginate:cursor component, run in-process via jco. Covers cursor
// round-trip, tamper/garbage rejection, limit clamping, and page assembly with
// directional cursors. The cursor-secret + max-page-size come from config-shim.js.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { cursors } from "../gen/pagination.js";

const isTag = (tag: string) => (e: { payload?: { tag: string } }) =>
  e?.payload?.tag === tag;

describe("paginate:cursor component", () => {
  it("encode then decode round-trips a position", () => {
    const pos = { sortKey: "2026-06-18T10:00:00Z", lastId: "row-42", forward: true };
    const cursor = cursors.encode(pos);
    assert.equal(typeof cursor, "string");
    const back = cursors.decode(cursor);
    assert.equal(back.sortKey, pos.sortKey);
    assert.equal(back.lastId, pos.lastId);
    assert.equal(back.forward, pos.forward);
  });

  it("decode of a tampered cursor throws invalid-cursor", () => {
    const cursor = cursors.encode({ sortKey: "k", lastId: "id", forward: true });
    // flip one character to break the HMAC
    const flipped = cursor[0] === "A" ? "B" : "A";
    const tampered = flipped + cursor.slice(1);
    assert.throws(() => cursors.decode(tampered), isTag("invalid-cursor"));
  });

  it("decode of garbage throws invalid-cursor", () => {
    assert.throws(() => cursors.decode("not-a-cursor"), isTag("invalid-cursor"));
  });

  it("clampLimit accepts, rejects zero, and clamps to max-page-size", () => {
    assert.equal(cursors.clampLimit(50), 50);
    assert.throws(() => cursors.clampLimit(0), isTag("bad-limit"));
    assert.equal(cursors.clampLimit(500), 100); // max-page-size = 100
  });

  it("buildPage emits a forward nextCursor when there is more after", () => {
    const last = { sortKey: "k-last", lastId: "id-last", forward: true };
    const page = cursors.buildPage(undefined, last, false, true);
    assert.equal(typeof page.nextCursor, "string");
    assert.equal(page.hasNext, true);
    assert.equal(cursors.decode(page.nextCursor!).forward, true);
  });

  it("buildPage emits a backward prevCursor when there is more before", () => {
    const first = { sortKey: "k-first", lastId: "id-first", forward: true };
    const page = cursors.buildPage(first, undefined, true, false);
    assert.equal(typeof page.prevCursor, "string");
    assert.equal(page.hasPrev, true);
    assert.equal(cursors.decode(page.prevCursor!).forward, false);
  });

  it("buildPage with no neighbours emits no cursors and false flags", () => {
    const page = cursors.buildPage(undefined, undefined, false, false);
    assert.equal(page.nextCursor, undefined);
    assert.equal(page.prevCursor, undefined);
    assert.equal(page.hasNext, false);
    assert.equal(page.hasPrev, false);
  });
});
