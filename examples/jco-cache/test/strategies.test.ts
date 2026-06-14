// E2E for the four caching strategies, exercised against the in-process cache
// component with a fake backing store (src/backing.js).

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { cache } from "../gen/cache.js";
// the same module the component's source/sink imports are mapped to
import { __backing, __seed } from "../src/backing.js";

const dec = (b: Uint8Array | undefined) => (b ? new TextDecoder().decode(b) : undefined);
const decB = (b: Uint8Array | undefined) => (b ? new TextDecoder().decode(b) : undefined);

describe("caching strategies", () => {
  it("cache-aside: consumer checks, loads on miss, sets (pure pattern)", () => {
    // cache-aside is consumer-orchestrated — just get/set, no callback.
    let loads = 0;
    const load = (k: string) => {
      loads++;
      return `db:${k}`;
    };
    const aside = (k: string): string => {
      const hit = cache.get(k);
      if (hit) return dec(hit)!;
      const v = load(k);
      cache.set(k, new TextEncoder().encode(v), 60n);
      return v;
    };
    assert.equal(aside("x"), "db:x");
    assert.equal(aside("x"), "db:x"); // second call is a hit
    assert.equal(loads, 1, "loaded once, then cached");
  });

  it("read-through: get-through loads from source on miss, then caches", () => {
    __seed("rt-key", "from-source");
    // first call: miss -> source.load -> cache
    const v1 = cache.getThrough("rt-key", 60n);
    assert.equal(dec(v1), "from-source");
    // now it's cached: removing from backing must not affect the hit
    __backing.delete("rt-key");
    const v2 = cache.getThrough("rt-key", 60n);
    assert.equal(dec(v2), "from-source");
    // absent in source -> none
    assert.equal(cache.getThrough("missing", 60n), undefined);
  });

  it("write-through: put-through writes cache AND backing synchronously", () => {
    cache.putThrough("wt-key", new TextEncoder().encode("v1"), 60n);
    assert.equal(dec(cache.get("wt-key")), "v1", "in cache");
    assert.equal(decB(__backing.get("wt-key")), "v1", "in backing immediately");
  });

  it("write-behind: put-behind caches now, flush drains to backing later", () => {
    cache.putBehind("wb-key", new TextEncoder().encode("later"), 60n);
    assert.equal(dec(cache.get("wb-key")), "later", "cached immediately");
    assert.equal(__backing.get("wb-key"), undefined, "NOT in backing yet");

    const n = cache.flush();
    assert.ok(n >= 1, "flushed at least one write");
    assert.equal(decB(__backing.get("wb-key")), "later", "now in backing");

    // a second flush has nothing new to drain
    assert.equal(cache.flush(), 0);
  });
});
