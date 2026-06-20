// Vet-clinic domain logic — pure orchestration over the comp components + the
// shared KV store. The example owns NO business component: pets, appointments
// and visit notes are plain records in the same in-memory bucket the wasm
// components use, and every cross-cutting concern (search, validation) is a
// call into a real comp component.

// The shared KV shim — the SAME module the transpiled components import, so the
// records we write here live in the same store search-index reads from.
import { open } from "./shims/keyvalue.js";
// search:index/index and validate:schema/validator, transpiled to JS.
import { index as search } from "../gen/search/search_index.js";
import { validator as validate } from "../gen/validate/validate.js";
// blob:store/blobstore (pet photos) + upload:policy/gate (type+size validation
// and a signed upload ticket) — two more comp components, no new storage code.
import { blobstore as blob } from "../gen/blob/blob_store.js";
import { gate as upload } from "../gen/upload/upload_policy.js";
// fsm:workflow/engine — appointment status lifecycle (legal transitions only).
import { engine as fsm } from "../gen/fsm/fsm_workflow.js";
// money:amount/arithmetic — exact invoice math (minor units, no floats).
import { arithmetic as money } from "../gen/money/money.js";
// md:render/renderer — safe Markdown -> HTML for rich visit notes.
import { renderer as markdown } from "../gen/markdown/markdown.js";

const bucket = open("default");
const PHOTO_CONTAINER = "pet-photos";
const APPT_MACHINE = "appointment";
const enc = (s: string) => new TextEncoder().encode(s);
const dec = (b: Uint8Array) => new TextDecoder().decode(b);

function put(key: string, value: unknown): void {
  bucket.set(key, enc(JSON.stringify(value)));
}
function read<T>(key: string): T | undefined {
  const raw = bucket.get(key);
  return raw ? (JSON.parse(dec(raw)) as T) : undefined;
}
function scan<T>(prefix: string): T[] {
  const { keys } = bucket.listKeys(undefined) as { keys: string[] };
  return keys
    .filter((k) => k.startsWith(prefix))
    .map((k) => read<T>(k))
    .filter((v): v is T => v !== undefined);
}

let counter = 0;
function id(prefix: string): string {
  // deterministic-ish unique id without Math.random (fine for an in-proc demo)
  counter += 1;
  return `${prefix}_${Date.now().toString(36)}${counter.toString(36)}`;
}

// ---- types ---------------------------------------------------------------

export interface Pet {
  id: string;
  name: string;
  species: string;
  owner: string; // owner subject id
  notes: string;
  photo?: string; // content-type of the stored photo, or absent
}
export interface Appointment {
  id: string;
  pet: string; // pet id
  owner: string;
  doctor: string; // doctor subject id (or "" = unassigned)
  datetime: string; // ISO-ish string
  status: string;
}
export interface VisitNote {
  id: string;
  appointment: string;
  author: string; // doctor subject
  text: string; // raw markdown (as written by the doctor)
  at: number;
  textHtml?: string; // safe HTML rendered from `text` (md:render); added on read
}

// ---- validation rules (declarative, enforced by the validate component) ---

const PET_RULES = [
  { field: "name", kind: "text" as const, required: true, minLen: 1, maxLen: 60, minValue: undefined, maxValue: undefined, oneOf: [] },
  { field: "species", kind: "text" as const, required: true, minLen: 1, maxLen: 40, minValue: undefined, maxValue: undefined, oneOf: [] },
];
const APPT_RULES = [
  { field: "pet", kind: "text" as const, required: true, minLen: 1, maxLen: 80, minValue: undefined, maxValue: undefined, oneOf: [] },
  { field: "datetime", kind: "text" as const, required: true, minLen: 4, maxLen: 40, minValue: undefined, maxValue: undefined, oneOf: [] },
];

export interface FieldError {
  field: string;
  code: string;
  message: string;
}

/** Validate a JSON body against rules via the validate component. */
export function check(body: unknown, which: "pet" | "appointment"): FieldError[] {
  const rules = which === "pet" ? PET_RULES : APPT_RULES;
  return validate.validate(JSON.stringify(body), rules) as FieldError[];
}

// ---- pets ----------------------------------------------------------------

