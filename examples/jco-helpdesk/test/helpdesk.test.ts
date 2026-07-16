// E2E for the helpdesk domain running as ONE composed wasm HTTP component,
// served over real HTTP by jco's WASI HTTPServer. No Node domain logic: every
// route is the Rust helpdesk-domain component orchestrating auth-guard +
// records:store + fsm:workflow + id:generate + md:render, all linked into the
// one .wasm.
//
// Flow: register requester/agent -> requester opens a ticket (FSM instance in
// `new`) -> agent sees it, requester isolation holds -> internal note is
// hidden and moves nothing -> agent reply drives new->open->pending ->
// requester reply pending->open -> solve -> requester reply reopens ->
// solve + close (terminal) -> further replies 409 -> history records it all.

import { describe, it, before, after } from "node:test";
import assert from "node:assert/strict";
import { HTTPServer } from "@bytecodealliance/preview2-shim/http";
import * as component from "../gen/helpdesk_domain.composed.js";

const PORT = 3077;
const BASE = `http://localhost:${PORT}`;
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
  const r = await post("/auth/login", { email, password });
  const body = await r.text();
  assert.equal(r.status, 200, `login ${email}: ${body}`);
  return (JSON.parse(body) as { access_token: string }).access_token;
}

let requester = "";
let agent = "";
let other = "";
let ticketId = "";

describe("helpdesk-domain as one composed wasm HTTP component", () => {
  before(async () => {
    server = new HTTPServer(component.incomingHandler) as typeof server;
    server.listen(PORT);
    for (const [email, role] of [
      ["alice@example.test", "requester"],
      ["bob@support.test", "agent"],
      ["mallory@example.test", "requester"],
    ]) {
      const r = await post("/auth/register", { email, password: `${role}-pass-1`, role });
      assert.equal(r.status, 201, await r.text());
    }
    requester = await login("alice@example.test", "requester-pass-1");
    agent = await login("bob@support.test", "agent-pass-1");
    other = await login("mallory@example.test", "requester-pass-1");
  });
  after(() => server.stop?.());

  it("requester opens a ticket -> FSM instance in `new`, ref minted", async () => {
    const bad = await post("/api/tickets", { subject: "", body: "x" }, requester);
    assert.equal(bad.status, 400, "empty subject fails validation");

    const r = await post(
      "/api/tickets",
      { subject: "Cannot export CSV", body: "The **export** button 500s.", priority: "high" },
      requester,
    );
    const t = (await r.json()) as { id: string; ref: string; status: string; priority: string };
    assert.equal(r.status, 201);
    assert.equal(t.status, "new");
    assert.equal(t.priority, "high");
    assert.match(t.ref, /^HD-/, "id-generate minted a public ref");
    ticketId = t.id;
  });

  it("agent sees all tickets; another requester sees none (404 on direct get)", async () => {
    const all = (await (await get("/api/tickets", agent)).json()) as { tickets: unknown[] };
    assert.equal(all.tickets.length, 1);
    const mine = (await (await get("/api/tickets", other)).json()) as { tickets: unknown[] };
    assert.equal(mine.tickets.length, 0, "requester isolation on list");
    const denied = await get(`/api/tickets/${ticketId}`, other);
    assert.equal(denied.status, 404, "existence not leaked to other requesters");
  });

  it("internal note: hidden from the requester, moves the FSM nowhere", async () => {
    const forbidden = await post(`/api/tickets/${ticketId}/messages`, { body: "sneaky", internal: true }, requester);
    assert.equal(forbidden.status, 403, "internal notes are agent-only");

    const note = await post(`/api/tickets/${ticketId}/messages`, { body: "looks like the csv bug", internal: true }, agent);
    const noteBody = (await note.json()) as { status: string };
    assert.equal(note.status, 201);
    assert.equal(noteBody.status, "new", "internal note does not transition");

    const agentView = (await (await get(`/api/tickets/${ticketId}`, agent)).json()) as { messages: { kind: string }[] };
    const requesterView = (await (await get(`/api/tickets/${ticketId}`, requester)).json()) as {
      messages: { kind: string; html: string }[];
    };
    assert.equal(agentView.messages.length, 2);
    assert.equal(requesterView.messages.length, 1, "internal note hidden from requester");
    assert.match(requesterView.messages[0].html, /<strong>export<\/strong>/, "md:render produced safe HTML");
  });

  it("agent public reply drives new->open->pending; requester reply -> open", async () => {
    const reply = await post(`/api/tickets/${ticketId}/messages`, { body: "Can you retry now?" }, agent);
    assert.equal(((await reply.json()) as { status: string }).status, "pending", "triage + reply fired");

    const back = await post(`/api/tickets/${ticketId}/messages`, { body: "Still broken." }, requester);
    assert.equal(((await back.json()) as { status: string }).status, "open", "requester-reply fired");
  });

  it("lifecycle verbs are agent-only and FSM-legal", async () => {
    const denied = await post(`/api/tickets/${ticketId}/state`, { event: "solve" }, requester);
    assert.equal(denied.status, 403);

    const illegal = await post(`/api/tickets/${ticketId}/state`, { event: "close" }, agent);
    assert.equal(illegal.status, 409, "close from open is not a legal transition");

    const solved = await post(`/api/tickets/${ticketId}/state`, { event: "solve" }, agent);
    assert.equal(((await solved.json()) as { status: string }).status, "solved");
  });

  it("requester reply reopens a solved ticket; close is terminal", async () => {
    const reopen = await post(`/api/tickets/${ticketId}/messages`, { body: "Nope, broke again." }, requester);
    assert.equal(((await reopen.json()) as { status: string }).status, "open", "reopen fired");

    await post(`/api/tickets/${ticketId}/state`, { event: "solve" }, agent);
    const closed = await post(`/api/tickets/${ticketId}/state`, { event: "close" }, agent);
    const closedBody = (await closed.json()) as { status: string; done: boolean };
    assert.equal(closedBody.status, "closed");
    assert.equal(closedBody.done, true, "closed is terminal");

    const rejected = await post(`/api/tickets/${ticketId}/messages`, { body: "hello?" }, requester);
    assert.equal(rejected.status, 409, "no messages on a closed ticket");
  });

  it("assignment + FSM history recorded the whole journey", async () => {
    const assigned = await post(`/api/tickets/${ticketId}/assign`, { subject: "bob@support.test" }, agent);
    assert.equal(assigned.status, 200);

    const h = (await (await get(`/api/tickets/${ticketId}/history`, agent)).json()) as {
      history: { event: string; from: string; to: string }[];
    };
    const events = h.history.map((e) => e.event);
    assert.deepEqual(
      events,
      ["triage", "reply", "requester-reply", "solve", "reopen", "solve", "close"],
      "append-only lifecycle audit trail",
    );
  });

  it("a missing token is rejected (401)", async () => {
    const r = await get("/api/tickets");
    assert.equal(r.status, 401);
  });
});
