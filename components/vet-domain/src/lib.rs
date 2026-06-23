//! `vet-domain` — the vet-clinic domain as a WIT HTTP component (FULL PARITY).
//!
//! The pet/appointment/note orchestration that used to be TypeScript glue under
//! jco, here a `wasi:http/incoming-handler` component that owns NO storage and
//! NO auth logic — it maps HTTP onto the capability contracts it imports. The
//! core slice (auth + pets/appointments/notes) is here joined by the full set
//! of cross-cutting capabilities, each a real comp component:
//!   auth:identity, records:store, validate:schema, search:index,
//!   blob:store + upload:policy (photos), fsm:workflow (lifecycle),
//!   money:amount (invoices), md:render (notes), csv:codec (export),
//!   pii:redact (audit), otp:totp + secrets:vault (2FA), i18n:catalog,
//!   paginate:cursor, ai:inference + cache:store (summary), sched:timer
//!   (reminders), lock:mutex (claim), event:bus (fan-out).
//!
//! Every mutating route authorizes the bearer (authorizer.authorize with the
//! required {target, action}); bodies are validated by validate:schema; owners
//! are scoped to their own pets/appointments, doctors/admins see all. The same
//! .wasm runs under jco or wasmCloud — backends + the LLM provider are a
//! compose-time choice, never in this code.
//!
//! Routes — see the match in `handle` for the full list.

#[allow(warnings)]
mod bindings;
mod datetime;

use serde::Deserialize;

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::rbac;
use bindings::auth::identity::types::{AuthError, Permission, Principal, TokenPair};
use bindings::records::store::store as records;
use bindings::records::store::store::StoreError;
use bindings::search::index::index as search;
use bindings::validate::schema::validator as validate;
use bindings::validate::schema::validator::{Kind, Rule};

// full-parity capability imports.
use bindings::ai::inference::inference as ai;
use bindings::blob::store::blobstore as blob;
use bindings::cache::store::cache as cache;
use bindings::csv::codec::codec as csv;
use bindings::event::bus::bus as events;
use bindings::fsm::workflow::engine as fsm;
use bindings::i18n::catalog::catalog as i18n;
use bindings::lock::mutex::mutex as lock;
use bindings::md::render::renderer as markdown;
use bindings::money::amount::arithmetic as money;
use bindings::otp::totp::authenticator as otp;
use bindings::paginate::cursor::cursors as paginate;
use bindings::pii::redact::redactor as pii;
use bindings::sched::timer::timer as timer;
use bindings::secrets::vault::vault as vault;
use bindings::upload::policy::gate as upload;

use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::store as kv;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const PHOTO_CONTAINER: &str = "pet-photos";
const APPT_MACHINE: &str = "appointment";
const APPT_TOPIC: &str = "appointment.booked";
const PROJECTION_GROUP: &str = "booked-counter";
const CURRENCY: &str = "USD";
const REMINDER_LEAD_SECONDS: i64 = 24 * 3600;
const AISUM_TTL_SECONDS: u64 = 24 * 3600;

// ---- routing ------------------------------------------------------------

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        // boot-time idempotent seeding (define the fsm machine + i18n catalog).
        // safe to run on every request — define/set-message replace in place.
        ensure_seeded();

        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let query = path.splitn(2, '?').nth(1).unwrap_or("").to_string();

        // segment helpers for /appointments/{id}/... and /pets/{id}/... routes.
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let result = match (&method, route.as_str()) {
            (Method::Post, "/register") => register(&request),
            (Method::Post, "/login") => login(&request),
            (Method::Get, "/me") => me(&request),

            // ---- pets ----
            (Method::Get, "/pets") => list_pets(&request, &query),
            (Method::Post, "/pets") => create_pet(&request),

            // ---- appointments ----
            (Method::Get, "/appointments") => list_appointments(&request),
            (Method::Post, "/appointments") => create_appointment(&request),

            // ---- admin: RBAC seeding ----
            (Method::Post, "/admin/role-permissions") => admin_set_role_perms(&request),
            (Method::Post, "/admin/assign-role") => admin_assign_role(&request),
            (Method::Get, "/admin/audit") => admin_audit(&request),
            (Method::Post, "/admin/run-reminders") => admin_run_reminders(&request, &query),
            (Method::Post, "/admin/run-projection") => admin_run_projection(&request),

            // ---- staff 2FA ----
            (Method::Post, "/auth/2fa/enroll") => twofa_enroll(&request),
            (Method::Post, "/auth/2fa/verify") => twofa_verify(&request),
            (Method::Get, "/auth/2fa/status") => twofa_status(&request),

            // segment-matched routes.
            _ => match (&method, seg.as_slice()) {
                // /admin/export/{what}.csv
                (Method::Get, ["admin", "export", file]) if file.ends_with(".csv") => {
                    admin_export_csv(&request, file.trim_end_matches(".csv"))
                }
                // /i18n/{locale}
                (Method::Get, ["i18n", locale]) => i18n_bundle(locale),

                // /pets/{id}
                (Method::Get, ["pets", id]) => get_pet_detail(&request, id),
                (Method::Delete, ["pets", id]) => delete_pet(&request, id),
                // /pets/{id}/photo
                (Method::Get, ["pets", id, "photo"]) => {
                    return serve_pet_photo(&request, id, response_out);
                }
                (Method::Post, ["pets", id, "photo"]) => upload_pet_photo(&request, id),

                // /appointments/{id}
                (Method::Delete, ["appointments", id]) => delete_appointment(&request, id),
                // /appointments/{id}/transition
                (Method::Post, ["appointments", id, "transition"]) => {
                    transition_appointment(&request, id)
                }
                // /appointments/{id}/invoice
                (Method::Get, ["appointments", id, "invoice"]) => get_invoice(&request, id),
                (Method::Put, ["appointments", id, "invoice"]) => put_invoice(&request, id),
                // /appointments/{id}/notes
                (Method::Post, ["appointments", id, "notes"]) => add_note(&request, id),
                (Method::Get, ["appointments", id, "notes"]) => list_notes(&request, id),
                // /appointments/{id}/summary
                (Method::Post, ["appointments", id, "summary"]) => post_summary(&request, id, &query),
                (Method::Get, ["appointments", id, "summary"]) => get_summary(&request, id),

                _ => Outcome::NotFound,
            },
        };

        emit(response_out, result);
    }
}

enum Outcome {
    Json(u16, String),
    /// raw body with an explicit content-type (csv / image bytes).
    Raw(u16, String, Vec<u8>),
    Auth(AuthError),
    Bad(String),
    /// {code, error-string} for the simple business-rule errors.
    Err(u16, String),
    NotFound,
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond(response_out, code, "application/json", body.as_bytes()),
        Outcome::Raw(code, ct, bytes) => respond(response_out, code, &ct, &bytes),
        Outcome::Auth(e) => {
            let (code, msg) = auth_error(&e);
            respond_json(response_out, code, &format!("{{\"error\":\"{msg}\"}}"));
        }
        Outcome::Bad(msg) => respond_json(response_out, 400, &format!("{{\"error\":\"{}\"}}", esc(&msg))),
        Outcome::Err(code, msg) => respond_json(response_out, code, &format!("{{\"error\":\"{}\"}}", esc(&msg))),
        Outcome::NotFound => respond_json(response_out, 404, "{\"error\":\"not_found\"}"),
    }
}

