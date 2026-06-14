// E2E test for the jco-embed example. Fully self-contained: the auth-guard
// component runs in-process (jco) with an in-memory keyvalue shim, so this
// needs NO cluster, NO NATS, NO network. Ideal for CI.
//
// Requires `npm run transpile` first (produces ./gen). The test runner script
// does this automatically.

import { after, before, describe, it } from "node:test";
import assert from "node:assert/strict";
import type { FastifyInstance } from "fastify";
import { buildApp } from "../src/app.js";

describe("jco-embed e2e (component in-process)", () => {
  let app: FastifyInstance;
  const email = "embed@test.com";
  const password = "hunter2hunter";
  const tenant = "acme";
  let token: string;

  before(async () => {
    app = buildApp();
    await app.ready();
  });
  after(async () => {
    await app.close();
  });

  it("registers a new account (201)", async () => {
    const res = await app.inject({
      method: "POST",
      url: "/auth/register",
      payload: { email, password, tenant },
    });
    assert.equal(res.statusCode, 201);
    assert.match(res.json().subject, /^usr_/);
  });

  it("enforces the configured password-min-len (400)", async () => {
    // default-tenant/password-min-len come from src/shims/config.js (min 8).
    const res = await app.inject({
      method: "POST",
      url: "/auth/register",
      payload: { email: "short@test.com", password: "short", tenant },
    });
    assert.equal(res.statusCode, 400);
    assert.equal(res.json().error, "malformed");
  });

  it("rejects duplicate registration (409)", async () => {
    const res = await app.inject({
      method: "POST",
      url: "/auth/register",
      payload: { email, password, tenant },
    });
    assert.equal(res.statusCode, 409);
  });

  it("rejects bad credentials (401)", async () => {
    const res = await app.inject({
      method: "POST",
      url: "/auth/login",
      payload: { email, password: "wrongpassword", tenant },
    });
    assert.equal(res.statusCode, 401);
    assert.equal(res.json().error, "invalid_credentials");
  });

  it("logs in and returns a session token", async () => {
    const res = await app.inject({
      method: "POST",
      url: "/auth/login",
      payload: { email, password, tenant },
    });
    assert.equal(res.statusCode, 200);
    const body = res.json();
    token = body.accessToken;
    assert.match(token, /^sess_/);
    assert.equal(body.expiresIn, 3600); // from config shim
  });

  it("returns the principal for /auth/me (200)", async () => {
    const res = await app.inject({
      method: "GET",
      url: "/auth/me",
      headers: { authorization: `Bearer ${token}` },
    });
    assert.equal(res.statusCode, 200);
    assert.equal(res.json().tenant, tenant);
  });

  it("denies a route the principal lacks permission for (403)", async () => {
    const res = await app.inject({
      method: "GET",
      url: "/orders",
      headers: { authorization: `Bearer ${token}` },
    });
    assert.equal(res.statusCode, 403);
    assert.equal(res.json().error, "insufficient_scope");
  });

  it("rejects a missing token on a guarded route (401)", async () => {
    const res = await app.inject({ method: "GET", url: "/orders" });
    assert.equal(res.statusCode, 401);
  });

  it("refreshes a token (rotates) and detects reuse of the old one", async () => {
    // fresh login to get a refresh token
    const login = await app.inject({
      method: "POST",
      url: "/auth/login",
      payload: { email, password, tenant },
    });
    const first = login.json().refreshToken as string;
    assert.match(first, /^ref_/);

    // first refresh succeeds, returns a NEW pair
    const r1 = await app.inject({
      method: "POST",
      url: "/auth/refresh",
      payload: { refresh_token: first },
    });
    assert.equal(r1.statusCode, 200);
    const second = r1.json().refreshToken as string;
    assert.notEqual(second, first);

    // REUSING the now-rotated first token is a breach -> 401, family revoked
    const reuse = await app.inject({
      method: "POST",
      url: "/auth/refresh",
      payload: { refresh_token: first },
    });
    assert.equal(reuse.statusCode, 401);

    // and the breach revoked the family, so the rotated token is dead too
    const after = await app.inject({
      method: "POST",
      url: "/auth/refresh",
      payload: { refresh_token: second },
    });
    assert.equal(after.statusCode, 401);
  });

  it("logs out (204) and the session is then invalid (401)", async () => {
    const out = await app.inject({
      method: "POST",
      url: "/auth/logout",
      headers: { authorization: `Bearer ${token}` },
    });
    assert.equal(out.statusCode, 204);

    const me = await app.inject({
      method: "GET",
      url: "/auth/me",
      headers: { authorization: `Bearer ${token}` },
    });
    assert.equal(me.statusCode, 401);
  });
});
