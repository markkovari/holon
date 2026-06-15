// E2E for the webhook:ingest component, run in-process via jco. This is the
// COMPOSITION showcase: webhook_ingest.wasm here is composed with
// idempotency-guard (`wac plug`), so the dedup capability runs in-process too —
// a single .wasm chaining two reusable components. The kv shim is shared, so
// the seeded secret and the idempotency records live in the same store.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import { verifier } from "../gen/webhook_ingest.js";
import { __seed } from "../src/keyvalue-shim.js";

const SECRET = "whsec_test_secret";
const SECRET_REF = "webhook-secret";
__seed(SECRET_REF, SECRET);

const enc = (s: string) => new TextEncoder().encode(s);
const sign = (payload: string) =>
  createHmac("sha256", SECRET).update(payload).digest("hex");

describe("webhook:ingest component (composed with idempotency-guard)", () => {
  it("accepts a validly-signed first delivery", () => {
    const body = '{"event":"charge.succeeded","id":1}';
    const v = verifier.ingest(enc(body), sign(body), SECRET_REF, "evt_001");
    assert.deepEqual(v, { accepted: true, replay: false });
  });

  it("treats a repeat delivery-id as a replay (idempotency compose)", () => {
    const body = '{"event":"charge.succeeded","id":2}';
    const first = verifier.ingest(enc(body), sign(body), SECRET_REF, "evt_002");
    assert.deepEqual(first, { accepted: true, replay: false });
    const repeat = verifier.ingest(enc(body), sign(body), SECRET_REF, "evt_002");
    assert.deepEqual(repeat, { accepted: false, replay: true });
  });

  it("rejects a bad signature before dedup", () => {
    const body = '{"event":"charge.succeeded","id":3}';
    assert.throws(
      () => verifier.ingest(enc(body), "deadbeef", SECRET_REF, "evt_003"),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "bad-signature",
    );
  });
});
