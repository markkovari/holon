// E2E for the sched:timer component, run in-process via jco (in-memory kv +
// atomics shims; the wall-clock import is jco's default host clock). Covers
// one-shot schedule-at -> due (leased) -> ack (gone), recurring schedule-every
// (advances run-at, catches up past a backlog), idempotent re-keying, cancel,
// and the not-found / invalid-period error surface. `due` takes an explicit
// `now` so time-dependent assertions are deterministic.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { timer } from "../gen/timer.js";

const enc = (s: string) => new TextEncoder().encode(s);
const dec = (b: Uint8Array) => new TextDecoder().decode(b);

// Pick distinct keys per test so the shared in-memory store never collides.
const T0 = 1_700_000_000n; // a fixed "now" baseline (unix seconds)

describe("sched:timer component", () => {
  it("schedule-at + due returns a one-shot once it is due", () => {
    timer.scheduleAt("appt-42-reminder", T0 + 100n, enc("remind owner"));
    // not due yet
    const early = timer.due(T0 + 50n, 10, 60n);
    assert.ok(!early.some((j) => j.key === "appt-42-reminder"), "future job not yet due");
    // due now
    const ready = timer.due(T0 + 100n, 10, 60n);
    const job = ready.find((j) => j.key === "appt-42-reminder");
    assert.ok(job, "job should be due at run-at");
    assert.equal(job!.kind, "once");
    assert.equal(dec(job!.payload), "remind owner");
  });

  it("a one-shot is leased: a second immediate due does not re-return it", () => {
    timer.scheduleAt("lease-job", T0, enc("x"));
    const first = timer.due(T0, 10, 600n);
    assert.ok(first.some((j) => j.key === "lease-job"), "first due leases it");
    const second = timer.due(T0, 10, 600n);
    assert.ok(!second.some((j) => j.key === "lease-job"), "leased one-shot must not reappear");
  });

  it("a lapsed lease makes a one-shot due again (crash-safe at-least-once)", () => {
    timer.scheduleAt("relapse-job", T0, enc("y"));
    const first = timer.due(T0, 10, 30n); // 30s lease
    assert.ok(first.some((j) => j.key === "relapse-job"));
    // after the lease window, it's due again (relay crashed before ack)
    const again = timer.due(T0 + 31n, 10, 30n);
    assert.ok(again.some((j) => j.key === "relapse-job"), "expired lease re-arms the job");
  });

  it("ack removes a fired one-shot; acking an unknown key throws not-found", () => {
    timer.scheduleAt("ack-job", T0, enc("z"));
    timer.due(T0, 10, 60n);
    timer.ack("ack-job");
    assert.equal(timer.peek("ack-job"), undefined, "acked one-shot is gone");
    assert.throws(
      () => timer.ack("nope-never-scheduled"),
      (e: { payload?: { tag?: string } }) => e?.payload?.tag === "not-found",
    );
  });

  it("schedule-every fires repeatedly, advancing run-at by the period", () => {
    timer.scheduleEvery("nightly", 100n, T0, enc("sweep"));
    const r1 = timer.due(T0, 10, 60n);
    const j1 = r1.find((j) => j.key === "nightly");
    assert.ok(j1, "recurring job due at first-run-at");
    assert.equal(j1!.kind, "every");
    assert.equal(j1!.fires, 1);
    // immediately after firing, the next run-at is in the future -> not due
    const r2 = timer.due(T0 + 1n, 10, 60n);
    assert.ok(!r2.some((j) => j.key === "nightly"), "advanced past current now");
    // at the next slot, it fires again
    const r3 = timer.due(T0 + 100n, 10, 60n);
    const j3 = r3.find((j) => j.key === "nightly");
    assert.ok(j3, "fires again one period later");
    assert.equal(j3!.fires, 2);
  });

  it("a recurring job catches up: a long outage fires once, not a backlog", () => {
    timer.scheduleEvery("hourly", 3600n, T0, enc("tick"));
    // relay is down for ~10 hours; one due call should fire ONCE and advance
    // to the next future slot, not return 10 stacked fires.
    const fired = timer.due(T0 + 36000n, 100, 60n).filter((j) => j.key === "hourly");
    assert.equal(fired.length, 1, "catch-up collapses missed windows into one fire");
    const next = timer.peek("hourly");
    assert.ok(next, "job persists");
    assert.ok(next!.runAt > T0 + 36000n, "next run-at is strictly in the future");
  });

  it("schedule with the same key REPLACES the prior job (idempotent)", () => {
    timer.scheduleAt("dup", T0 + 10n, enc("first"));
    timer.scheduleAt("dup", T0 + 20n, enc("second"));
    const all = timer.listJobs(100).filter((j) => j.key === "dup");
    assert.equal(all.length, 1, "same key is one job, not two");
    const job = timer.peek("dup");
    assert.equal(dec(job!.payload), "second", "re-schedule replaced the payload");
    assert.equal(job!.runAt, T0 + 20n);
  });

  it("cancel removes a job; cancelling an unknown key throws not-found", () => {
    timer.scheduleAt("cancel-me", T0 + 5n, enc("bye"));
    timer.cancel("cancel-me");
    assert.equal(timer.peek("cancel-me"), undefined);
    assert.throws(
      () => timer.cancel("ghost"),
      (e: { payload?: { tag?: string } }) => e?.payload?.tag === "not-found",
    );
  });

  it("schedule-every with period 0 throws invalid-period", () => {
    assert.throws(
      () => timer.scheduleEvery("bad", 0n, T0, enc("nope")),
      (e: { payload?: { tag?: string } }) => e?.payload?.tag === "invalid-period",
    );
  });
});
