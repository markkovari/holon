//! `vet-domain` — the vet-clinic domain as a WIT HTTP component.
//!
//! The pet/appointment/note orchestration that used to be TypeScript glue under
//! jco, here a `wasi:http/incoming-handler` component that owns NO storage and
//! NO auth logic — it maps HTTP onto the capability contracts it imports:
//!   auth:identity (accounts/session/authorizer/rbac), records:store,
//!   validate:schema, search:index.
//!
//! Every mutating route authorizes the bearer token first (authorizer.authorize
//! with the required {target, action}); bodies are validated by validate:schema;
//! pets/appointments/notes are records:store collections; pets are indexed in
//! search:index. Owners are scoped to their own pets/appointments; doctors and
//! admins see all. The same .wasm runs under jco or wasmCloud — backends are a
//! compose-time choice, never in this code.
//!
//! Routes (core slice):
//!   POST /register {email,password,role?}     -> 201 principal      (+ assign role)
//!   POST /login    {email,password}           -> 200 token-pair
//!   GET  /me                                  -> 200 principal
//!   GET  /pets[?q=]                            -> 200 {pets:[...]}    (guard pets:read)
//!   POST /pets {name,species,notes?}          -> 201 pet            (guard pets:write)
//!   GET  /appointments                        -> 200 {appointments} (guard appointments:read)
//!   POST /appointments {pet,datetime}         -> 201 appointment    (guard appointments:write)
//!   POST /appointments/{id}/notes {text}      -> 201 note           (guard notes:write)

#[allow(warnings)]
mod bindings;

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

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

// ---- routing ------------------------------------------------------------

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let query = path.splitn(2, '?').nth(1).unwrap_or("").to_string();

        let result = match (&method, route.as_str()) {
            (Method::Post, "/register") => register(&request),
            (Method::Post, "/login") => login(&request),
            (Method::Get, "/me") => me(&request),
            (Method::Get, "/pets") => list_pets(&request, &query),
            (Method::Post, "/pets") => create_pet(&request),
            (Method::Get, "/appointments") => list_appointments(&request),
            (Method::Post, "/appointments") => create_appointment(&request),
            // POST /appointments/{id}/notes
            (Method::Post, r) if r.starts_with("/appointments/") && r.ends_with("/notes") => {
                let id = r.trim_start_matches("/appointments/").trim_end_matches("/notes");
                add_note(&request, id)
            }
            // RBAC seeding — define role→permission maps + assign roles. Used to
            // bootstrap the clinic's roles on deploy. (Unauthenticated here for
            // demo parity with accounts-app; guard or internal-only in prod.)
            (Method::Post, "/admin/role-permissions") => admin_set_role_perms(&request),
            (Method::Post, "/admin/assign-role") => admin_assign_role(&request),
            _ => Outcome::NotFound,
        };

        emit(response_out, result);
    }
}

enum Outcome {
    Json(u16, String),
    Auth(AuthError),
    Bad(String),
    NotFound,
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond(response_out, code, &body),
        Outcome::Auth(e) => {
            let (code, msg) = auth_error(&e);
            respond(response_out, code, &format!("{{\"error\":\"{msg}\"}}"));
        }
        Outcome::Bad(msg) => respond(response_out, 400, &format!("{{\"error\":\"{}\"}}", esc(&msg))),
        Outcome::NotFound => respond(response_out, 404, "{\"error\":\"not_found\"}"),
    }
}

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
    // assign the requested role (default pet-owner); best-effort, non-fatal.
    let role = req.role.unwrap_or_else(|| "pet-owner".to_string());
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
    let token = match bearer(request) {
        Some(t) => t,
        None => return Outcome::Auth(AuthError::InvalidToken("missing bearer".into())),
    };
    match authorizer::introspect(&token) {
        Ok(p) => Outcome::Json(200, principal_json(&p)),
        Err(e) => Outcome::Auth(e),
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

fn is_privileged(p: &Principal) -> bool {
    p.roles.iter().any(|r| r == "admin" || r == "doctor")
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
    vec![
        text_rule("name", true, 1, 60),
        text_rule("species", true, 1, 40),
    ]
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
    // validate the raw JSON against the declarative rules.
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
    // index for full-text search (name + species + notes), tagged by owner.
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
    let q = query_param(query, "q");

    let entries: Vec<(String, String)> = if let Some(term) = q {
        // search path: query the index, optionally tag-scoped to the owner.
        let tags = if owner_scope { vec![format!("owner:{}", principal.subject)] } else { vec![] };
        let hits = search::query(&term, search::Mode::Any, &tags, 50).unwrap_or_default();
        hits.into_iter()
            .filter_map(|h| records::get("pets", &h.id).ok().map(|e| (e.id, e.data)))
            .collect()
    } else if owner_scope {
        records::find_by("pets", "owner", &js(&principal.subject))
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.id, e.data))
            .collect()
    } else {
        records::list_records("pets", 0, "")
            .map(|p| p.entries.into_iter().map(|e| (e.id, e.data)).collect())
            .unwrap_or_default()
    };

    let items: Vec<String> = entries.iter().map(|(id, data)| pet_json(id, data)).collect();
    Outcome::Json(200, format!("{{\"pets\":[{}]}}", items.join(",")))
}

