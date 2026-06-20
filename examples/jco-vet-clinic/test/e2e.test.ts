// End-to-end flow for the vet-clinic example, driven via Fastify's app.inject
// (no network). Exercises the whole component stack: composed auth-guard
// (accounts/login/RBAC/authorize + audit), search-index, validate,
// notify-dispatch — all in-process over the shared KV.

import { describe, it, before } from "node:test";
import assert from "node:assert/strict";
import type { FastifyInstance } from "fastify";
import { buildApp } from "../src/app.js";

let app: FastifyInstance;

async function login(email: string, password: string): Promise<string> {
  const res = await app.inject({ method: "POST", url: "/auth/login", payload: { email, password } });
  assert.equal(res.statusCode, 200, `login ${email}: ${res.body}`);
  const tp = res.json() as { accessToken?: string; access_token?: string };
  return (tp.accessToken ?? tp.access_token)!;
}
function auth(token: string) {
  return { authorization: `Bearer ${token}` };
}

describe("vet-clinic e2e (composed components, in-process)", () => {
  before(() => {
    // Static disabled in tests (no public dir needed for inject).
    app = buildApp({ serveStatic: false }).app;
  });

  it("seeds 3 demo roles and they can all log in", async () => {
    await login("owner@acme-vet.test", "ownerpass1");
    await login("doctor@acme-vet.test", "doctorpass1");
    await login("admin@acme-vet.test", "adminpass1");
  });

  it("a pet-owner registers, logs in, and is a pet-owner", async () => {
    const reg = await app.inject({
      method: "POST",
      url: "/auth/register",
      payload: { email: "alice@acme-vet.test", password: "alicepass1", role: "pet-owner" },
    });
    assert.equal(reg.statusCode, 201, reg.body);
    const token = await login("alice@acme-vet.test", "alicepass1");
    const me = await app.inject({ method: "GET", url: "/auth/me", headers: auth(token) });
    assert.deepEqual((me.json() as { roles: string[] }).roles, ["pet-owner"]);
  });

  it("owner adds a pet (validated + indexed), then search finds it", async () => {
    const token = await login("alice@acme-vet.test", "alicepass1");
    const bad = await app.inject({ method: "POST", url: "/pets", headers: auth(token), payload: { name: "" } });
    assert.equal(bad.statusCode, 400, "empty name should fail validation");

    const ok = await app.inject({
      method: "POST",
      url: "/pets",
      headers: auth(token),
      payload: { name: "Rex", species: "dog" },
    });
    assert.equal(ok.statusCode, 201, ok.body);

    const found = await app.inject({ method: "GET", url: "/pets?q=Rex", headers: auth(token) });
    const pets = (found.json() as { pets: { name: string }[] }).pets;
    assert.ok(pets.some((p) => p.name === "Rex"), "search should find Rex");
  });

  it("owner books an appointment (notify fires, no throw)", async () => {
    const token = await login("alice@acme-vet.test", "alicepass1");
    const pets = (await app.inject({ method: "GET", url: "/pets", headers: auth(token) }).then((r) => r.json())) as { pets: { id: string }[] };
    const petId = pets.pets[0].id;
    const res = await app.inject({
      method: "POST",
      url: "/appointments",
      headers: auth(token),
      payload: { pet: petId, datetime: "2026-07-01T10:00" },
    });
    assert.equal(res.statusCode, 201, res.body);
  });

  it("owner is FORBIDDEN from writing a visit note (notes:write)", async () => {
    const token = await login("alice@acme-vet.test", "alicepass1");
    const res = await app.inject({
      method: "POST",
      url: "/appointments/appt_x/notes",
      headers: auth(token),
      payload: { text: "should be blocked" },
    });
    assert.equal(res.statusCode, 403, "pet-owner lacks notes:write");
  });

  it("doctor can list appointments and write a note", async () => {
    const doc = await login("doctor@acme-vet.test", "doctorpass1");
    const list = await app.inject({ method: "GET", url: "/appointments", headers: auth(doc) });
    assert.equal(list.statusCode, 200);
    // doctor writes a note on any appointment id (route-level perm is what we assert)
    const owner = await login("alice@acme-vet.test", "alicepass1");
    const appts = (await app.inject({ method: "GET", url: "/appointments", headers: auth(owner) }).then((r) => r.json())) as { appointments: { id: string }[] };
    const apptId = appts.appointments[0].id;
    const note = await app.inject({
      method: "POST",
      url: `/appointments/${apptId}/notes`,
      headers: auth(doc),
      payload: { text: "Healthy. Next visit in 6 months." },
    });
    assert.equal(note.statusCode, 201, note.body);
  });

  it("admin reads the audit trail (auth-guard recorded events)", async () => {
    const admin = await login("admin@acme-vet.test", "adminpass1");
    const res = await app.inject({ method: "GET", url: "/admin/audit", headers: auth(admin) });
    assert.equal(res.statusCode, 200, res.body);
    const events = (res.json() as { events: unknown[] }).events;
    assert.ok(Array.isArray(events) && events.length > 0, "audit trail should have events from all the auth activity");
  });

  it("an owner cannot reach the admin audit route", async () => {
    const token = await login("alice@acme-vet.test", "alicepass1");
    const res = await app.inject({ method: "GET", url: "/admin/audit", headers: auth(token) });
    assert.equal(res.statusCode, 403, "owner lacks audit:read");
  });

  it("cannot delete a pet that has an active booking; can after the booking is gone", async () => {
    const token = await login("alice@acme-vet.test", "alicepass1");
    // a fresh pet + a far-future booking
    const pet = (await app.inject({ method: "POST", url: "/pets", headers: auth(token), payload: { name: "Milo", species: "dog" } }).then((r) => r.json())) as { id: string };
    const far = new Date(Date.now() + 30 * 864e5).toISOString().slice(0, 16); // +30d
    const appt = (await app.inject({ method: "POST", url: "/appointments", headers: auth(token), payload: { pet: pet.id, datetime: far } }).then((r) => r.json())) as { id: string };

    const blocked = await app.inject({ method: "DELETE", url: `/pets/${pet.id}`, headers: auth(token) });
    assert.equal(blocked.statusCode, 409, "pet with active booking can't be deleted");

    // cancel the booking (far future => allowed), then the pet deletes
    const cancel = await app.inject({ method: "DELETE", url: `/appointments/${appt.id}`, headers: auth(token) });
    assert.equal(cancel.statusCode, 204, cancel.body);
    const ok = await app.inject({ method: "DELETE", url: `/pets/${pet.id}`, headers: auth(token) });
    assert.equal(ok.statusCode, 204, "pet deletes once it has no active bookings");
  });

  it("cannot cancel an appointment within 24h, can when >24h away", async () => {
    const token = await login("alice@acme-vet.test", "alicepass1");
    const pet = (await app.inject({ method: "POST", url: "/pets", headers: auth(token), payload: { name: "Bea", species: "cat" } }).then((r) => r.json())) as { id: string };

    const soon = new Date(Date.now() + 2 * 3600e3).toISOString().slice(0, 16); // +2h
    const soonAppt = (await app.inject({ method: "POST", url: "/appointments", headers: auth(token), payload: { pet: pet.id, datetime: soon } }).then((r) => r.json())) as { id: string };
    const within = await app.inject({ method: "DELETE", url: `/appointments/${soonAppt.id}`, headers: auth(token) });
    assert.equal(within.statusCode, 409, "appointment within 24h can't be cancelled");

    const far = new Date(Date.now() + 3 * 864e5).toISOString().slice(0, 16); // +3d
    const farAppt = (await app.inject({ method: "POST", url: "/appointments", headers: auth(token), payload: { pet: pet.id, datetime: far } }).then((r) => r.json())) as { id: string };
    const okCancel = await app.inject({ method: "DELETE", url: `/appointments/${farAppt.id}`, headers: auth(token) });
    assert.equal(okCancel.statusCode, 204, ">24h appointment cancels");
  });

  it("owner uploads a pet photo (validated + stored in blob:store) and it serves back", async () => {
    const token = await login("alice@acme-vet.test", "alicepass1");
    const pet = (await app.inject({ method: "POST", url: "/pets", headers: auth(token), payload: { name: "Pixel", species: "cat" } }).then((r) => r.json())) as { id: string };
    // a tiny valid PNG (1x1)
    const png = Buffer.from(
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
      "base64",
    );

    const up = await app.inject({
      method: "POST",
      url: `/pets/${pet.id}/photo`,
      headers: { ...auth(token), "content-type": "image/png" },
      payload: png,
    });
    assert.equal(up.statusCode, 201, `upload: ${up.body}`);

    const got = await app.inject({ method: "GET", url: `/pets/${pet.id}/photo`, headers: auth(token) });
    assert.equal(got.statusCode, 200);
    assert.equal(got.headers["content-type"], "image/png");
    assert.deepEqual(got.rawPayload, png, "served bytes match what was uploaded");
  });

  it("rejects a disallowed photo type (upload:policy)", async () => {
    const token = await login("alice@acme-vet.test", "alicepass1");
    const pet = (await app.inject({ method: "POST", url: "/pets", headers: auth(token), payload: { name: "Doc", species: "dog" } }).then((r) => r.json())) as { id: string };
    // image/* parser only fires for image content-types; a non-image is rejected
    // before policy, but an allowed-list miss (e.g. image/tiff) is the policy path.
    const res = await app.inject({
      method: "POST",
      url: `/pets/${pet.id}/photo`,
      headers: { ...auth(token), "content-type": "image/tiff" },
      payload: Buffer.from("II* ", "binary"),
    });
    assert.equal(res.statusCode, 415, `expected type_not_allowed, got ${res.statusCode} ${res.body}`);
  });

  it("pet detail returns the pet + its appointments with notes", async () => {
    const owner = await login("alice@acme-vet.test", "alicepass1");
    const pet = (await app.inject({ method: "POST", url: "/pets", headers: auth(owner), payload: { name: "Detailed", species: "dog" } }).then((r) => r.json())) as { id: string };
    const far = new Date(Date.now() + 12 * 864e5).toISOString().slice(0, 16);
    const appt = (await app.inject({ method: "POST", url: "/appointments", headers: auth(owner), payload: { pet: pet.id, datetime: far } }).then((r) => r.json())) as { id: string };
    const doc = await login("doctor@acme-vet.test", "doctorpass1");
    await app.inject({ method: "POST", url: `/appointments/${appt.id}/notes`, headers: auth(doc), payload: { text: "Detail note." } });

    const res = await app.inject({ method: "GET", url: `/pets/${pet.id}`, headers: auth(owner) });
    assert.equal(res.statusCode, 200, res.body);
    const d = res.json() as { name: string; appointments: { id: string; notes: { text: string }[] }[] };
    assert.equal(d.name, "Detailed");
    assert.equal(d.appointments.length, 1);
    assert.ok(d.appointments[0].notes.some((n) => n.text === "Detail note."), "detail embeds the note");

    // another owner can't view it
    await app.inject({ method: "POST", url: "/auth/register", payload: { email: "carol@acme-vet.test", password: "carolpass1", role: "pet-owner" } });
    const carol = await login("carol@acme-vet.test", "carolpass1");
    const forbidden = await app.inject({ method: "GET", url: `/pets/${pet.id}`, headers: auth(carol) });
    assert.equal(forbidden.statusCode, 403, "non-owner blocked from pet detail");
  });

  it("owner sees the doctor's visit notes on their own appointment", async () => {
    const owner = await login("alice@acme-vet.test", "alicepass1");
    const pet = (await app.inject({ method: "POST", url: "/pets", headers: auth(owner), payload: { name: "Notable", species: "dog" } }).then((r) => r.json())) as { id: string };
    const far = new Date(Date.now() + 10 * 864e5).toISOString().slice(0, 16);
    const appt = (await app.inject({ method: "POST", url: "/appointments", headers: auth(owner), payload: { pet: pet.id, datetime: far } }).then((r) => r.json())) as { id: string };
    // doctor writes a note
    const doc = await login("doctor@acme-vet.test", "doctorpass1");
    await app.inject({ method: "POST", url: `/appointments/${appt.id}/notes`, headers: auth(doc), payload: { text: "All good." } });
    // owner can read it
    const res = await app.inject({ method: "GET", url: `/appointments/${appt.id}/notes`, headers: auth(owner) });
    assert.equal(res.statusCode, 200, res.body);
    const notes = (res.json() as { notes: { text: string }[] }).notes;
    assert.ok(notes.some((n) => n.text === "All good."), "owner sees the doctor's note");
  });

  it("an owner cannot read notes on someone else's appointment", async () => {
    // bob's appointment
    await app.inject({ method: "POST", url: "/auth/register", payload: { email: "bob@acme-vet.test", password: "bobpass12", role: "pet-owner" } });
    const bob = await login("bob@acme-vet.test", "bobpass12");
    const bobPet = (await app.inject({ method: "POST", url: "/pets", headers: auth(bob), payload: { name: "BobDog", species: "dog" } }).then((r) => r.json())) as { id: string };
    const far = new Date(Date.now() + 11 * 864e5).toISOString().slice(0, 16);
    const bobAppt = (await app.inject({ method: "POST", url: "/appointments", headers: auth(bob), payload: { pet: bobPet.id, datetime: far } }).then((r) => r.json())) as { id: string };
    // alice tries to read bob's appointment notes
    const alice = await login("alice@acme-vet.test", "alicepass1");
    const res = await app.inject({ method: "GET", url: `/appointments/${bobAppt.id}/notes`, headers: auth(alice) });
    assert.equal(res.statusCode, 403, "owner can't read another owner's appointment notes");
  });

  it("rejects an oversized photo (upload:policy max-size)", async () => {
    const token = await login("alice@acme-vet.test", "alicepass1");
    const pet = (await app.inject({ method: "POST", url: "/pets", headers: auth(token), payload: { name: "Big", species: "horse" } }).then((r) => r.json())) as { id: string };
    const huge = Buffer.alloc(3 * 1024 * 1024, 1); // 3 MiB > 2 MiB cap
    const res = await app.inject({
      method: "POST",
      url: `/pets/${pet.id}/photo`,
      headers: { ...auth(token), "content-type": "image/png" },
      payload: huge,
    });
    assert.equal(res.statusCode, 413, `expected too_large, got ${res.statusCode}`);
  });

  it("appointment lifecycle is governed by the fsm (legal moves only)", async () => {
    const owner = await login("alice@acme-vet.test", "alicepass1");
    const pet = (await app.inject({ method: "POST", url: "/pets", headers: auth(owner), payload: { name: "Fsm", species: "dog" } }).then((r) => r.json())) as { id: string };
    const far = new Date(Date.now() + 20 * 864e5).toISOString().slice(0, 16);
    const appt = (await app.inject({ method: "POST", url: "/appointments", headers: auth(owner), payload: { pet: pet.id, datetime: far } }).then((r) => r.json())) as { id: string };
    const doc = await login("doctor@acme-vet.test", "doctorpass1");

    // illegal: can't complete a booked appointment (must confirm first) -> 409
    const bad = await app.inject({ method: "POST", url: `/appointments/${appt.id}/transition`, headers: auth(doc), payload: { event: "complete" } });
    assert.equal(bad.statusCode, 409, `expected illegal_transition, got ${bad.statusCode} ${bad.body}`);
    assert.equal((bad.json() as { current: string }).current, "booked");

    // legal: confirm -> complete
    const c1 = await app.inject({ method: "POST", url: `/appointments/${appt.id}/transition`, headers: auth(doc), payload: { event: "confirm" } });
    assert.equal(c1.statusCode, 200, c1.body);
    assert.equal((c1.json() as { status: string }).status, "confirmed");
    const c2 = await app.inject({ method: "POST", url: `/appointments/${appt.id}/transition`, headers: auth(doc), payload: { event: "complete" } });
    assert.equal((c2.json() as { status: string }).status, "completed");

    // terminal: no further moves
    const c3 = await app.inject({ method: "POST", url: `/appointments/${appt.id}/transition`, headers: auth(doc), payload: { event: "cancel" } });
    assert.equal(c3.statusCode, 409, "completed is terminal");

    // an owner can cancel their OWN booked appointment
    const appt2 = (await app.inject({ method: "POST", url: "/appointments", headers: auth(owner), payload: { pet: pet.id, datetime: far } }).then((r) => r.json())) as { id: string };
    const oc = await app.inject({ method: "POST", url: `/appointments/${appt2.id}/transition`, headers: auth(owner), payload: { event: "cancel" } });
    assert.equal(oc.statusCode, 200, oc.body);
    assert.equal((oc.json() as { status: string }).status, "cancelled");
    // but an owner can't confirm (doctor/admin only)
    const appt3 = (await app.inject({ method: "POST", url: "/appointments", headers: auth(owner), payload: { pet: pet.id, datetime: far } }).then((r) => r.json())) as { id: string };
    const ocf = await app.inject({ method: "POST", url: `/appointments/${appt3.id}/transition`, headers: auth(owner), payload: { event: "confirm" } });
    assert.equal(ocf.statusCode, 403, "owner can't confirm");
  });

  it("doctor's markdown notes render to safe HTML (md:render)", async () => {
    const owner = await login("alice@acme-vet.test", "alicepass1");
    const pet = (await app.inject({ method: "POST", url: "/pets", headers: auth(owner), payload: { name: "Md", species: "dog" } }).then((r) => r.json())) as { id: string };
    const far = new Date(Date.now() + 21 * 864e5).toISOString().slice(0, 16);
    const appt = (await app.inject({ method: "POST", url: "/appointments", headers: auth(owner), payload: { pet: pet.id, datetime: far } }).then((r) => r.json())) as { id: string };
    const doc = await login("doctor@acme-vet.test", "doctorpass1");
    await app.inject({ method: "POST", url: `/appointments/${appt.id}/notes`, headers: auth(doc), payload: { text: "**Bold** and <script>alert(1)</script>" } });
    const res = await app.inject({ method: "GET", url: `/appointments/${appt.id}/notes`, headers: auth(owner) });
    const notes = (res.json() as { notes: { textHtml: string }[] }).notes;
    const html = notes[0].textHtml;
    assert.ok(html.includes("<strong>Bold</strong>"), "markdown rendered");
    assert.ok(!html.includes("<script>"), "raw HTML escaped (XSS-safe)");
  });

  it("doctor invoices an appointment; total is exact (money:amount)", async () => {
    const owner = await login("alice@acme-vet.test", "alicepass1");
    const pet = (await app.inject({ method: "POST", url: "/pets", headers: auth(owner), payload: { name: "Bill", species: "dog" } }).then((r) => r.json())) as { id: string };
    const far = new Date(Date.now() + 22 * 864e5).toISOString().slice(0, 16);
    const appt = (await app.inject({ method: "POST", url: "/appointments", headers: auth(owner), payload: { pet: pet.id, datetime: far } }).then((r) => r.json())) as { id: string };
    const doc = await login("doctor@acme-vet.test", "doctorpass1");

    const denied = await app.inject({ method: "PUT", url: `/appointments/${appt.id}/invoice`, headers: auth(owner), payload: { items: [{ description: "x", cents: 100 }] } });
    assert.equal(denied.statusCode, 403, "owner can't invoice");

    const inv = await app.inject({
      method: "PUT",
      url: `/appointments/${appt.id}/invoice`,
      headers: auth(doc),
      payload: { items: [{ description: "Consultation", cents: 5000 }, { description: "Vaccine", cents: 3450 }] },
    });
    assert.equal(inv.statusCode, 201, inv.body);
    const body = inv.json() as { totalCents: number; totalFormatted: string };
    assert.equal(body.totalCents, 8450, "exact cents sum");
    assert.equal(body.totalFormatted, "84.50", "formatted total");

    const got = await app.inject({ method: "GET", url: `/appointments/${appt.id}/invoice`, headers: auth(owner) });
    assert.equal(got.statusCode, 200);
    assert.equal((got.json() as { totalCents: number }).totalCents, 8450);
  });
});
