// Fastify backend for the vet-clinic example. Every business route verifies the
// caller's token AND required permission through the composed auth-guard
// (authorizer.authorize) before touching domain data; the auth-guard records
// each auth decision to its plugged-in audit-log. Static SPA assets are served
// from ./public.
//
// Pattern (guarded helper, statusFor map, BigInt reply serializer) mirrors
// examples/jco-embed/src/app.ts.

import { fileURLToPath } from "node:url";
import path from "node:path";
import Fastify, { type FastifyInstance, type FastifyReply, type FastifyRequest } from "fastify";
import fastifyStatic from "@fastify/static";

import * as auth from "./auth.js";
import * as domain from "./domain.js";
import { seedRoles, seedDemoUsers, type DemoUser } from "./seed.js";

interface AuthErr {
  tag: string;
  val?: unknown;
}

function statusFor(e: AuthErr): [number, string] {
  switch (e.tag) {
    case "invalid-credentials": return [401, "invalid_credentials"];
    case "already-exists": return [409, "already_exists"];
    case "rate-limited": return [429, "rate_limited"];
    case "insufficient-scope": return [403, "insufficient_scope"];
    case "expired": return [401, "expired"];
    case "invalid-token": return [401, "invalid_token"];
    case "unknown-tenant": return [403, "unknown_tenant"];
    case "malformed": return [400, "malformed"];
    case "backend-unavailable": return [503, "backend_unavailable"];
    default: return [500, "internal"];
  }
}

function bearer(h?: string): string | undefined {
  return h?.startsWith("Bearer ") ? h.slice(7).trim() : undefined;
}

export interface BuildResult {
  app: FastifyInstance;
  demoUsers: DemoUser[];
}