// ---- appointments -------------------------------------------------------

#[derive(Deserialize)]
struct ApptReq {
    pet: String,
    datetime: String,
}

fn appt_rules() -> Vec<Rule> {
    vec![
        text_rule("pet", true, 1, 80),
        text_rule("datetime", true, 4, 40),
    ]
}

fn create_appointment(request: &IncomingRequest) -> Outcome {
    let principal = match require(request, "appointments", "write") {
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
        Err(_) => return Outcome::Bad("pet_not_found".into()),
    };
    let owner = json_field(&pet.data, "owner").unwrap_or(principal.subject.clone());
    let data = format!(
        "{{\"pet\":{},\"owner\":{},\"doctor\":\"\",\"datetime\":{},\"status\":\"booked\"}}",
        js(&req.pet), js(&owner), js(&req.datetime)
    );
    match records::create("appointments", &data, &["owner".to_string(), "doctor".to_string(), "pet".to_string()]) {
        Ok(e) => Outcome::Json(201, appt_json(&e.id, &e.data)),
        Err(e) => store_err(e),
    }
}

fn list_appointments(request: &IncomingRequest) -> Outcome {
    let principal = match require(request, "appointments", "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let entries = if is_privileged(&principal) {
        records::list_records("appointments", 0, "")
            .map(|p| p.entries)
            .unwrap_or_default()
    } else {
        // owners see only their own appointments.
        records::find_by("appointments", "owner", &js(&principal.subject)).unwrap_or_default()
    };
    let items: Vec<String> = entries.iter().map(|e| appt_json(&e.id, &e.data)).collect();
    Outcome::Json(200, format!("{{\"appointments\":[{}]}}", items.join(",")))
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
    // the appointment must exist.
    if records::get("appointments", appt_id).is_err() {
        return Outcome::NotFound;
    }
    let req: NoteReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if req.text.trim().is_empty() {
        return Outcome::Bad("empty_note".into());
    }
    let data = format!(
        "{{\"appointment\":{},\"author\":{},\"text\":{}}}",
        js(appt_id), js(&principal.subject), js(&req.text)
    );
    match records::create("notes", &data, &["appointment".to_string()]) {
        Ok(e) => Outcome::Json(201, format!("{{\"id\":{},{}}}", js(&e.id), strip_braces(&e.data))),
        Err(e) => store_err(e),
    }
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
    let headers = request.headers();
    for v in headers.get(&"authorization".to_string()) {
        if let Ok(s) = String::from_utf8(v) {
            if let Some(tok) = s.strip_prefix("Bearer ") {
                return Some(tok.trim().to_string());
            }
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
    // minimal: '+' -> space, %XX -> byte. Enough for a search term.
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

/// JSON-encode a string value (quotes + escaping) via serde_json.
fn js(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// Escape a string for embedding in an error message (no surrounding quotes).
fn esc(s: &str) -> String {
    let q = js(s);
    q.trim_matches('"').to_string()
}

/// `{...}` -> `...` (drop the outer braces so a record can be merged with an id).
fn strip_braces(obj: &str) -> &str {
    obj.trim().trim_start_matches('{').trim_end_matches('}')
}

/// Pull a top-level string field out of a JSON object string (small, dependency
/// -free; the record bodies are flat objects we wrote ourselves).
fn json_field(obj: &str, key: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(obj).ok()?;
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn pet_json(id: &str, data: &str) -> String {
    format!("{{\"id\":{},{}}}", js(id), strip_braces(data))
}

fn appt_json(id: &str, data: &str) -> String {
    format!("{{\"id\":{},{}}}", js(id), strip_braces(data))
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
        StoreError::BackendUnavailable(m) => Outcome::Json(503, format!("{{\"error\":\"backend: {}\"}}", esc(&m))),
    }
}

// ---- responses ----------------------------------------------------------

fn respond(response_out: ResponseOutparam, status: u16, body: &str) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        let _ = stream.blocking_write_and_flush(body.as_bytes());
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
