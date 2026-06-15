// In-process checks for the notify:dispatch component via jco.
//
// IMPORTANT — outbound HTTP is NOT exercised here. The component performs a
// blocking outbound request (`wasi:http/outgoing-handler` + a WASI pollable
// `.block()`), and jco's preview2-shim backs outbound HTTP with async `fetch`.
// In Node's single-threaded event loop, blocking the guest while fetch needs
// the loop to make progress deadlocks. So a live send cannot be driven in
// in-process jco — the HTTP path is validated under wasmCloud with a real
// http-client provider (see infra/wadm.yaml) instead.
//
// What IS verifiable in-process: the config-gated channel routing. With no
// gateway URL configured, email/sms return `unsupported-channel` BEFORE any
// network call — a pure synchronous path that proves the component loads,
// reads config, and branches correctly.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { dispatcher } from "../gen/notify_dispatch.js";

describe("notify:dispatch component (in-process: config routing only)", () => {
  it("email with no notify:email-url -> unsupported-channel", () => {
    assert.throws(
      () => dispatcher.send({ channel: "email", target: "a@b.com", subject: "x", body: "y" }),
      (e: { payload?: { tag: string; val?: string } }) =>
        e?.payload?.tag === "unsupported-channel",
    );
  });

  it("sms with no notify:sms-url -> unsupported-channel", () => {
    assert.throws(
      () => dispatcher.send({ channel: "sms", target: "+1555", subject: "", body: "y" }),
      (e: { payload?: { tag: string } }) => e?.payload?.tag === "unsupported-channel",
    );
  });
});