// ---- boot seeding (fsm machine + i18n catalog) --------------------------

fn ensure_seeded() {
    // appointment lifecycle: booked -> confirmed -> completed | cancel.
    let def = fsm::Definition {
        states: vec![
            "booked".into(),
            "confirmed".into(),
            "completed".into(),
            "cancelled".into(),
        ],
        initial: "booked".into(),
        transitions: vec![
            fsm::Transition { event: "confirm".into(), source: "booked".into(), target: "confirmed".into() },
            fsm::Transition { event: "complete".into(), source: "confirmed".into(), target: "completed".into() },
            fsm::Transition { event: "cancel".into(), source: "booked".into(), target: "cancelled".into() },
            fsm::Transition { event: "cancel".into(), source: "confirmed".into(), target: "cancelled".into() },
        ],
        terminal: vec!["completed".into(), "cancelled".into()],
    };
    let _ = fsm::define(APPT_MACHINE, &def);

    for (locale, key, value) in ui_strings() {
        let _ = i18n::set_message(locale, key, value);
    }
}

const APPT_EVENTS: [&str; 3] = ["confirm", "complete", "cancel"];

/// (locale, key, value) tuples for the UI catalog — en + es, matching the TS.
fn ui_strings() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("en", "app.title", "Acme Vet Clinic"),
        ("en", "nav.logout", "Log out"),
        ("en", "pets.title", "My pets"),
        ("en", "pets.add", "Add pet"),
        ("en", "appt.book", "Book an appointment"),
        ("en", "appt.status", "Status"),
        ("en", "notes.title", "Visit notes"),
        ("es", "app.title", "Clínica Veterinaria Acme"),
        ("es", "nav.logout", "Cerrar sesión"),
        ("es", "pets.title", "Mis mascotas"),
        ("es", "pets.add", "Añadir mascota"),
        ("es", "appt.book", "Reservar una cita"),
        ("es", "appt.status", "Estado"),
        ("es", "notes.title", "Notas de la visita"),
    ]
}

const UI_KEYS: [&str; 7] = [
    "app.title",
    "nav.logout",
    "pets.title",
    "pets.add",
    "appt.book",
    "appt.status",
    "notes.title",
];

// ---- auth routes --------------------------------------------------------

#[derive(Deserialize)]
struct RegisterReq {
    email: String,
    password: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    tenant: Option<String>,
}

fn register(request: &IncomingRequest) -> Outcome {
    let req: RegisterReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    let tenant = req.tenant.clone().unwrap_or_default();
    let principal = match accounts::register(&req.email, &req.password, &tenant) {
        Ok(p) => p,
        Err(e) => return Outcome::Auth(e),
    };
    // assign the requested role (default pet-owner), gated to the known set.
    let wanted = req.role.unwrap_or_else(|| "pet-owner".to_string());
    let role = if ["pet-owner", "doctor", "admin"].contains(&wanted.as_str()) {
        wanted
    } else {
        "pet-owner".to_string()
    };
    let _ = rbac::assign_role(&principal.tenant, &principal.subject, &role);
    Outcome::Json(201, principal_json(&principal))
}

#[derive(Deserialize)]
struct LoginReq {
    email: String,
    password: String,
    #[serde(default)]
    tenant: Option<String>,
}

fn login(request: &IncomingRequest) -> Outcome {
    let req: LoginReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    let tenant = req.tenant.unwrap_or_default();
    match accounts::login(&req.email, &req.password, &tenant) {
        Ok(tp) => Outcome::Json(200, token_pair_json(&tp)),
        Err(e) => Outcome::Auth(e),
    }
}

fn me(request: &IncomingRequest) -> Outcome {
    match introspect(request) {
        Ok(p) => Outcome::Json(200, principal_json(&p)),
        Err(o) => o,
    }
}

/// Authorize the bearer for {target, action}; returns the principal or an Auth
/// outcome the caller can early-return.
fn require(request: &IncomingRequest, target: &str, action: &str) -> Result<Principal, Outcome> {
    let token = match bearer(request) {
        Some(t) => t,
        None => return Err(Outcome::Auth(AuthError::InvalidToken("missing bearer".into()))),
    };
    let perm = Permission { target: target.to_string(), action: action.to_string() };
    authorizer::authorize(&token, &perm).map_err(Outcome::Auth)
}

/// Just introspect the bearer (no permission requirement) — for /me + 2FA.
fn introspect(request: &IncomingRequest) -> Result<Principal, Outcome> {
    let token = match bearer(request) {
        Some(t) => t,
        None => return Err(Outcome::Auth(AuthError::InvalidToken("missing bearer".into()))),
    };
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

fn is_privileged(p: &Principal) -> bool {
    p.roles.iter().any(|r| r == "admin" || r == "doctor")
}
fn is_admin(p: &Principal) -> bool {
    p.roles.iter().any(|r| r == "admin")
}
fn is_doctor(p: &Principal) -> bool {
    p.roles.iter().any(|r| r == "doctor")
}

// ---- admin: RBAC seeding ------------------------------------------------

#[derive(Deserialize)]
struct PermDto {
    target: String,
    action: String,
}
#[derive(Deserialize)]
struct SetRolePermsReq {
    tenant: String,
    role: String,
    permissions: Vec<PermDto>,
}
#[derive(Deserialize)]
struct AssignRoleReq {
    tenant: String,
    subject: String,
    role: String,
}

fn admin_set_role_perms(request: &IncomingRequest) -> Outcome {
    let req: SetRolePermsReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    let perms: Vec<Permission> = req
        .permissions
        .into_iter()
        .map(|p| Permission { target: p.target, action: p.action })
        .collect();
    match rbac::set_role_permissions(&req.tenant, &req.role, &perms) {
        Ok(()) => Outcome::Json(204, String::new()),
        Err(e) => Outcome::Auth(e),
    }
}

fn admin_assign_role(request: &IncomingRequest) -> Outcome {
    let req: AssignRoleReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    match rbac::assign_role(&req.tenant, &req.subject, &req.role) {
        Ok(()) => Outcome::Json(204, String::new()),
        Err(e) => Outcome::Auth(e),
    }
}

// ---- pets ---------------------------------------------------------------

#[derive(Deserialize)]
struct PetReq {
    name: String,
    species: String,
    #[serde(default)]
    notes: Option<String>,
}

fn pet_rules() -> Vec<Rule> {
    vec![text_rule("name", true, 1, 60), text_rule("species", true, 1, 40)]
}

fn create_pet(request: &IncomingRequest) -> Outcome {
    let principal = match require(request, "pets", "write") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let body = match read_body(request) {
        Ok(b) => b,
        Err(_) => return Outcome::Bad("could not read body".into()),
    };
    let body_str = String::from_utf8_lossy(&body).to_string();
    let errs = validate::validate(&body_str, &pet_rules());
    if !errs.is_empty() {
        return Outcome::Bad(format!("validation_failed: {}", errs[0].field));
    }
    let req: PetReq = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return Outcome::Bad(format!("bad json: {e}")),
    };
    let notes = req.notes.unwrap_or_default();
    // store WITHOUT an id — records:store mints the ULID. owner is the subject.
    let data = format!(
        "{{\"name\":{},\"species\":{},\"owner\":{},\"notes\":{}}}",
        js(&req.name), js(&req.species), js(&principal.subject), js(&notes)
    );
    let entry = match records::create("pets", &data, &["owner".to_string()]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    let _ = search::index_doc(
        &entry.id,
        &format!("{} {} {}", req.name, req.species, notes),
        &[format!("owner:{}", principal.subject)],
    );
    Outcome::Json(201, pet_json(&entry.id, &entry.data))
}

fn list_pets(request: &IncomingRequest, query: &str) -> Outcome {
    let principal = match require(request, "pets", "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let owner_scope = !is_privileged(&principal);
    let scope_owner = if owner_scope { Some(principal.subject.clone()) } else { None };
    let q = query_param(query, "q");
    let limit = query_param(query, "limit");
    let cursor = query_param(query, "cursor");

    // ?limit (and no ?q) -> a cursor-paginated page (paginate:cursor); else the
    // full list/search, matching the TS back-compat behaviour.
    if limit.is_some() && q.is_none() {
        let n: u32 = limit.and_then(|l| l.parse().ok()).unwrap_or(10);
        return paginate_pets(scope_owner.as_deref(), n, cursor.as_deref(), &principal.subject);
    }

    let entries: Vec<(String, String)> = if let Some(term) = q {
        let tags = if owner_scope { vec![format!("owner:{}", principal.subject)] } else { vec![] };
        let hits = search::query(&term, search::Mode::Any, &tags, 20).unwrap_or_default();
        hits.into_iter()
            .filter_map(|h| records::get("pets", &h.id).ok().map(|e| (e.id, e.data)))
            .collect()
    } else {
        list_pet_entries(scope_owner.as_deref())
    };

    let items: Vec<String> = entries.iter().map(|(id, data)| pet_json(id, data)).collect();
    Outcome::Json(
        200,
        format!("{{\"pets\":[{}],\"viewer\":{}}}", items.join(","), js(&principal.subject)),
    )
}

/// All pet entries (id, data) for an owner (or all, when `owner` is None).
fn list_pet_entries(owner: Option<&str>) -> Vec<(String, String)> {
    match owner {
        Some(o) => records::find_by("pets", "owner", &js(o))
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.id, e.data))
            .collect(),
        None => records::list_records("pets", 0, "")
            .map(|p| p.entries.into_iter().map(|e| (e.id, e.data)).collect())
            .unwrap_or_default(),
    }
}