export function createPet(input: { name: string; species: string; owner: string; notes?: string }): Pet {
  const pet: Pet = { id: id("pet"), notes: "", ...input };
  put(`pet_${pet.id}`, pet);
  // index it for full-text search (name + species + notes), tagged by owner.
  search.indexDoc(pet.id, `${pet.name} ${pet.species} ${pet.notes}`, [`owner:${pet.owner}`]);
  return pet;
}

export function listPets(forOwner?: string): Pet[] {
  const pets = scan<Pet>("pet_");
  return forOwner ? pets.filter((p) => p.owner === forOwner) : pets;
}

export function searchPets(q: string, forOwner?: string): Pet[] {
  const tags = forOwner ? [`owner:${forOwner}`] : [];
  const hits = search.query(q, "any", tags, 20) as { id: string; score: number }[];
  return hits.map((h) => read<Pet>(`pet_${h.id}`)).filter((p): p is Pet => p !== undefined);
}

export function getPet(petId: string): Pet | undefined {
  return read<Pet>(`pet_${petId}`);
}

// ---- pet detail (pet + its appointments, each with visit notes) ----------

export interface AppointmentWithNotes extends Appointment {
  notes: VisitNote[];
}
export interface PetDetail extends Pet {
  appointments: AppointmentWithNotes[];
}

/** All appointments for a pet (any status), newest datetime first. */
export function appointmentsForPet(petId: string): Appointment[] {
  return scan<Appointment>("appt_")
    .filter((a) => a.pet === petId)
    .sort((a, b) => (a.datetime < b.datetime ? 1 : -1));
}

/** Aggregate a pet with its appointments + each appointment's notes. */
export function petDetail(petId: string): PetDetail | undefined {
  const pet = getPet(petId);
  if (!pet) return undefined;
  const appointments = appointmentsForPet(petId).map((a) => ({
    ...a,
    notes: notesFor(a.id),
  }));
  return { ...pet, appointments };
}

// ---- pet photos (upload:policy + blob:store) -----------------------------
// upload-policy validates the declared content-type + size against the policy
// (config-driven) and mints a signed ticket; we redeem it, then blob-store
// holds the bytes. The pet record only remembers the content-type — the image
// itself lives in blob:store, durable in the same KV the components share.

export interface PhotoResult {
  ok: boolean;
  reason?: string;
}

export function putPetPhoto(
  petId: string,
  owner: string,
  contentType: string,
  data: Uint8Array,
): PhotoResult {
  const pet = getPet(petId);
  if (!pet) return { ok: false, reason: "not_found" };
  if (pet.owner !== owner) return { ok: false, reason: "forbidden" };

  // 1) upload:policy — validate + mint a signed ticket (proves the gate path).
  let ticket;
  try {
    ticket = upload.authorize(`pet/${petId}`, contentType, BigInt(data.length), 0n);
  } catch (e) {
    const tag = (e as { payload?: { tag?: string } })?.payload?.tag ?? "rejected";
    return { ok: false, reason: tag };
  }
  // 2) upload:policy — redeem the ticket (signature + expiry check).
  try {
    upload.redeem(ticket.token);
  } catch {
    return { ok: false, reason: "invalid_ticket" };
  }
  // 3) blob:store — store the bytes under the pet id.
  try {
    blob.put(PHOTO_CONTAINER, petId, data, contentType);
  } catch (e) {
    return { ok: false, reason: `blob: ${String(e)}` };
  }
  pet.photo = contentType;
  put(`pet_${petId}`, pet);
  return { ok: true };
}

export function getPetPhoto(petId: string): { contentType: string; data: Uint8Array } | undefined {
  const pet = getPet(petId);
  if (!pet || !pet.photo) return undefined;
  try {
    const data = blob.get(PHOTO_CONTAINER, petId) as Uint8Array;
    return { contentType: pet.photo, data };
  } catch {
    return undefined;
  }
}

// ---- appointment lifecycle (fsm:workflow) --------------------------------
// The appointment status is governed by a declarative state machine — legal
// moves only: booked -> confirmed -> completed, cancel from booked|confirmed,
// completed|cancelled are terminal. The fsm component is the source of truth;
// the status string mirrored onto the appt record is just for list/filter.

export const APPT_EVENTS = ["confirm", "complete", "cancel"] as const;
export type ApptEvent = (typeof APPT_EVENTS)[number];

