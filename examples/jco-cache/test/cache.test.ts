// E2E for the cache:store component, run in-process via jco (in-memory kv shim).
// Covers get/set, miss, TTL expiry, no-expiry, ttl(), delete/invalidate, and
// invalidate-prefix.

import { after, describe, it } from "node:test";
import assert from "node:assert/strict";
import { cache } from "../gen/cache.js";

const enc = (s: string) => new TextEncoder().encode(s);
const dec = (b: Uint8Array | undefined) => new TextDecoder().decode(b);
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

describe("cache:store component", () => {
  it("set then get returns the value", () => {
    cache.set("a", enc("hello"), 60n);
    const v = cache.get("a");
    assert.ok(v);
    assert.equal(dec(v), "hello");
  });

  it("missing key is a miss (undefined/none)", () => {
    assert.equal(cache.get("nope"), undefined);
  });

  it("ttl() reports remaining seconds, or none for no-expiry", () => {
    cache.set("ttl-key", enc("x"), 100n);
    const t = cache.ttl("ttl-key");
    assert.ok(typeof t === "bigint" && t > 0n && t <= 100n);
    cache.set("forever", enc("x"), 0n);
    assert.equal(cache.ttl("forever"), 0n); // 0 = stored, no expiry
    assert.equal(cache.ttl("absent"), undefined);
  });

  it("expires after its TTL", async () => {
    cache.set("short", enc("v"), 1n);
    assert.ok(cache.get("short"), "fresh immediately");
    await sleep(1100);
    assert.equal(cache.get("short"), undefined, "gone after 1s");
  });

  it("delete / invalidate remove an entry", () => {
    cache.set("d", enc("v"), 60n);
    cache.invalidate("d");
    assert.equal(cache.get("d"), undefined);
  });

  it("invalidate-prefix removes all matching keys and counts them", () => {
    cache.set("user:1:name", enc("a"), 60n);
    cache.set("user:1:email", enc("b"), 60n);
    cache.set("user:2:name", enc("c"), 60n);
    const n = cache.invalidatePrefix("user:1:");
    assert.equal(n, 2);
    assert.equal(cache.get("user:1:name"), undefined);
    assert.equal(cache.get("user:1:email"), undefined);
    assert.ok(cache.get("user:2:name"), "other prefix untouched");
  });

  after(() => {});
});