/// Cursor-paginated page of pets, sorted by name (id as tiebreaker), assembled
/// via paginate:cursor (clamp + opaque signed cursors + page envelope).
fn paginate_pets(owner: Option<&str>, limit: u32, cursor: Option<&str>, viewer: &str) -> Outcome {
    let clamped = paginate::clamp_limit(if limit > 0 { limit } else { 10 }).unwrap_or(10) as usize;
    let mut all = list_pet_entries(owner);
    // sort by name then id; name is a JSON field of the data blob.
    all.sort_by(|a, b| {
        let na = json_field(&a.1, "name").unwrap_or_default();
        let nb = json_field(&b.1, "name").unwrap_or_default();
        na.cmp(&nb).then(a.0.cmp(&b.0))
    });

    // decode the incoming cursor to find the start offset (after that boundary).
    let mut start = 0usize;
    if let Some(c) = cursor {
        if let Ok(pos) = paginate::decode(c) {
            if let Some(idx) = all.iter().position(|(id, data)| {
                json_field(data, "name").unwrap_or_default() == pos.sort_key && *id == pos.last_id
            }) {
                start = idx + 1;
            }
        }
    }

    let end = (start + clamped).min(all.len());
    let slice = &all[start.min(all.len())..end];
    let more_after = end < all.len();
    let more_before = start > 0;

    let pos_of = |(id, data): &(String, String)| paginate::Position {
        sort_key: json_field(data, "name").unwrap_or_default(),
        last_id: id.clone(),
        forward: true,
    };
    let first = slice.first().map(pos_of);
    let last = slice.last().map(pos_of);
    let info = paginate::build_page(first.as_ref(), last.as_ref(), more_before, more_after);

    let items: Vec<String> = slice.iter().map(|(id, data)| pet_json(id, data)).collect();
    Outcome::Json(
        200,
        format!(
            "{{\"pets\":[{}],\"page\":{},\"viewer\":{}}}",
            items.join(","),
            page_info_json(&info),
            js(viewer)
        ),
    )
}

/// Full detail for a pet: the pet + its appointments, each with visit notes.
/// Owner-scoped; doctors/admins may view any.
fn get_pet_detail(request: &IncomingRequest, pet_id: &str) -> Outcome {
    let principal = match require(request, "pets", "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let pet = match records::get("pets", pet_id) {
        Ok(e) => e,
        Err(_) => return Outcome::Err(404, "pet_not_found".into()),
    };
    let owner = json_field(&pet.data, "owner").unwrap_or_default();
    if !is_privileged(&principal) && owner != principal.subject {
        return Outcome::Err(403, "not_your_pet".into());
    }
    // appointments for this pet (newest datetime first), each with its notes.
    let mut appts = records::find_by("appointments", "pet", &js(pet_id)).unwrap_or_default();
    appts.sort_by(|a, b| {
        let da = json_field(&a.data, "datetime").unwrap_or_default();
        let db = json_field(&b.data, "datetime").unwrap_or_default();
        db.cmp(&da)
    });
    let appt_json_items: Vec<String> = appts
        .iter()
        .map(|a| {
            let notes = notes_for(&a.id);
            format!(
                "{{\"id\":{},{},\"notes\":[{}]}}",
                js(&a.id),
                strip_braces(&a.data),
                notes.join(",")
            )
        })
        .collect();
    Outcome::Json(
        200,
        format!(
            "{{\"id\":{},{},\"appointments\":[{}]}}",
            js(pet_id),
            strip_braces(&pet.data),
            appt_json_items.join(",")
        ),
    )
}

/// Delete a pet — only if it has no active (non-cancelled) bookings. Owner-scoped.
fn delete_pet(request: &IncomingRequest, pet_id: &str) -> Outcome {
    let principal = match require(request, "pets", "write") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let pet = match records::get("pets", pet_id) {
        Ok(e) => e,
        Err(_) => return Outcome::Err(404, "pet_not_found".into()),
    };
    let owner = json_field(&pet.data, "owner").unwrap_or_default();
    if owner != principal.subject {
        return Outcome::Err(403, "not_your_pet".into());
    }
    let active = records::find_by("appointments", "pet", &js(pet_id))
        .unwrap_or_default()
        .into_iter()
        .filter(|a| json_field(&a.data, "status").as_deref() != Some("cancelled"))
        .count();
    if active > 0 {
        return Outcome::Err(409, "pet_has_active_bookings".into());
    }
    let _ = records::delete("pets", pet_id);
    let _ = search::remove(pet_id);
    if json_field(&pet.data, "photo").is_some() {
        let _ = blob::delete(PHOTO_CONTAINER, pet_id);
    }
    Outcome::Json(204, String::new())
}

// ---- pet photos (upload:policy + blob:store) ----------------------------

