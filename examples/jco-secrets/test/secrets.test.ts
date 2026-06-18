// E2E for the secrets:vault component, run in-process via jco (in-memory kv
// shim + test master-key from config-shim). Covers put/version, plaintext
// round-trip, version bump, old-version overlap during rotation, rotate tuple,
// describe, listNames, delete, and not-found errors. Envelope encryption is
// internal — we only assert that plaintext round-trips.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { vault } from "../gen/secrets_vault.js";

const enc = (s: string) => new TextEncoder().encode(s);
const dec = (b: Uint8Array) => new TextDecoder().decode(b);

const notFound = (e: { payload?: { tag: string } }) =>
  e?.payload?.tag === "not-found";

describe("secrets:vault component", () => {
  it("put returns version 1 and get round-trips the plaintext", () => {
    const meta = vault.put("db-password", enc("hunter2"));
    assert.equal(meta.name, "db-password");
    assert.equal(meta.version, 1);
    assert.equal(dec(vault.get("db-password")), "hunter2");
  });

  it("a second put bumps the version to 2", () => {
    vault.put("api-token", enc("v1-secret"));
    const meta = vault.put("api-token", enc("v2-secret"));
    assert.equal(meta.version, 2);
    assert.equal(dec(vault.get("api-token")), "v2-secret");
  });

  it("getVersion still returns the OLD value after a bump (overlap)", () => {
    vault.put("rotating", enc("old-value"));
    vault.put("rotating", enc("new-value"));
    assert.equal(dec(vault.getVersion("rotating", 1)), "old-value");
    assert.equal(dec(vault.getVersion("rotating", 2)), "new-value");
  });

  it("rotate returns [newVersion, prevVersion]", () => {
    vault.put("signing-key", enc("seed"));
    const [newVersion, prevVersion] = vault.rotate("signing-key", enc("rolled"));
    assert.equal(prevVersion, 1);
    assert.equal(newVersion, 2);
    assert.equal(dec(vault.get("signing-key")), "rolled");
  });

  it("describe returns current meta without throwing", () => {
    vault.put("described", enc("payload"));
    const meta = vault.describe("described");
    assert.equal(meta.name, "described");
    assert.equal(meta.version, 1);
    assert.ok(typeof meta.updated === "bigint" || typeof meta.updated === "number");
  });

  it("listNames includes a stored secret", () => {
    vault.put("listed-secret", enc("x"));
    const names = vault.listNames(100);
    assert.ok(names.includes("listed-secret"));
  });

  it("delete then get throws not-found", () => {
    vault.put("ephemeral", enc("temp"));
    vault.delete("ephemeral");
    assert.throws(() => vault.get("ephemeral"), notFound);
  });

  it("get on an unknown name throws not-found", () => {
    assert.throws(() => vault.get("does-not-exist"), notFound);
  });
});
