// E2E for the audit:log component, run in-process via jco (in-memory kv shim).
// Covers record-event (id/timestamp stamping), recent (newest-first), and
// by-trace filtering.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { recorder, query } from "../gen/audit_log.js";

const ev = (over: Partial<Parameters<typeof recorder.recordEvent>[0]> = {}) => ({
  id: "",
  traceId: "",
  spanId: "",
  timestamp: 0n,
  event: "authorize",
  outcome: "allow",
  tenant: "acme",
  subject: "u1",
  detail: "orders:read",
  ...over,
});

describe("audit:log component", () => {
  it("records an event and stamps id + timestamp when empty", () => {
    recorder.recordEvent(ev({ event: "login", outcome: "allow", subject: "alice" }));
    const recent = query.recent(10);
    const found = recent.find((e) => e.subject === "alice");
    assert.ok(found, "event was persisted");
    assert.notEqual(found.id, "", "id was stamped");
    assert.ok(found.timestamp > 0, "timestamp was stamped");
  });

  it("recent returns newest first", () => {
    recorder.recordEvent(ev({ timestamp: 1000n, subject: "older" }));
    recorder.recordEvent(ev({ timestamp: 2000n, subject: "newer" }));
    const recent = query.recent(50);
    const iOlder = recent.findIndex((e) => e.subject === "older");
    const iNewer = recent.findIndex((e) => e.subject === "newer");
    assert.ok(iNewer < iOlder, "newer event sorts before older");
  });

  it("by-trace filters to one trace id", () => {
    const tid = "00112233445566778899aabbccddeeff";
    recorder.recordEvent(ev({ traceId: tid, detail: "first" }));
    recorder.recordEvent(ev({ traceId: tid, detail: "second" }));
    recorder.recordEvent(ev({ traceId: "ffffffffffffffffffffffffffffffff", detail: "other" }));
    const trail = query.byTrace(tid);
    assert.equal(trail.length, 2);
    assert.ok(trail.every((e) => e.traceId === tid));
  });
});