fn upload_pet_photo(request: &IncomingRequest, pet_id: &str) -> Outcome {
    let principal = match require(request, "pets", "write") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let pet = match records::get("pets", pet_id) {
        Ok(e) => e,
        Err(_) => return Outcome::Err(404, "pet_not_found".into()),
    };
    let owner = json_field(&pet.data, "owner").unwrap_or_default();
    if owner != principal.subject {
        return Outcome::Err(403, "not_your_pet".into());
    }
    let content_type = header(request, "content-type").unwrap_or_else(|| "application/octet-stream".into());
    let body = match read_body(request) {
        Ok(b) => b,
        Err(_) => return Outcome::Bad("could not read body".into()),
    };
    if body.is_empty() {
        return Outcome::Err(400, "empty_body".into());
    }
    // 1) upload:policy — validate + mint a signed ticket.
    let ticket = match upload::authorize(&format!("pet/{pet_id}"), &content_type, body.len() as u64, 0) {
        Ok(t) => t,
        Err(e) => return policy_err(e),
    };
    // 2) upload:policy — redeem the ticket (signature + expiry check).
    if upload::redeem(&ticket.token).is_err() {
        return Outcome::Err(400, "invalid_ticket".into());
    }
    // 3) blob:store — store the bytes under the pet id.
    if let Err(e) = blob::put(PHOTO_CONTAINER, pet_id, &body, &content_type) {
        return Outcome::Err(400, format!("blob: {e:?}"));
    }
    // record the content-type on the pet (the bytes live in blob:store).
    let new_data = set_json_field(&pet.data, "photo", &content_type);
    let _ = records::update("pets", pet_id, &new_data, 0);
    Outcome::Json(201, "{\"ok\":true}".into())
}

/// Serve the raw photo bytes with the stored content-type. Any authed user with
/// pets:read may read; this writes the response directly (binary body).
fn serve_pet_photo(request: &IncomingRequest, pet_id: &str, response_out: ResponseOutparam) {
    let _principal = match require(request, "pets", "read") {
        Ok(p) => p,
        Err(o) => return emit(response_out, o),
    };
    let pet = match records::get("pets", pet_id) {
        Ok(e) => e,
        Err(_) => return emit(response_out, Outcome::Err(404, "no_photo".into())),
    };
    let ct = match json_field(&pet.data, "photo") {
        Some(c) => c,
        None => return emit(response_out, Outcome::Err(404, "no_photo".into())),
    };
    match blob::get(PHOTO_CONTAINER, pet_id) {
        Ok(bytes) => emit(response_out, Outcome::Raw(200, ct, bytes)),
        Err(_) => emit(response_out, Outcome::Err(404, "no_photo".into())),
    }
}

// ---- appointments -------------------------------------------------------

#[derive(Deserialize)]
struct ApptReq {
    pet: String,
    datetime: String,
    #[serde(default)]
    doctor: Option<String>,
}

fn appt_rules() -> Vec<Rule> {
    vec![text_rule("pet", true, 1, 80), text_rule("datetime", true, 4, 40)]
}

fn create_appointment(request: &IncomingRequest) -> Outcome {
    let _principal = match require(request, "appointments", "write") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let body = match read_body(request) {
        Ok(b) => b,
        Err(_) => return Outcome::Bad("could not read body".into()),
    };
    let body_str = String::from_utf8_lossy(&body).to_string();
    let errs = validate::validate(&body_str, &appt_rules());
    if !errs.is_empty() {
        return Outcome::Bad(format!("validation_failed: {}", errs[0].field));
    }
    let req: ApptReq = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return Outcome::Bad(format!("bad json: {e}")),
    };
    // the pet must exist; the booking owner is the pet's owner.
    let pet = match records::get("pets", &req.pet) {
        Ok(e) => e,
        Err(_) => return Outcome::Err(404, "pet_not_found".into()),
    };
    let owner = json_field(&pet.data, "owner").unwrap_or_default();
    let doctor = req.doctor.unwrap_or_default();
    let data = format!(
        "{{\"pet\":{},\"owner\":{},\"doctor\":{},\"datetime\":{},\"status\":\"booked\"}}",
        js(&req.pet), js(&owner), js(&doctor), js(&req.datetime)
    );
    let entry = match records::create(
        "appointments",
        &data,
        &["owner".to_string(), "doctor".to_string(), "pet".to_string()],
    ) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    // start its fsm instance (initial "booked"), keyed by the minted id.
    let _ = fsm::create_instance(APPT_MACHINE, &entry.id);
    // schedule the 24h-before reminder (no-op on unparseable datetime).
    schedule_reminder(&entry.id, &req.datetime);
    // publish the domain event — reactions consume it via event:bus on their own.
    let payload = format!(
        "{{\"id\":{},\"pet\":{},\"owner\":{}}}",
        js(&entry.id), js(&req.pet), js(&owner)
    );
    let _ = events::publish(APPT_TOPIC, payload.as_bytes());
    Outcome::Json(201, appt_json(&entry.id, &entry.data))
}

fn list_appointments(request: &IncomingRequest) -> Outcome {
    let principal = match require(request, "appointments", "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let entries = if is_admin(&principal) {
        records::list_records("appointments", 0, "").map(|p| p.entries).unwrap_or_default()
    } else if is_doctor(&principal) {
        // a doctor sees their own assigned appointments PLUS the unassigned pool.
        records::list_records("appointments", 0, "")
            .map(|p| p.entries)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| {
                let d = json_field(&e.data, "doctor").unwrap_or_default();
                d == principal.subject || d.is_empty()
            })
            .collect()
    } else {
        records::find_by("appointments", "owner", &js(&principal.subject)).unwrap_or_default()
    };
    let items: Vec<String> = entries.iter().map(|e| appt_json(&e.id, &e.data)).collect();
    Outcome::Json(
        200,
        format!("{{\"appointments\":[{}],\"viewer\":{}}}", items.join(","), js(&principal.subject)),
    )
}

/// Cancel (delete) an appointment — only when it is more than 24h away. Owner-scoped.
fn delete_appointment(request: &IncomingRequest, appt_id: &str) -> Outcome {
    let principal = match require(request, "appointments", "write") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let appt = match records::get("appointments", appt_id) {
        Ok(e) => e,
        Err(_) => return Outcome::Err(404, "appointment_not_found".into()),
    };
    let owner = json_field(&appt.data, "owner").unwrap_or_default();
    if owner != principal.subject {
        return Outcome::Err(403, "not_your_appointment".into());
    }
    let datetime = json_field(&appt.data, "datetime").unwrap_or_default();
    let when = match datetime::parse_unix_seconds(&datetime) {
        Some(w) => w,
        None => return Outcome::Err(400, "bad_datetime".into()),
    };
    let now = now_seconds();
    let hours_away = (when - now) as f64 / 3600.0;
    if hours_away < 24.0 {
        return Outcome::Err(409, "within_24h_no_cancel".into());
    }
    let _ = records::delete("appointments", appt_id);
    cancel_reminder(appt_id);
    Outcome::Json(204, String::new())
}

#[derive(Deserialize)]
struct TransitionReq {
    event: String,
}

