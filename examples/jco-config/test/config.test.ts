// E2E for the config:store component, run in-process via jco (in-memory kv
// shim). Covers typed values, versioning, compare-and-swap via set-if,
// listing, idempotent delete, and namespace isolation.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { store as config } from "../gen/config_store.js";

describe("config:store component", () => {
  it("set returns version 1 for a fresh key", () => {
    const v = config.set("svc", "greeting", { tag: "text", val: "hello" });
    assert.equal(v, 1);
  });

  it("get returns the typed value at version 1", () => {
    const got = config.get("svc", "greeting");
    assert.deepEqual(got.value, { tag: "text", val: "hello" });
    assert.equal(got.version, 1);
  });

  it("set again bumps the version to 2", () => {
    const v = config.set("svc", "greeting", { tag: "text", val: "bonjour" });
    assert.equal(v, 2);
    assert.equal(config.get("svc", "greeting").version, 2);
  });

  it("set-if with the correct expected version succeeds -> version 3", () => {
    const v = config.setIf(
      "svc",
      "greeting",
      { tag: "text", val: "ciao" },
      2,
    );
    assert.equal(v, 3);
    assert.equal(config.get("svc", "greeting").value.val, "ciao");
  });

  it("set-if with a stale expected version throws version-conflict", () => {
    assert.throws(
      () =>
        config.setIf("svc", "greeting", { tag: "text", val: "nope" }, 2),
      (e: { payload?: { tag: string; val?: number } }) =>
        e?.payload?.tag === "version-conflict" && e.payload.val === 3,
    );
  });

  it("get throws not-found for an unset key", () => {
    assert.throws(
      () => config.get("svc", "missing"),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "not-found",
    );
  });

  it("integer value round-trips as bigint", () => {
    config.set("svc", "max-retries", { tag: "integer", val: 42n });
    const got = config.get("svc", "max-retries");
    assert.equal(got.value.tag, "integer");
    assert.equal(got.value.val, 42n);
  });

  it("boolean value round-trips", () => {
    config.set("svc", "feature-on", { tag: "boolean", val: true });
    const got = config.get("svc", "feature-on");
    assert.deepEqual(got.value, { tag: "boolean", val: true });
  });

  it("decimal (f64) value round-trips as number", () => {
    config.set("svc", "ratio", { tag: "decimal", val: 0.75 });
    const got = config.get("svc", "ratio");
    assert.equal(got.value.tag, "decimal");
    assert.equal(got.value.val, 0.75);
  });

  it("json value round-trips", () => {
    const payload = '{"a":1,"b":[true,null]}';
    config.set("svc", "blob", { tag: "json", val: payload });
    const got = config.get("svc", "blob");
    assert.deepEqual(got.value, { tag: "json", val: payload });
  });

  it("keys lists the keys set in a namespace", () => {
    const keys = config.keys("svc", 100);
    for (const k of ["greeting", "max-retries", "feature-on", "ratio", "blob"]) {
      assert.ok(keys.includes(k), `expected keys to include ${k}`);
    }
  });

  it("delete returns true, then get throws not-found", () => {
    config.set("svc", "temp", { tag: "text", val: "x" });
    assert.equal(config.delete("svc", "temp"), true);
    assert.throws(
      () => config.get("svc", "temp"),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "not-found",
    );
  });

  it("delete is idempotent: a second delete returns false", () => {
    assert.equal(config.delete("svc", "temp"), false);
  });

  it("namespaces isolate the same key", () => {
    config.set("ns-a", "shared", { tag: "text", val: "from-a" });
    config.set("ns-b", "shared", { tag: "text", val: "from-b" });
    assert.equal(config.get("ns-a", "shared").value.val, "from-a");
    assert.equal(config.get("ns-b", "shared").value.val, "from-b");
  });
});