export function buildApp(opts: { logger?: boolean; serveStatic?: boolean } = {}): BuildResult {
  // bodyLimit above the upload-policy max-size (2 MiB) so the POLICY rejects an
  // oversized image (413 too_large), not Fastify's framework limit.
  const app = Fastify({ logger: opts.logger ?? false, bodyLimit: 8 * 1024 * 1024 });

  // WIT u64 (expires-in / timestamps) arrives as BigInt; coerce on the way out.
  app.setReplySerializer((payload) =>
    JSON.stringify(payload, (_k, v) => (typeof v === "bigint" ? Number(v) : v)),
  );

  // Raw-bytes body parser for image uploads (every image/* content-type) — the
  // photo route receives a Buffer rather than parsed JSON.
  app.addContentTypeParser(
    /^image\//,
    { parseAs: "buffer" },
    (_req, body, done) => done(null, body),
  );

  // Seed the 3 roles + demo users + register the appointment state machine.
  seedRoles();
  const demoUsers = seedDemoUsers();
  domain.initFsm();
  domain.seedI18n();

  // Translate a thrown auth-error variant into an HTTP response. Returns the
  // call's value on success, or undefined after sending an error.
  function guarded<T>(reply: FastifyReply, fn: () => T): T | undefined {
    try {
      return fn();
    } catch (e) {
      const err = (e && typeof e === "object" && "payload" in e
        ? (e as { payload: unknown }).payload
        : e) as AuthErr;
      if (!err || typeof err.tag !== "string") {
        app.log.error(e, "non-auth throw");
        reply.code(500).send({ error: "internal" });
        return undefined;
      }
      const [code, msg] = statusFor(err);
      reply.code(code).send({ error: msg });
      return undefined;
    }
  }

  // Require a bearer token + permission; returns the principal or sends 401/403.
  function require(
    request: FastifyRequest,
    reply: FastifyReply,
    target: string,
    action: string,
  ): auth.Principal | undefined {
    const token = bearer(request.headers.authorization);
    if (!token) {
      reply.code(401).send({ error: "missing_bearer_token" });
      return undefined;
    }
    return guarded(reply, () => auth.authorize(token, { target, action }));
  }

  // ---- auth -------------------------------------------------------------

  app.post("/auth/register", async (request, reply) => {
    const { email, password, role } = request.body as Record<string, string>;
    const p = guarded(reply, () => auth.register(email, password));
    if (!p) return;
    // Self-service registration assigns the requested role, defaulting to
    // pet-owner. (A real clinic would gate doctor/admin behind an admin.)
    const wanted = role && ["pet-owner", "doctor", "admin"].includes(role) ? role : "pet-owner";
    guarded(reply, () => auth.assignRole(p.subject, wanted));
    if (!reply.sent) reply.code(201).send({ subject: p.subject, tenant: p.tenant, role: wanted });
  });

  app.post("/auth/login", async (request, reply) => {
    const { email, password } = request.body as Record<string, string>;
    return guarded(reply, () => auth.login(email, password));
  });

  app.get("/auth/me", async (request, reply) => {
    const token = bearer(request.headers.authorization);
    if (!token) return reply.code(401).send({ error: "missing_bearer_token" });
    return guarded(reply, () => auth.introspect(token));
  });

  app.post("/auth/logout", async (request, reply) => {
    const token = bearer(request.headers.authorization);
    if (!token) return reply.code(401).send({ error: "missing_bearer_token" });
    guarded(reply, () => auth.logout(token));
    if (!reply.sent) reply.code(204).send();
  });

  // ---- pets -------------------------------------------------------------

  app.get("/pets", async (request, reply) => {
    const p = require(request, reply, "pets", "read");
    if (!p) return;
    const { q, limit, cursor } = request.query as { q?: string; limit?: string; cursor?: string };
    // Owners only see their own pets; doctors/admins see all.
    const scopeOwner = p.roles.includes("pet-owner") && !p.roles.includes("admin") && !p.roles.includes("doctor")
      ? p.subject
      : undefined;
    // With ?limit, return a cursor-paginated page (paginate:cursor); otherwise
    // the full list/search (back-compat with the existing UI).
    if (limit !== undefined && !q) {
      const { pets, page } = domain.paginatePets(scopeOwner, Number(limit) || 10, cursor);
      return { pets, page, viewer: p.subject };
    }
    const pets = q ? domain.searchPets(q, scopeOwner) : domain.listPets(scopeOwner);
    return { pets, viewer: p.subject };
  });

  app.post("/pets", async (request, reply) => {
    const p = require(request, reply, "pets", "write");
    if (!p) return;
    const body = request.body as { name?: string; species?: string; notes?: string };
    const errs = domain.check(body, "pet");
    if (errs.length) return reply.code(400).send({ error: "validation_failed", fields: errs });
    const pet = domain.createPet({
      name: body.name!,
      species: body.species!,
      owner: p.subject,
      notes: body.notes ?? "",
    });
    return reply.code(201).send(pet);
  });

  // Upload a pet photo: raw image bytes in the body, content-type header set.
  // upload-policy validates type+size and mints/redeems a signed ticket;
  // blob-store holds the bytes. Owner-scoped.
  app.post("/pets/:id/photo", async (request, reply) => {
    const p = require(request, reply, "pets", "write");
    if (!p) return;
    const { id } = request.params as { id: string };
    const ct = request.headers["content-type"] ?? "application/octet-stream";
    const body = request.body as Buffer; // raw bytes (see content-type parser below)
    if (!body || !body.length) return reply.code(400).send({ error: "empty_body" });
    const r = domain.putPetPhoto(id, p.subject, ct, new Uint8Array(body));
    if (r.ok) return reply.code(201).send({ ok: true });
    const map: Record<string, [number, string]> = {
      not_found: [404, "pet_not_found"],
      forbidden: [403, "not_your_pet"],
      "type-not-allowed": [415, "type_not_allowed"],
      "too-large": [413, "too_large"],
      invalid_ticket: [400, "invalid_ticket"],
    };
    const [code, error] = map[r.reason ?? ""] ?? [400, r.reason ?? "rejected"];
    return reply.code(code).send({ error });
  });

  // Full detail for one pet: the pet + its appointments, each with visit notes.
  // Owner-scoped (own pet); doctors/admins may view any.
  app.get("/pets/:id", async (request, reply) => {
    const p = require(request, reply, "pets", "read");
    if (!p) return;
    const { id } = request.params as { id: string };
    const detail = domain.petDetail(id);
    if (!detail) return reply.code(404).send({ error: "pet_not_found" });
    const privileged = p.roles.includes("admin") || p.roles.includes("doctor");
    if (!privileged && detail.owner !== p.subject) {
      return reply.code(403).send({ error: "not_your_pet" });
    }
    return detail;
  });

  // Serve a pet photo (public-ish read: any authed user with pets:read).
  app.get("/pets/:id/photo", async (request, reply) => {
    const p = require(request, reply, "pets", "read");
    if (!p) return;
    const { id } = request.params as { id: string };
    const photo = domain.getPetPhoto(id);
    if (!photo) return reply.code(404).send({ error: "no_photo" });
    reply.header("content-type", photo.contentType);
    reply.header("cache-control", "no-cache");
    return reply.send(Buffer.from(photo.data));
  });

  // ---- appointments -----------------------------------------------------

  app.get("/appointments", async (request, reply) => {
    const p = require(request, reply, "appointments", "read");
    if (!p) return;
    let appointments;
    if (p.roles.includes("admin")) {
      appointments = domain.listAppointments({}); // all
    } else if (p.roles.includes("doctor")) {
      // A doctor sees their own assigned appointments PLUS the unassigned queue
      // (there's no per-doctor assignment at booking time, so the clinic shares
      // a pool; writing a note claims one — see the notes route).
      appointments = domain
        .listAppointments({})
        .filter((a) => a.doctor === p.subject || a.doctor === "");
    } else {
      appointments = domain.listAppointments({ owner: p.subject }); // own
    }
    return { appointments, viewer: p.subject };
  });

  app.post("/appointments", async (request, reply) => {
    const p = require(request, reply, "appointments", "write");
    if (!p) return;
    const body = request.body as { pet?: string; datetime?: string; doctor?: string };
    const errs = domain.check(body, "appointment");
    if (errs.length) return reply.code(400).send({ error: "validation_failed", fields: errs });
    const pet = domain.getPet(body.pet!);
    if (!pet) return reply.code(404).send({ error: "pet_not_found" });
    const appt = domain.createAppointment({
      pet: body.pet!,
      owner: pet.owner,
      doctor: body.doctor ?? "",
      datetime: body.datetime!,
    });
    // Fire a confirmation via notify-dispatch (local sink in the demo).
    auth.notifyEmail(p.subject, "Appointment booked", `Your appointment ${appt.id} for ${pet.name} is ${appt.datetime}.`);
    return reply.code(201).send(appt);
  });

  // Advance an appointment through its lifecycle (fsm:workflow enforces legal
  // moves). confirm/complete are doctor/admin; cancel may also be done by the
  // owner of the appointment. Illegal moves -> 409.
  app.post("/appointments/:id/transition", async (request, reply) => {
    const p = require(request, reply, "appointments", "write");
    if (!p) return;
    const { id } = request.params as { id: string };
    const { event } = request.body as { event?: string };
    if (!event || !domain.APPT_EVENTS.includes(event as never)) {
      return reply.code(400).send({ error: "bad_event" });
    }
    const appt = domain.getAppointment(id);
    if (!appt) return reply.code(404).send({ error: "appointment_not_found" });
    const privileged = p.roles.includes("admin") || p.roles.includes("doctor");
    // owners may only cancel, and only their own appointment
    if (!privileged && !(event === "cancel" && appt.owner === p.subject)) {
      return reply.code(403).send({ error: "forbidden" });
    }
    const r = domain.transitionAppointment(id, event as domain.ApptEvent);
    if (r.ok) return { id, status: r.status, allowed: domain.appointmentEvents(id) };
    if (r.reason === "illegal_transition") {
      return reply.code(409).send({ error: "illegal_transition", current: r.current });
    }
    return reply.code(400).send({ error: r.reason });
  });

  // Invoice an appointment (doctor/admin sets line items; money:amount totals).
  app.put("/appointments/:id/invoice", async (request, reply) => {
    const p = require(request, reply, "appointments", "write");
    if (!p) return;
    if (!p.roles.includes("doctor") && !p.roles.includes("admin")) {
      return reply.code(403).send({ error: "forbidden" });
    }
    const { id } = request.params as { id: string };
    if (!domain.getAppointment(id)) return reply.code(404).send({ error: "appointment_not_found" });
    const { items } = request.body as { items?: { description: string; cents: number }[] };
    if (!Array.isArray(items) || items.length === 0) {
      return reply.code(400).send({ error: "no_items" });
    }
    return reply.code(201).send(domain.setInvoice(id, items));
  });

  // Read an appointment's invoice (owner of it, or doctor/admin).
  app.get("/appointments/:id/invoice", async (request, reply) => {
    const p = require(request, reply, "appointments", "read");
    if (!p) return;
    const { id } = request.params as { id: string };
    const appt = domain.getAppointment(id);
    if (!appt) return reply.code(404).send({ error: "appointment_not_found" });
    const privileged = p.roles.includes("admin") || p.roles.includes("doctor");
    if (!privileged && appt.owner !== p.subject) {
      return reply.code(403).send({ error: "forbidden" });
    }
    const inv = domain.getInvoice(id);
    if (!inv) return reply.code(404).send({ error: "no_invoice" });
    return inv;
  });

  app.post("/appointments/:id/notes", async (request, reply) => {
    const p = require(request, reply, "notes", "write");
    if (!p) return;
    const { id } = request.params as { id: string };
    const appt = domain.getAppointment(id);
    if (!appt) return reply.code(404).send({ error: "appointment_not_found" });
    const { text } = request.body as { text?: string };
    if (!text || text.length < 1) return reply.code(400).send({ error: "empty_note" });
    // Writing a note claims the appointment for this doctor (if unassigned).
    domain.assignDoctor(id, p.subject);
    const note = domain.addNote({ appointment: id, author: p.subject, text });
    return reply.code(201).send(note);
  });

  app.get("/appointments/:id/notes", async (request, reply) => {
    const p = require(request, reply, "appointments", "read");
    if (!p) return;
    const { id } = request.params as { id: string };
    // Owners may only read notes on their own appointments; doctors/admins any.
    const appt = domain.getAppointment(id);
    if (!appt) return reply.code(404).send({ error: "appointment_not_found" });
    const privileged = p.roles.includes("admin") || p.roles.includes("doctor");
    if (!privileged && appt.owner !== p.subject) {
      return reply.code(403).send({ error: "not_your_appointment" });
    }
    return { notes: domain.notesFor(id) };
  });

  // ---- AI clinical summary (ai:inference) -------------------------------
  // Generate a summary of the pet + this appointment's notes (doctor/admin).
  // ?force=1 re-runs inference; otherwise returns the cached one if present.
  app.post("/appointments/:id/summary", async (request, reply) => {
    const p = require(request, reply, "notes", "write"); // doctor (or admin via *)
    if (!p) return;
    const { id } = request.params as { id: string };
    const { force } = request.query as { force?: string };
    const result = domain.summarizeAppointment(id, force === "1" || force === "true");
    if (!result) return reply.code(404).send({ error: "appointment_not_found" });
    return result;
  });

  // Read a cached AI summary (owner of the appointment, or doctor/admin).
  app.get("/appointments/:id/summary", async (request, reply) => {
    const p = require(request, reply, "appointments", "read");
    if (!p) return;
    const { id } = request.params as { id: string };
    const appt = domain.getAppointment(id);
    if (!appt) return reply.code(404).send({ error: "appointment_not_found" });
    const privileged = p.roles.includes("admin") || p.roles.includes("doctor");
    if (!privileged && appt.owner !== p.subject) {
      return reply.code(403).send({ error: "not_your_appointment" });
    }
    const result = domain.getSummary(id);
    if (!result) return reply.code(404).send({ error: "no_summary" });
    return result;
  });

  // ---- deletes (owner-scoped, with business rules) ----------------------

  // Delete a pet — only when it has no active bookings.
  app.delete("/pets/:id", async (request, reply) => {
    const p = require(request, reply, "pets", "write");
    if (!p) return;
    const { id } = request.params as { id: string };
    const r = domain.deletePet(id, p.subject);
    if (r.ok) return reply.code(204).send();
    const map: Record<string, [number, string]> = {
      not_found: [404, "pet_not_found"],
      forbidden: [403, "not_your_pet"],
      has_active_bookings: [409, "pet_has_active_bookings"],
    };
    const [code, error] = map[r.reason] ?? [400, r.reason];
    return reply.code(code).send({ error });
  });

  // Cancel an appointment — only when it's more than 24h away.
  app.delete("/appointments/:id", async (request, reply) => {
    const p = require(request, reply, "appointments", "write");
    if (!p) return;
    const { id } = request.params as { id: string };
    const now = Math.floor(Date.now() / 1000);
    const r = domain.deleteAppointment(id, p.subject, now);
    if (r.ok) return reply.code(204).send();
    const map: Record<string, [number, string]> = {
      not_found: [404, "appointment_not_found"],
      forbidden: [403, "not_your_appointment"],
      within_24h: [409, "within_24h_no_cancel"],
      bad_datetime: [400, "bad_datetime"],
    };
    const [code, error] = map[r.reason] ?? [400, r.reason];
    return reply.code(code).send({ error });
  });

  // ---- admin ------------------------------------------------------------

  app.get("/admin/audit", async (request, reply) => {
    // Admin-only. auth-guard records audit events into the shared KV under
    // audit-log's own keys; expose a thin read of them. (The composed wasm
    // wires audit:log/recorder internally but does not re-export its `query`
    // interface, so we read the raw audit keys the recorder wrote.)
    const p = require(request, reply, "audit", "read");
    if (!p) return;
    // PII (emails/phones/cards/SSN/IPs) masked via pii:redact before display.
    return { events: domain.readAuditTrailRedacted() };
  });

  app.post("/admin/assign-role", async (request, reply) => {
    const p = require(request, reply, "rbac", "admin");
    if (!p) return;
    const { subject, role } = request.body as { subject: string; role: string };
    guarded(reply, () => auth.assignRole(subject, role));
    if (!reply.sent) reply.code(204).send();
  });

  // ---- CSV export (csv:codec) — admin downloads ------------------------
  app.get("/admin/export/:what.csv", async (request, reply) => {
    const p = require(request, reply, "audit", "read"); // admin (has *:*)
    if (!p) return;
    const { what } = request.params as { what: string };
    const csvText =
      what === "appointments" ? domain.appointmentsCsv()
      : what === "audit" ? domain.auditCsv()
      : null;
    if (csvText === null) return reply.code(404).send({ error: "unknown_export" });
    reply.header("content-type", "text/csv; charset=utf-8");
    reply.header("content-disposition", `attachment; filename="${what}.csv"`);
    return reply.send(csvText);
  });

  // ---- staff 2FA (otp:totp) -------------------------------------------
  // Enroll: returns the otpauth:// URI (render as QR) + the secret. Then verify
  // a code to confirm enrollment. Any authed staff member enrolls themselves.
  app.post("/auth/2fa/enroll", async (request, reply) => {
    const token = bearer(request.headers.authorization);
    if (!token) return reply.code(401).send({ error: "missing_bearer_token" });
    const me = guarded(reply, () => auth.introspect(token));
    if (!me) return;
    return domain.provisionOtp(me.subject, `${me.subject}@${me.tenant}`);
  });

  app.post("/auth/2fa/verify", async (request, reply) => {
    const token = bearer(request.headers.authorization);
    if (!token) return reply.code(401).send({ error: "missing_bearer_token" });
    const me = guarded(reply, () => auth.introspect(token));
    if (!me) return;
    const { code } = request.body as { code?: string };
    const ok = domain.verifyOtp(me.subject, code ?? "");
    if (!ok) return reply.code(401).send({ error: "bad_code" });
    return { ok: true };
  });

  app.get("/auth/2fa/status", async (request, reply) => {
    const token = bearer(request.headers.authorization);
    if (!token) return reply.code(401).send({ error: "missing_bearer_token" });
    const me = guarded(reply, () => auth.introspect(token));
    if (!me) return;
    return { enrolled: domain.hasOtp(me.subject) };
  });

  // ---- i18n (i18n:catalog) — UI string bundle -------------------------
  app.get("/i18n/:locale", async (request, reply) => {
    const { locale } = request.params as { locale: string };
    return { locale, messages: domain.uiBundle(locale) };
  });

  // ---- static SPA -------------------------------------------------------

  if (opts.serveStatic ?? true) {
    const here = path.dirname(fileURLToPath(import.meta.url));
    app.register(fastifyStatic, {
      root: path.join(here, "..", "public"),
      prefix: "/",
    });
  }

  return { app, demoUsers };
}