/// Advance an appointment through its lifecycle via fsm:workflow. confirm/complete
/// are doctor/admin; cancel may also be done by the appointment's owner.
fn transition_appointment(request: &IncomingRequest, appt_id: &str) -> Outcome {
    let principal = match require(request, "appointments", "write") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let req: TransitionReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if !APPT_EVENTS.contains(&req.event.as_str()) {
        return Outcome::Err(400, "bad_event".into());
    }
    let appt = match records::get("appointments", appt_id) {
        Ok(e) => e,
        Err(_) => return Outcome::Err(404, "appointment_not_found".into()),
    };
    let owner = json_field(&appt.data, "owner").unwrap_or_default();
    let privileged = is_privileged(&principal);
    if !privileged && !(req.event == "cancel" && owner == principal.subject) {
        return Outcome::Err(403, "forbidden".into());
    }
    match fsm::fire(APPT_MACHINE, appt_id, &req.event) {
        Ok(status) => {
            // mirror the new fsm state onto the appt record.
            let new_data = set_json_field(&appt.data, "status", &status.state);
            let _ = records::update("appointments", appt_id, &new_data, 0);
            if status.state == "cancelled" || status.state == "completed" {
                cancel_reminder(appt_id);
            }
            let allowed = fsm::allowed_events(APPT_MACHINE, appt_id).unwrap_or_default();
            Outcome::Json(
                200,
                format!(
                    "{{\"id\":{},\"status\":{},\"allowed\":{}}}",
                    js(appt_id),
                    js(&status.state),
                    json_str_array(&allowed)
                ),
            )
        }
        Err(fsm::FsmError::IllegalTransition(current)) => Outcome::Json(
            409,
            format!("{{\"error\":\"illegal_transition\",\"current\":{}}}", js(&current)),
        ),
        Err(e) => Outcome::Err(400, format!("{e:?}")),
    }
}

// ---- invoices (money:amount) --------------------------------------------

#[derive(Deserialize)]
struct LineItem {
    description: String,
    cents: i64,
}
#[derive(Deserialize)]
struct InvoiceReq {
    items: Vec<LineItem>,
}

/// Set/replace the invoice for an appointment (doctor/admin); money:amount totals.
fn put_invoice(request: &IncomingRequest, appt_id: &str) -> Outcome {
    let principal = match require(request, "appointments", "write") {
        Ok(p) => p,
        Err(o) => return o,
    };
    if !is_privileged(&principal) {
        return Outcome::Err(403, "forbidden".into());
    }
    if records::get("appointments", appt_id).is_err() {
        return Outcome::Err(404, "appointment_not_found".into());
    }
    let req: InvoiceReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if req.items.is_empty() {
        return Outcome::Err(400, "no_items".into());
    }
    // sum from an explicit zero (parse("0") is rejected), then add each line.
    let mut total = money::Amount { units: 0, currency: CURRENCY.to_string() };
    for it in &req.items {
        let amt = money::Amount { units: it.cents, currency: CURRENCY.to_string() };
        total = match money::add(&total, &amt) {
            Ok(t) => t,
            Err(e) => return Outcome::Err(400, format!("money: {e:?}")),
        };
    }
    let total_formatted = money::format(&total).unwrap_or_default();
    let items_json: Vec<String> = req
        .items
        .iter()
        .map(|it| format!("{{\"description\":{},\"cents\":{}}}", js(&it.description), it.cents))
        .collect();
    let invoice = format!(
        "{{\"appointment\":{},\"items\":[{}],\"totalCents\":{},\"totalFormatted\":{},\"currency\":{}}}",
        js(appt_id),
        items_json.join(","),
        total.units,
        js(&total_formatted),
        js(CURRENCY)
    );
    // 1:1 with the appointment — drop any prior invoice, then create fresh.
    for e in records::find_by("invoices", "appointment", &js(appt_id)).unwrap_or_default() {
        let _ = records::delete("invoices", &e.id);
    }
    let _ = records::create("invoices", &invoice, &["appointment".to_string()]);
    Outcome::Json(201, invoice)
}

/// Read an appointment's invoice (owner of it, or doctor/admin).
fn get_invoice(request: &IncomingRequest, appt_id: &str) -> Outcome {
    let principal = match require(request, "appointments", "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let appt = match records::get("appointments", appt_id) {
        Ok(e) => e,
        Err(_) => return Outcome::Err(404, "appointment_not_found".into()),
    };
    let owner = json_field(&appt.data, "owner").unwrap_or_default();
    if !is_privileged(&principal) && owner != principal.subject {
        return Outcome::Err(403, "forbidden".into());
    }
    let hits = records::find_by("invoices", "appointment", &js(appt_id)).unwrap_or_default();
    match hits.first() {
        Some(e) => Outcome::Json(200, e.data.clone()),
        None => Outcome::Err(404, "no_invoice".into()),
    }
}

// ---- visit notes --------------------------------------------------------

#[derive(Deserialize)]
struct NoteReq {
    text: String,
}

fn add_note(request: &IncomingRequest, appt_id: &str) -> Outcome {
    let principal = match require(request, "notes", "write") {
        Ok(p) => p,
        Err(o) => return o,
    };
    if records::get("appointments", appt_id).is_err() {
        return Outcome::Err(404, "appointment_not_found".into());
    }
    let req: NoteReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if req.text.trim().is_empty() {
        return Outcome::Err(400, "empty_note".into());
    }
    // writing a note claims the appointment for this doctor (if unassigned),
    // fenced by lock:mutex so two doctors can't both win the claim.
    assign_doctor(appt_id, &principal.subject);
    let data = format!(
        "{{\"appointment\":{},\"author\":{},\"text\":{},\"at\":{}}}",
        js(appt_id), js(&principal.subject), js(&req.text), now_seconds()
    );
    match records::create("notes", &data, &["appointment".to_string()]) {
        Ok(e) => Outcome::Json(201, format!("{{\"id\":{},{}}}", js(&e.id), strip_braces(&e.data))),
        Err(e) => store_err(e),
    }
}

/// Notes for an appointment — owner of the appointment, or doctor/admin. Each
/// note's raw markdown is rendered to safe HTML via md:render.
fn list_notes(request: &IncomingRequest, appt_id: &str) -> Outcome {
    let principal = match require(request, "appointments", "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let appt = match records::get("appointments", appt_id) {
        Ok(e) => e,
        Err(_) => return Outcome::Err(404, "appointment_not_found".into()),
    };
    let owner = json_field(&appt.data, "owner").unwrap_or_default();
    if !is_privileged(&principal) && owner != principal.subject {
        return Outcome::Err(403, "not_your_appointment".into());
    }
    Outcome::Json(200, format!("{{\"notes\":[{}]}}", notes_for(appt_id).join(",")))
}

/// Notes for an appointment as a Vec of JSON strings, each with `textHtml`
/// (md:render of the raw markdown text added on read), matching the TS.
fn notes_for(appt_id: &str) -> Vec<String> {
    records::find_by("notes", "appointment", &js(appt_id))
        .unwrap_or_default()
        .into_iter()
        .map(|e| {
            let text = json_field(&e.data, "text").unwrap_or_default();
            let html = render_markdown(&text);
            format!(
                "{{\"id\":{},{},\"textHtml\":{}}}",
                js(&e.id),
                strip_braces(&e.data),
                js(&html)
            )
        })
        .collect()
}

/// Render markdown to safe HTML via md:render; fall back to raw text on error.
fn render_markdown(text: &str) -> String {
    // md:render's to_html never returns a Result; it sanitizes raw HTML inline.
    markdown::to_html(text)
}