/** Register the appointment machine. Idempotent — safe on every boot. */
export function initFsm(): void {
  fsm.define(APPT_MACHINE, {
    states: ["booked", "confirmed", "completed", "cancelled"],
    initial: "booked",
    transitions: [
      { event: "confirm", source: "booked", target: "confirmed" },
      { event: "complete", source: "confirmed", target: "completed" },
      { event: "cancel", source: "booked", target: "cancelled" },
      { event: "cancel", source: "confirmed", target: "cancelled" },
    ],
    terminal: ["completed", "cancelled"],
  });
}

// ---- appointments --------------------------------------------------------

export function createAppointment(input: { pet: string; owner: string; doctor?: string; datetime: string }): Appointment {
  const appt: Appointment = {
    id: id("appt"),
    doctor: "",
    status: "booked",
    ...input,
  };
  put(`appt_${appt.id}`, appt);
  // start its lifecycle instance in the fsm (initial state "booked").
  fsm.createInstance(APPT_MACHINE, appt.id);
  return appt;
}

export type TransitionResult =
  | { ok: true; status: string }
  | { ok: false; reason: string; current?: string };

/**
 * Drive an appointment through a lifecycle event via the fsm. Rejects illegal
 * moves (the fsm enforces booked→confirmed→completed | cancel rules). On
 * success the appt record's status mirrors the new fsm state.
 */
export function transitionAppointment(apptId: string, event: ApptEvent): TransitionResult {
  const appt = getAppointment(apptId);
  if (!appt) return { ok: false, reason: "not_found" };
  try {
    const status = fsm.fire(APPT_MACHINE, apptId, event) as { state: string };
    appt.status = status.state;
    put(`appt_${apptId}`, appt);
    return { ok: true, status: status.state };
  } catch (e) {
    const p = (e as { payload?: { tag?: string; val?: string } })?.payload;
    if (p?.tag === "illegal-transition") {
      return { ok: false, reason: "illegal_transition", current: p.val };
    }
    return { ok: false, reason: p?.tag ?? "fsm_error" };
  }
}

/** Events legal from an appointment's current state (for UI affordances). */
export function appointmentEvents(apptId: string): string[] {
  try {
    return fsm.allowedEvents(APPT_MACHINE, apptId) as string[];
  } catch {
    return [];
  }
}

export function listAppointments(filter: { owner?: string; doctor?: string }): Appointment[] {
  let appts = scan<Appointment>("appt_");
  if (filter.owner) appts = appts.filter((a) => a.owner === filter.owner);
  if (filter.doctor) appts = appts.filter((a) => a.doctor === filter.doctor);
  return appts;
}

export function getAppointment(apptId: string): Appointment | undefined {
  return read<Appointment>(`appt_${apptId}`);
}

/** Assign a doctor to an appointment (claim it). No-op if already that doctor. */
export function assignDoctor(apptId: string, doctor: string): Appointment | undefined {
  const appt = getAppointment(apptId);
  if (!appt) return undefined;
  if (appt.doctor !== doctor) {
    appt.doctor = doctor;
    put(`appt_${apptId}`, appt);
  }
  return appt;
}

/** Active (not cancelled) appointments referencing a pet. */
export function activeAppointmentsForPet(petId: string): Appointment[] {
  return scan<Appointment>("appt_").filter((a) => a.pet === petId && a.status !== "cancelled");
}

// ---- invoices (money:amount) ---------------------------------------------
// A doctor/admin bills an appointment with line items. Amounts are exact minor
// units (cents) in a single currency; the total is summed by the money
// component (no float drift), and `format` renders it for display.

const CURRENCY = "USD";

export interface LineItem {
  description: string;
  /** price in minor units (cents). */
  cents: number;
}
export interface Invoice {
  appointment: string;
  items: LineItem[];
  /** total minor units, summed exactly by money:amount. */
  totalCents: number;
  /** display string, e.g. "84.50". */
  totalFormatted: string;
  currency: string;
}

