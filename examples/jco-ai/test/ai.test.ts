import { test } from "node:test";
import assert from "node:assert/strict";

// The composed component exports ONLY ai:inference/inference@0.1.0.
// The llm:inference import is satisfied internally by the wac-plugged MOCK
// provider, so output here is fully deterministic and offline.
import { inference as ai } from "../gen/ai_inference.composed.js";

test("summarize a pet evaluation for a doctor (mock -> 'Summary: ...')", () => {
  const out = ai.summarize(
    "Bella is a 4yo Labrador presenting with limping on the left hind leg, mild swelling around the stifle joint.",
    "brief",
    "clinical findings",
  );
  assert.equal(typeof out, "string");
  assert.ok(out.startsWith("Summary:"), `expected a 'Summary:' line, got: ${out}`);
});

test("classify triages a complaint to the first matching label", () => {
  const res = ai.classify("severe bleeding, collapsed", ["urgent", "routine"]);
  assert.ok(["urgent", "routine"].includes(res.label));
  // The mock returns the FIRST label deterministically.
  assert.equal(res.label, "urgent");
  assert.equal(res.confidence, 1000);
});

test("classify rejects an empty label set with invalid-request", () => {
  assert.throws(
    () => ai.classify("x", []),
    (err: any) => err?.payload?.tag === "invalid-request",
  );
});

test("extract returns a pair per requested field, in order", () => {
  const pairs = ai.extract("Species: dog. Symptom: limping.", ["species", "symptom"]);
  // The mock fills every requested field with "mock-<field>" (deterministic).
  assert.deepEqual(pairs, [
    ["species", "mock-species"],
    ["symptom", "mock-symptom"],
  ]);
});

test("generate produces a non-empty string (mock echoes the prompt)", () => {
  const out = ai.generate("Write a reminder", "appointment tomorrow 10am");
  assert.equal(typeof out, "string");
  assert.ok(out.length > 0);
});

test("rewrite produces a non-empty string", () => {
  const out = ai.rewrite("The felis catus is in good health", "for a pet owner");
  assert.equal(typeof out, "string");
  assert.ok(out.length > 0);
});

test("embed returns a deterministic 8-dim finite vector", () => {
  const v = ai.embed("golden retriever");
  assert.equal(v.length, 8);
  for (const x of v) {
    assert.ok(Number.isFinite(x), `non-finite component: ${x}`);
  }
  // Deterministic: same text -> identical vector.
  const v2 = ai.embed("golden retriever");
  assert.deepEqual(Array.from(v), Array.from(v2));
});
