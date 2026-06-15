// E2E for the blob:store component, run in-process via jco (in-memory kv shim).
// Covers put/get round-trip, head metadata, exists, delete, container scoping,
// prefix listing, and not-found.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { blobstore as blob } from "../gen/blob_store.js";

const enc = (s: string) => new TextEncoder().encode(s);
const dec = (b: Uint8Array) => new TextDecoder().decode(b);

describe("blob:store component", () => {
  it("put then get round-trips the bytes", () => {
    blob.put("uploads", "hello.txt", enc("hello world"), "text/plain");
    assert.equal(dec(blob.get("uploads", "hello.txt")), "hello world");
  });

  it("head returns size + content-type without the bytes", () => {
    blob.put("uploads", "doc.json", enc('{"a":1}'), "application/json");
    const info = blob.head("uploads", "doc.json");
    assert.equal(info.name, "doc.json");
    assert.equal(info.size, 7n);
    assert.equal(info.contentType, "application/json");
  });

  it("get / head on an absent object is not-found", () => {
    assert.throws(
      () => blob.get("uploads", "nope"),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "not-found",
    );
    assert.equal(blob.exists("uploads", "nope"), false);
  });

  it("delete removes the object (idempotent)", () => {
    blob.put("uploads", "tmp.bin", enc("x"), "");
    assert.equal(blob.exists("uploads", "tmp.bin"), true);
    blob.delete("uploads", "tmp.bin");
    assert.equal(blob.exists("uploads", "tmp.bin"), false);
    blob.delete("uploads", "tmp.bin"); // idempotent — no throw
  });

  it("containers are isolated", () => {
    blob.put("a", "k", enc("from-a"), "");
    blob.put("b", "k", enc("from-b"), "");
    assert.equal(dec(blob.get("a", "k")), "from-a");
    assert.equal(dec(blob.get("b", "k")), "from-b");
  });

  it("list-objects filters by prefix within a container", () => {
    blob.put("imgs", "2024/jan.png", enc("1"), "image/png");
    blob.put("imgs", "2024/feb.png", enc("22"), "image/png");
    blob.put("imgs", "2025/jan.png", enc("333"), "image/png");
    const y2024 = blob.listObjects("imgs", "2024/");
    const names = y2024.map((o) => o.name).sort();
    assert.deepEqual(names, ["2024/feb.png", "2024/jan.png"]);
    // metadata comes back with the listing
    const feb = y2024.find((o) => o.name === "2024/feb.png");
    assert.equal(feb?.size, 2n);
    assert.equal(feb?.contentType, "image/png");
  });

  it("handles binary data with separator-like bytes in names", () => {
    // names with '/' and '_' must round-trip through the sanitizer.
    blob.put("c", "a/b_c.dat", new Uint8Array([0, 255, 47, 95]), "application/octet-stream");
    const got = blob.get("c", "a/b_c.dat");
    assert.deepEqual([...got], [0, 255, 47, 95]);
    assert.ok(blob.listObjects("c", "a/").some((o) => o.name === "a/b_c.dat"));
  });
});