/** Set/replace the invoice for an appointment; total computed via money:amount. */
export function setInvoice(appointmentId: string, items: LineItem[]): Invoice {
  // sum with the money component: start at an explicit zero amount (parse("0")
  // is rejected — a 2-exponent currency needs minor digits), then add each line.
  let total: { units: bigint; currency: string } = { units: 0n, currency: CURRENCY };
  for (const it of items) {
    const amt = { units: BigInt(Math.trunc(it.cents)), currency: CURRENCY };
    total = money.add(total, amt) as { units: bigint; currency: string };
  }
  const invoice: Invoice = {
    appointment: appointmentId,
    items,
    totalCents: Number(total.units),
    totalFormatted: money.format(total) as string,
    currency: CURRENCY,
  };
  put(`inv_${appointmentId}`, invoice);
  return invoice;
}

export function getInvoice(appointmentId: string): Invoice | undefined {
  return read<Invoice>(`inv_${appointmentId}`);
}

export type DeleteResult = { ok: true } | { ok: false; reason: string };

/**
 * Delete a pet — only if it has no active bookings. `owner` scopes the action
 * (an owner may only delete their own pet).
 */
export function deletePet(petId: string, owner: string): DeleteResult {
  const pet = getPet(petId);
  if (!pet) return { ok: false, reason: "not_found" };
  if (pet.owner !== owner) return { ok: false, reason: "forbidden" };
  const active = activeAppointmentsForPet(petId);
  if (active.length > 0) return { ok: false, reason: "has_active_bookings" };
  bucket.delete(`pet_${petId}`);
  search.remove(petId); // keep the index in sync
  if (pet.photo) {
    try {
      blob.delete(PHOTO_CONTAINER, petId);
    } catch {
      /* best-effort photo cleanup */
    }
  }
  return { ok: true };
}

/**
 * Delete (cancel) an appointment — only if it's more than 24h away. `owner`
 * scopes the action. `nowSeconds` is injected so the rule is testable.
 */
export function deleteAppointment(apptId: string, owner: string, nowSeconds: number): DeleteResult {
  const appt = getAppointment(apptId);
  if (!appt) return { ok: false, reason: "not_found" };
  if (appt.owner !== owner) return { ok: false, reason: "forbidden" };
  const when = Date.parse(appt.datetime); // ms, NaN if unparseable
  if (Number.isNaN(when)) return { ok: false, reason: "bad_datetime" };
  const hoursAway = (when - nowSeconds * 1000) / 3_600_000;
  if (hoursAway < 24) return { ok: false, reason: "within_24h" };
  bucket.delete(`appt_${apptId}`);
  return { ok: true };
}

// ---- visit notes ---------------------------------------------------------

export function addNote(input: { appointment: string; author: string; text: string }): VisitNote {
  const note: VisitNote = { id: id("note"), at: Math.floor(Date.now() / 1000), ...input };
  put(`note_${note.id}`, note);
  return note;
}

export function notesFor(appointmentId: string): VisitNote[] {
  return scan<VisitNote>("note_")
    .filter((n) => n.appointment === appointmentId)
    .map((n) => ({ ...n, textHtml: renderMarkdown(n.text) }));
}

/** Render a doctor's markdown note to SAFE HTML via md:render (XSS-escaped). */
export function renderMarkdown(text: string): string {
  try {
    return markdown.toHtml(text) as string;
  } catch {
    return text; // fall back to raw text if the renderer errors
  }
}

// ---- audit trail ---------------------------------------------------------
// The composed auth-guard wires audit:log/recorder internally and writes each
// event to the shared KV under `al_{timestamp}_{id}`. It does NOT re-export the
// audit `query` interface, so the admin view reads those keys directly. Returns
// newest-first.

export interface AuditEvent {
  id: string;
  timestamp: number;
  event: string;
  outcome: string;
  tenant: string;
  subject: string;
  detail: string;
}

export function readAuditTrail(max = 100): AuditEvent[] {
  const { keys } = bucket.listKeys(undefined) as { keys: string[] };
  return keys
    .filter((k) => k.startsWith("al_"))
    .sort()
    .reverse()
    .slice(0, max)
    .map((k) => {
      const raw = bucket.get(k);
      if (!raw) return undefined;
      const e = JSON.parse(dec(raw)) as Record<string, unknown>;
      return {
        id: String(e.id ?? ""),
        timestamp: Number(e.timestamp ?? 0),
        event: String(e.event ?? ""),
        outcome: String(e.outcome ?? ""),
        tenant: String(e.tenant ?? ""),
        subject: String(e.subject ?? ""),
        detail: String(e.detail ?? ""),
      };
    })
    .filter((e): e is AuditEvent => e !== undefined);
}