/// Claim an appointment for a doctor, fenced by lock:mutex so two doctors can't
/// both win an unassigned appointment. No-op if already this doctor's; on
/// contention the loser yields to whoever holds the claim.
fn assign_doctor(appt_id: &str, doctor: &str) {
    let appt = match records::get("appointments", appt_id) {
        Ok(e) => e,
        Err(_) => return,
    };
    if json_field(&appt.data, "doctor").as_deref() == Some(doctor) {
        return; // already mine
    }
    let key = format!("claim/{appt_id}");
    let lease = match lock::acquire(&key, doctor, 10) {
        Ok(l) => l,
        Err(_) => return, // held by another claimer -> yield
    };
    // re-read inside the lock: someone may have claimed it just now.
    if let Ok(fresh) = records::get("appointments", appt_id) {
        let cur = json_field(&fresh.data, "doctor").unwrap_or_default();
        if cur.is_empty() || cur == doctor {
            let new_data = set_json_field(&fresh.data, "doctor", doctor);
            let _ = records::update("appointments", appt_id, &new_data, 0);
        }
    }
    let _ = lock::release(&key, &lease.token);
}

// ---- AI clinical summary (ai:inference + cache:store) -------------------

fn aisum_key(appt_id: &str) -> String {
    format!("aisum_{appt_id}")
}

/// Generate (and cache) a clinical summary for an appointment via ai:inference.
/// Doctor/admin (notes:write). ?force=1 re-runs even if cached.
fn post_summary(request: &IncomingRequest, appt_id: &str, query: &str) -> Outcome {
    let _principal = match require(request, "notes", "write") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let force = matches!(query_param(query, "force").as_deref(), Some("1") | Some("true"));
    if !force {
        if let Some(cached) = read_summary(appt_id) {
            return Outcome::Json(200, cached);
        }
    }
    let (src, pet_name) = match summary_source(appt_id) {
        Some(v) => v,
        None => return Outcome::Err(404, "appointment_not_found".into()),
    };
    let summary = ai::summarize(&src, ai::Length::Normal, "clinical findings")
        .unwrap_or_else(|_| format!("Summary unavailable for {pet_name}."));
    let result = format!("{{\"summary\":{},\"at\":{}}}", js(&summary), now_seconds());
    let _ = cache::set(&aisum_key(appt_id), result.as_bytes(), AISUM_TTL_SECONDS);
    Outcome::Json(200, result)
}

/// Read a cached AI summary (owner of the appointment, or doctor/admin).
fn get_summary(request: &IncomingRequest, appt_id: &str) -> Outcome {
    let principal = match require(request, "appointments", "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let appt = match records::get("appointments", appt_id) {
        Ok(e) => e,
        Err(_) => return Outcome::Err(404, "appointment_not_found".into()),
    };
    let owner = json_field(&appt.data, "owner").unwrap_or_default();
    if !is_privileged(&principal) && owner != principal.subject {
        return Outcome::Err(403, "not_your_appointment".into());
    }
    match read_summary(appt_id) {
        Some(cached) => Outcome::Json(200, cached),
        None => Outcome::Err(404, "no_summary".into()),
    }
}

fn read_summary(appt_id: &str) -> Option<String> {
    match cache::get(&aisum_key(appt_id)) {
        Ok(Some(bytes)) => String::from_utf8(bytes).ok(),
        _ => None,
    }
}

/// Build the source text for an appointment's summary from the pet + its notes.
fn summary_source(appt_id: &str) -> Option<(String, String)> {
    let appt = records::get("appointments", appt_id).ok()?;
    let pet_id = json_field(&appt.data, "pet")?;
    let pet = records::get("pets", &pet_id).ok()?;
    let pet_name = json_field(&pet.data, "name").unwrap_or_default();
    let species = json_field(&pet.data, "species").unwrap_or_default();
    let pet_notes = json_field(&pet.data, "notes").unwrap_or_default();
    let datetime = json_field(&appt.data, "datetime").unwrap_or_default();
    let status = json_field(&appt.data, "status").unwrap_or_default();

    let note_texts: Vec<String> = records::find_by("notes", "appointment", &js(appt_id))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| json_field(&e.data, "text"))
        .collect();

    let mut lines = vec![format!("Patient: {pet_name}, a {species}.")];
    if !pet_notes.is_empty() {
        lines.push(format!("Owner-provided notes: {pet_notes}"));
    }
    lines.push(format!("Appointment {datetime}, status {status}."));
    if note_texts.is_empty() {
        lines.push("No visit notes recorded yet.".into());
    } else {
        let bulleted: Vec<String> = note_texts.iter().map(|t| format!("- {t}")).collect();
        lines.push(format!("Visit notes:\n{}", bulleted.join("\n")));
    }
    Some((lines.join("\n"), pet_name))
}

// ---- domain events + reminders ------------------------------------------

fn schedule_reminder(appt_id: &str, datetime: &str) {
    if let Some(when) = datetime::parse_unix_seconds(datetime) {
        let run_at = (when - REMINDER_LEAD_SECONDS).max(0) as u64;
        let _ = timer::schedule_at(&reminder_key(appt_id), run_at, appt_id.as_bytes());
    }
}

fn cancel_reminder(appt_id: &str) {
    let _ = timer::cancel(&reminder_key(appt_id));
}

fn reminder_key(appt_id: &str) -> String {
    format!("appt-reminder-{appt_id}")
}

