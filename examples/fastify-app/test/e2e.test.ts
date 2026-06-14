// E2E test for the HTTP-integration example. Exercises the full guarded flow
// through Fastify (via app.inject) against the REAL auth backend.
//
// Requires the auth stack reachable at AUTH_BASE_URL (default :8001 — see
// comp/README.md for the port-forward). If it's unreachable the whole suite is
// skipped (not failed), so this is safe to run in CI without the cluster.
//
//   AUTH_BASE_URL=http://localhost:8001 npm test

import { after, before, describe, it } from "node:test";
import assert from "node:assert/strict";
import type { FastifyInstance } from "fastify";
import { buildApp } from "../src/app.js";

const AUTH_BASE_URL = process.env.AUTH_BASE_URL ?? "http://localhost:8001";

async function authReachable(): Promise<boolean> {
  try {
    const res = await fetch(`${AUTH_BASE_URL}/login`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ email: "x", password: "y", tenant: "t" }),
      signal: AbortSignal.timeout(2000),
    });
    return res.status === 401; // reachable + rejecting bad creds
  } catch {
    return false;
  }
}

const reachable = await authReachable();

describe("fastify-app e2e (HTTP -> wasmCloud auth)", { skip: reachable ? false : `auth backend unreachable at ${AUTH_BASE_URL}` }, () => {
  let app: FastifyInstance;
  // Unique email per run so re-runs don't collide on already-exists.
  const email = `e2e-${Date.now()}@test.com`;
  const password = "hunter2hunter";
  const tenant = "acme";
  let token: string;

  before(async () => {
    app = await buildApp({ baseUrl: AUTH_BASE_URL });
    await app.ready();
  });
  after(async () => {
    await app.close();
  });

  it("serves a public route without auth", async () => {
    const res = await app.inject({ method: "GET", url: "/public" });
    assert.equal(res.statusCode, 200);
    assert.equal(res.json().ok, true);
  });

  it("rejects a guarded route with no token (401)", async () => {
    const res = await app.inject({ method: "GET", url: "/orders" });
    assert.equal(res.statusCode, 401);
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

  it("rejects duplicate registration (409)", async () => {
    const res = await app.inject({
      method: "POST",
      url: "/auth/register",
      payload: { email, password, tenant },
    });
    assert.equal(res.statusCode, 409);
  });

  it("logs in and returns a token (200)", async () => {
    const res = await app.inject({
      method: "POST",
      url: "/auth/login",
      payload: { email, password, tenant },
    });
    assert.equal(res.statusCode, 200);
    token = res.json().access_token;
    assert.match(token, /^sess_/);
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

  it("denies a guarded route the principal lacks (403)", async () => {
    const res = await app.inject({
      method: "GET",
      url: "/orders",
      headers: { authorization: `Bearer ${token}` },
    });
    assert.equal(res.statusCode, 403);
    assert.equal(res.json().error, "insufficient_scope");
  });

  it("logs out (204) and the token is then rejected (401)", async () => {
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
