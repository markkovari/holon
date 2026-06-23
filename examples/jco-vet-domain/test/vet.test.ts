// E2E for the vet-clinic domain running as ONE composed wasm HTTP component,
// served over real HTTP by jco's WASI HTTPServer (the same incoming-handler a
// wasmCloud http-server provider drives — identical bytes). No Node domain
// logic: every route is the Rust vet-domain component orchestrating auth-guard
// + records:store + validate + search, all linked into the one .wasm.
//
// Flow: seed RBAC -> register owner/doctor -> login -> owner adds a pet
// (validated + indexed) and books an appointment -> search finds the pet ->
// owner is 403 on a visit note -> doctor writes the note. All over HTTP.

import { describe, it, before, after } from "node:test";
import assert from "node:assert/strict";
import { HTTPServer } from "@bytecodealliance/preview2-shim/http";
import * as component from "../gen/vet_domain.composed.js";

const PORT = 3066;
const BASE = `http://localhost:${PORT}`;
const TENANT = "acme-vet";
let server: { listen(p: number): void; stop?(): void };

async function post(path: string, body: unknown, token?: string) {
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (token) headers.authorization = `Bearer ${token}`;
  return fetch(`${BASE}${path}`, { method: "POST", headers, body: JSON.stringify(body) });
}
async function get(path: string, token?: string) {
  const headers: Record<string, string> = {};
  if (token) headers.authorization = `Bearer ${token}`;
  return fetch(`${BASE}${path}`, { headers });
}
async function login(email: string, password: string): Promise<string> {
  const r = await post("/login", { email, password, tenant: TENANT });
  const body = await r.text();
  assert.equal(r.status, 200, `login ${email}: ${body}`);
  return (JSON.parse(body) as { access_token: string }).access_token;
}

describe("vet-domain as one composed wasm HTTP component (jco WASI HTTPServer)", () => {
  before(async () => {
    server = new HTTPServer(component.incomingHandler) as typeof server;
    server.listen(PORT);
    // seed role -> permission maps for the clinic's roles.
    await post("/admin/role-permissions", {
      tenant: TENANT, role: "pet-owner",
      permissions: [
        { target: "pets", action: "read" }, { target: "pets", action: "write" },
        { target: "appointments", action: "read" }, { target: "appointments", action: "write" },
      ],
    });
    await post("/admin/role-permissions", {
      tenant: TENANT, role: "doctor",
      permissions: [
        { target: "pets", action: "read" }, { target: "appointments", action: "read" },
        { target: "appointments", action: "write" }, { target: "notes", action: "write" },
      ],
    });
  });
  after(() => server.stop?.());

  it("register + login an owner and a doctor", async () => {
    const ro = await post("/register", { email: "owner@acme-vet.test", password: "ownerpass1", role: "pet-owner", tenant: TENANT });
    assert.equal(ro.status, 201, await ro.text());
    const rd = await post("/register", { email: "doc@acme-vet.test", password: "docpass1", role: "doctor", tenant: TENANT });
    assert.equal(rd.status, 201, await rd.text());
    await login("owner@acme-vet.test", "ownerpass1");
    await login("doc@acme-vet.test", "docpass1");
  });

  it("owner adds a pet (validated + indexed); search finds it", async () => {
    const token = await login("owner@acme-vet.test", "ownerpass1");
    // validation: empty name -> 400
    const bad = await post("/pets", { name: "", species: "dog" }, token);
    assert.equal(bad.status, 400, "empty name fails validation");
    // valid
    const ok = await post("/pets", { name: "Rex", species: "dog", notes: "good boy" }, token);
    const okBody = await ok.text();
    assert.equal(ok.status, 201, okBody);
    const pet = JSON.parse(okBody) as { id: string; name: string; owner: string };
    assert.equal(pet.name, "Rex");
    assert.ok(pet.id.length > 0, "records:store minted an id");
    // search finds it
    const found = await get("/pets?q=Rex", token);
    const { pets } = (await found.json()) as { pets: { name: string }[] };
    assert.ok(pets.some((p) => p.name === "Rex"), "search:index found Rex");
  });

  it("a missing token is rejected (401)", async () => {
    const r = await get("/pets");
    assert.equal(r.status, 401);
  });

  it("owner books an appointment; lists it", async () => {
    const token = await login("owner@acme-vet.test", "ownerpass1");
    const pets = (await (await get("/pets", token)).json()) as { pets: { id: string }[] };
    const petId = pets.pets[0].id;
    const r = await post("/appointments", { pet: petId, datetime: "2030-05-01T10:00" }, token);
    assert.equal(r.status, 201, await r.text()); // last read of r — ok
    const list = (await (await get("/appointments", token)).json()) as { appointments: { pet: string }[] };
    assert.ok(list.appointments.some((a) => a.pet === petId), "booked appointment is listed");
  });

  it("owner is FORBIDDEN from writing a visit note; doctor can", async () => {
    const owner = await login("owner@acme-vet.test", "ownerpass1");
    const appts = (await (await get("/appointments", owner)).json()) as { appointments: { id: string }[] };
    const apptId = appts.appointments[0].id;
    // owner lacks notes:write -> 403
    const denied = await post(`/appointments/${apptId}/notes`, { text: "should be blocked" }, owner);
    assert.equal(denied.status, 403, "pet-owner lacks notes:write");
    // doctor has notes:write -> 201
    const doc = await login("doc@acme-vet.test", "docpass1");
    const note = await post(`/appointments/${apptId}/notes`, { text: "Initial exam normal." }, doc);
    assert.equal(note.status, 201, await note.text());
  });
});
