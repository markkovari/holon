// E2E for the id:generate component, run in-process via jco. No shim needed:
// the component only imports wasi:clocks/wall-clock and wasi:random/random,
// both auto-shimmed by jco's transpile output.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { generator as id } from "../gen/id_generate.js";

// Crockford base32: 0-9 A-Z minus I L O U.
const ULID_RE = /^[0-9A-HJKMNP-TV-Z]{26}$/;
const UUID_V4_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const NANOID_RE = /^[A-Za-z0-9_-]+$/;
// Unambiguous short-code alphabet: no 0 O 1 I L U.
const SHORTCODE_RE = /^[23456789ABCDEFGHJKMNPQRSTUVWXYZ]+$/;

describe("id:generate component", () => {
  it("ulid() is 26 Crockford-base32 chars, unique, and strictly increasing", () => {
    const u = id.ulid();
    assert.equal(u.length, 26);
    assert.match(u, ULID_RE);

    // Monotonic-within-ms: back-to-back calls in the same millisecond increment
    // the random tail, so every id is distinct AND each strictly sorts after the
    // previous one. (This guards the encoder bug where the low bit was dropped
    // and id/id+1 collided.)
    const all: string[] = [];
    for (let i = 0; i < 1000; i++) all.push(id.ulid());
    assert.equal(new Set(all).size, 1000, "all 1000 ulids distinct");
    for (let i = 1; i < all.length; i++) {
      assert.ok(all[i] > all[i - 1], "ulids strictly increase in creation order");
    }
  });

  it("ulid() is sortable: creation order is lexicographically non-decreasing", () => {
    const ids: string[] = [];
    for (let i = 0; i < 50; i++) ids.push(id.ulid());
    // Time only moves forward; with within-ms monotonicity this is strictly
    // increasing, otherwise at least non-decreasing.
    for (let i = 0; i + 1 < ids.length; i++) {
      assert.ok(
        ids[i] <= ids[i + 1],
        `ulid order broke at ${i}: ${ids[i]} > ${ids[i + 1]}`,
      );
    }
  });

  it("ulidAt(ms) yields valid ulids; same ms differs in random part", () => {
    const a = id.ulidAt(1469918176385n);
    const b = id.ulidAt(1469918176385n);
    assert.match(a, ULID_RE);
    assert.match(b, ULID_RE);
    assert.equal(a.length, 26);
    assert.notEqual(a, b, "two ulids at the same ms should differ (random)");
  });

  it("uuidV4() matches the v4 format and mints unique ids", () => {
    assert.match(id.uuidV4(), UUID_V4_RE);

    const seen = new Set<string>();
    for (let i = 0; i < 1000; i++) seen.add(id.uuidV4());
    assert.equal(seen.size, 1000, "1000 uuids should all be unique");
  });

  it("nanoid(length) is url-safe, clamps length, and mints unique ids", () => {
    const n = id.nanoid(21);
    assert.equal(n.length, 21);
    assert.match(n, NANOID_RE);

    assert.ok(id.nanoid(0).length >= 1, "nanoid(0) clamps to >= 1");
    assert.ok(id.nanoid(100).length <= 64, "nanoid(100) clamps to <= 64");

    const seen = new Set<string>();
    for (let i = 0; i < 1000; i++) seen.add(id.nanoid(21));
    assert.ok(seen.size > 990, "1000 nanoids should be ~all unique");
  });

  it("shortCode(length) uses only the unambiguous alphabet", () => {
    const c = id.shortCode(8);
    assert.equal(c.length, 8);
    assert.match(c, SHORTCODE_RE);
  });
});