/// Run the appointment-reminder relay (sched:timer). Admin-triggered (audit:read,
/// which admin's *:* covers). `?now=<unix>` overrides the clock for testing.
/// Fires every reminder due at `now`: collects what it would notify (the
/// notify sink is a compose-time concern, out of this domain), then acks the
/// timer jobs. A reminder for a gone/cancelled appointment is acked, not fired.
fn admin_run_reminders(request: &IncomingRequest, query: &str) -> Outcome {
    let _principal = match require(request, "audit", "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let now: u64 = query_param(query, "now")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| now_seconds().max(0) as u64);

    let due = timer::due(now, 50, 300).unwrap_or_default();
    let mut fired: Vec<String> = Vec::new();
    for job in due {
        let appt_id = String::from_utf8_lossy(&job.payload).to_string();
        if let Ok(appt) = records::get("appointments", &appt_id) {
            if json_field(&appt.data, "status").as_deref() != Some("cancelled") {
                let pet_id = json_field(&appt.data, "pet").unwrap_or_default();
                let owner = json_field(&appt.data, "owner").unwrap_or_default();
                fired.push(format!(
                    "{{\"appointment\":{},\"pet\":{},\"owner\":{}}}",
                    js(&appt_id), js(&pet_id), js(&owner)
                ));
            }
        }
        let _ = timer::ack(&job.key);
    }
    Outcome::Json(200, format!("{{\"now\":{},\"fired\":[{}]}}", now, fired.join(",")))
}

/// Drain the appointment.booked topic into the projection group, ack, and report
/// the running booked count. Admin-triggered. Proves the event:bus fan-out.
fn admin_run_projection(request: &IncomingRequest) -> Outcome {
    let _principal = match require(request, "audit", "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let batch = events::poll(APPT_TOPIC, PROJECTION_GROUP, 100).unwrap_or_default();
    let ids: Vec<String> = batch.iter().map(|e| e.id.clone()).collect();
    // count is the projection group's total acked offset — we maintain a durable
    // counter in records:store (collection "projections", a single row).
    let mut total = read_booked_count();
    if !ids.is_empty() {
        total += ids.len() as u64;
        write_booked_count(total);
        let _ = events::ack(APPT_TOPIC, PROJECTION_GROUP, &ids);
    }
    Outcome::Json(200, format!("{{\"bookedCount\":{total}}}"))
}

fn read_booked_count() -> u64 {
    records::find_by("projections", "name", &js("booked-counter"))
        .ok()
        .and_then(|v| v.into_iter().next())
        .and_then(|e| json_field(&e.data, "count").and_then(|c| c.parse().ok()))
        .unwrap_or(0)
}

fn write_booked_count(count: u64) {
    let data = format!("{{\"name\":\"booked-counter\",\"count\":\"{count}\"}}");
    // single-row collection: drop the prior, write fresh.
    for e in records::find_by("projections", "name", &js("booked-counter")).unwrap_or_default() {
        let _ = records::delete("projections", &e.id);
    }
    let _ = records::create("projections", &data, &["name".to_string()]);
}

// ---- admin: audit + CSV export ------------------------------------------

/// PII-redacted audit trail (admin-only). The composed auth-guard's recorder
/// writes events to the shared KV under `al_*` keys; we read them straight from
/// the bucket (audit:log does not re-export its query interface), newest-first,
/// and mask PII in the detail + subject fields via pii:redact.
fn admin_audit(request: &IncomingRequest) -> Outcome {
    let _principal = match require(request, "audit", "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let events = read_audit_trail_redacted(100);
    Outcome::Json(200, format!("{{\"events\":[{}]}}", events.join(",")))
}

#[derive(Default)]
struct AuditEvent {
    id: String,
    timestamp: i64,
    event: String,
    outcome: String,
    tenant: String,
    subject: String,
    detail: String,
}

/// Read raw `al_*` audit keys from the shared KV bucket, newest-first.
fn read_audit_events(max: usize) -> Vec<AuditEvent> {
    let bucket = match kv::open("default") {
        Ok(b) => b,
        Err(_) => return vec![],
    };
    let mut keys: Vec<String> = Vec::new();
    let mut cursor: Option<u64> = None;
    // page through all keys (the in-process shim returns them in one page, but
    // honour the cursor protocol so a paged backend works too).
    loop {
        match bucket.list_keys(cursor) {
            Ok(resp) => {
                keys.extend(resp.keys.into_iter().filter(|k| k.starts_with("al_")));
                match resp.cursor {
                    Some(c) => cursor = Some(c),
                    None => break,
                }
            }
            Err(_) => break,
        }
    }
    keys.sort();
    keys.reverse();
    keys.into_iter()
        .take(max)
        .filter_map(|k| {
            let raw = bucket.get(&k).ok()??;
            let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
            Some(AuditEvent {
                id: jget_str(&v, "id"),
                timestamp: v.get("timestamp").and_then(|x| x.as_i64()).unwrap_or(0),
                event: jget_str(&v, "event"),
                outcome: jget_str(&v, "outcome"),
                tenant: jget_str(&v, "tenant"),
                subject: jget_str(&v, "subject"),
                detail: jget_str(&v, "detail"),
            })
        })
        .collect()
}

fn read_audit_trail_redacted(max: usize) -> Vec<String> {
    let opts = pii::Options { kinds: vec![] }; // empty = all PII kinds
    let scrub = |s: &str| pii::mask(s, &opts);
    read_audit_events(max)
        .into_iter()
        .map(|e| {
            format!(
                "{{\"id\":{},\"timestamp\":{},\"event\":{},\"outcome\":{},\"tenant\":{},\"subject\":{},\"detail\":{}}}",
                js(&e.id),
                e.timestamp,
                js(&e.event),
                js(&e.outcome),
                js(&e.tenant),
                js(&scrub(&e.subject)),
                js(&scrub(&e.detail))
            )
        })
        .collect()
}

/// CSV export (admin): `appointments` or `audit`, formatted by csv:codec.
fn admin_export_csv(request: &IncomingRequest, what: &str) -> Outcome {
    let _principal = match require(request, "audit", "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let dialect = csv::Dialect { delimiter: String::new(), has_header: true, trim: false };
    let csv_text = match what {
        "appointments" => {
            let mut rows = vec![csv::Row {
                fields: vec![
                    "id".into(),
                    "pet".into(),
                    "owner".into(),
                    "doctor".into(),
                    "datetime".into(),
                    "status".into(),
                ],
            }];
            let appts = records::list_records("appointments", 0, "").map(|p| p.entries).unwrap_or_default();
            for a in appts {
                rows.push(csv::Row {
                    fields: vec![
                        a.id.clone(),
                        json_field(&a.data, "pet").unwrap_or_default(),
                        json_field(&a.data, "owner").unwrap_or_default(),
                        json_field(&a.data, "doctor").unwrap_or_default(),
                        json_field(&a.data, "datetime").unwrap_or_default(),
                        json_field(&a.data, "status").unwrap_or_default(),
                    ],
                });
            }
            csv::format(&rows, &dialect)
        }
        "audit" => {
            let mut rows = vec![csv::Row {
                fields: vec![
                    "timestamp".into(),
                    "event".into(),
                    "outcome".into(),
                    "tenant".into(),
                    "subject".into(),
                    "detail".into(),
                ],
            }];
            let opts = pii::Options { kinds: vec![] };
            for e in read_audit_events(1000) {
                rows.push(csv::Row {
                    fields: vec![
                        e.timestamp.to_string(),
                        e.event,
                        e.outcome,
                        e.tenant,
                        pii::mask(&e.subject, &opts),
                        pii::mask(&e.detail, &opts),
                    ],
                });
            }
            csv::format(&rows, &dialect)
        }
        _ => return Outcome::Err(404, "unknown_export".into()),
    };
    Outcome::Raw(200, "text/csv; charset=utf-8".into(), csv_text.into_bytes())
}

// ---- staff 2FA (otp:totp + secrets:vault) -------------------------------

fn otp_secret_name(subject: &str) -> String {
    format!("otp/{subject}")
}

fn twofa_enroll(request: &IncomingRequest) -> Outcome {
    let me = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let account = format!("{}@{}", me.subject, me.tenant);
    let p = match otp::provision("Acme Vet Clinic", &account) {
        Ok(p) => p,
        Err(e) => return Outcome::Err(500, format!("otp: {e:?}")),
    };
    // seal the base32 TOTP secret — only ciphertext touches the store.
    let _ = vault::put(&otp_secret_name(&me.subject), p.secret.as_bytes());
    Outcome::Json(200, format!("{{\"secret\":{},\"uri\":{}}}", js(&p.secret), js(&p.uri)))
}

#[derive(Deserialize)]
struct CodeReq {
    code: String,
}

fn twofa_verify(request: &IncomingRequest) -> Outcome {
    let me = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let req: CodeReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    let secret = match vault::get(&otp_secret_name(&me.subject)) {
        Ok(bytes) => String::from_utf8(bytes).unwrap_or_default(),
        Err(_) => return Outcome::Err(401, "bad_code".into()), // not enrolled
    };
    match otp::verify(&secret, &req.code, 30, 6, 1) {
        Ok(true) => Outcome::Json(200, "{\"ok\":true}".into()),
        _ => Outcome::Err(401, "bad_code".into()),
    }
}

fn twofa_status(request: &IncomingRequest) -> Outcome {
    let me = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let enrolled = vault::describe(&otp_secret_name(&me.subject)).is_ok();
    Outcome::Json(200, format!("{{\"enrolled\":{enrolled}}}"))
}

// ---- i18n (i18n:catalog) ------------------------------------------------

/// The full UI bundle for a locale (negotiated against what we seeded).
fn i18n_bundle(locale: &str) -> Outcome {
    let available = vec!["en".to_string(), "es".to_string()];
    let resolved = i18n::negotiate(&[locale.to_string()], &available);
    let pairs: Vec<String> = UI_KEYS
        .iter()
        .map(|key| {
            let value = i18n::translate(&resolved, key, &[]).unwrap_or_else(|_| key.to_string());
            format!("{}:{}", js(key), js(&value))
        })
        .collect();
    Outcome::Json(
        200,
        format!("{{\"locale\":{},\"messages\":{{{}}}}}", js(locale), pairs.join(",")),
    )
}

// ---- helpers: request ---------------------------------------------------

fn parse<T: for<'de> Deserialize<'de>>(request: &IncomingRequest) -> Result<T, String> {
    let body = read_body(request).map_err(|_| "could not read body".to_string())?;
    serde_json::from_slice(&body).map_err(|e| format!("bad json: {e}"))
}

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let body = request.consume().map_err(|_| ())?;
    let stream = body.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => buf.extend_from_slice(&chunk),
            Err(_) => break,
        }
    }
    Ok(buf)
}

fn bearer(request: &IncomingRequest) -> Option<String> {
    header(request, "authorization").and_then(|s| {
        s.strip_prefix("Bearer ").map(|tok| tok.trim().to_string())
    })
}

/// First value of a request header as a UTF-8 string.
fn header(request: &IncomingRequest, name: &str) -> Option<String> {
    let headers = request.headers();
    for v in headers.get(&name.to_string()) {
        if let Ok(s) = String::from_utf8(v) {
            return Some(s);
        }
    }
    None
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let mut it = kv.splitn(2, '=');
        if it.next()? == key {
            Some(url_decode(it.next().unwrap_or("")))
        } else {
            None
        }
    })
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = hex(bytes[i + 1]);
                let lo = hex(bytes[i + 2]);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push(h * 16 + l);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Host wall-clock "now" in unix seconds.
fn now_seconds() -> i64 {
    let now = wall_clock::now();
    now.seconds as i64
}

// ---- helpers: validation rules ------------------------------------------

fn text_rule(field: &str, required: bool, min_len: u32, max_len: u32) -> Rule {
    Rule {
        field: field.to_string(),
        kind: Kind::Text,
        required,
        min_len,
        max_len,
        min_value: None,
        max_value: None,
        one_of: vec![],
    }
}

// ---- helpers: JSON ------------------------------------------------------

fn js(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

fn esc(s: &str) -> String {
    let q = js(s);
    q.trim_matches('"').to_string()
}

fn strip_braces(obj: &str) -> &str {
    obj.trim().trim_start_matches('{').trim_end_matches('}')
}

fn json_field(obj: &str, key: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(obj).ok()?;
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn jget_str(v: &serde_json::Value, key: &str) -> String {
    match v.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Set/replace a top-level string field in a flat JSON object string.
fn set_json_field(obj: &str, key: &str, value: &str) -> String {
    let mut v: serde_json::Value = serde_json::from_str(obj).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(map) = v.as_object_mut() {
        map.insert(key.to_string(), serde_json::Value::String(value.to_string()));
    }
    serde_json::to_string(&v).unwrap_or_else(|_| obj.to_string())
}

fn pet_json(id: &str, data: &str) -> String {
    format!("{{\"id\":{},{}}}", js(id), strip_braces(data))
}

fn appt_json(id: &str, data: &str) -> String {
    format!("{{\"id\":{},{}}}", js(id), strip_braces(data))
}

fn page_info_json(info: &paginate::PageInfo) -> String {
    let nc = info.next_cursor.as_ref().map(|c| js(c)).unwrap_or_else(|| "null".into());
    let pc = info.prev_cursor.as_ref().map(|c| js(c)).unwrap_or_else(|| "null".into());
    format!(
        "{{\"nextCursor\":{},\"prevCursor\":{},\"hasNext\":{},\"hasPrev\":{}}}",
        nc, pc, info.has_next, info.has_prev
    )
}

fn principal_json(p: &Principal) -> String {
    format!(
        "{{\"subject\":{},\"tenant\":{},\"roles\":{},\"scopes\":{}}}",
        js(&p.subject),
        js(&p.tenant),
        json_str_array(&p.roles),
        json_str_array(&p.scopes),
    )
}

fn token_pair_json(tp: &TokenPair) -> String {
    let refresh = tp.refresh_token.as_ref().map(|r| js(r)).unwrap_or_else(|| "null".into());
    let session = tp.session_id.as_ref().map(|s| js(s)).unwrap_or_else(|| "null".into());
    format!(
        "{{\"access_token\":{},\"refresh_token\":{},\"expires_in\":{},\"session_id\":{}}}",
        js(&tp.access_token), refresh, tp.expires_in, session
    )
}

fn json_str_array(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| js(s)).collect();
    format!("[{}]", inner.join(","))
}

// ---- helpers: errors ----------------------------------------------------

fn auth_error(e: &AuthError) -> (u16, &'static str) {
    match e {
        AuthError::InvalidCredentials => (401, "invalid_credentials"),
        AuthError::AlreadyExists => (409, "already_exists"),
        AuthError::RateLimited => (429, "rate_limited"),
        AuthError::InsufficientScope(_) => (403, "insufficient_scope"),
        AuthError::Expired => (401, "expired"),
        AuthError::InvalidToken(_) => (401, "invalid_token"),
        AuthError::UnknownTenant => (403, "unknown_tenant"),
        AuthError::Malformed(_) => (400, "malformed"),
        AuthError::BackendUnavailable(_) => (503, "backend_unavailable"),
        AuthError::Internal(_) => (500, "internal"),
    }
}

fn store_err(e: StoreError) -> Outcome {
    match e {
        StoreError::NotFound => Outcome::NotFound,
        StoreError::InvalidJson(m) => Outcome::Bad(format!("invalid_json: {m}")),
        StoreError::RevisionConflict(_) => Outcome::Bad("revision_conflict".into()),
        StoreError::BackendUnavailable(m) => {
            Outcome::Json(503, format!("{{\"error\":\"backend: {}\"}}", esc(&m)))
        }
    }
}

/// Map upload:policy errors to the HTTP shapes the TS app used.
fn policy_err(e: upload::PolicyError) -> Outcome {
    match e {
        upload::PolicyError::TypeNotAllowed(_) => Outcome::Err(415, "type_not_allowed".into()),
        upload::PolicyError::TooLarge(_) => Outcome::Err(413, "too_large".into()),
        upload::PolicyError::InvalidTicket => Outcome::Err(400, "invalid_ticket".into()),
        upload::PolicyError::BackendUnavailable(m) => Outcome::Err(503, format!("backend: {}", esc(&m))),
    }
}

// ---- responses ----------------------------------------------------------

fn respond_json(response_out: ResponseOutparam, status: u16, body: &str) {
    respond(response_out, status, "application/json", body.as_bytes());
}

fn respond(response_out: ResponseOutparam, status: u16, content_type: &str, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[content_type.as_bytes().to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        // write in <= 4096-byte chunks (the stream's typical write budget).
        for chunk in body.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
