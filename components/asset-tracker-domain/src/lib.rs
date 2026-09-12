//! `asset-tracker` — who has the projector, and who is allowed to say it's back.
//!
//! RBAC (`auth:identity`) answers "may this ROLE checkout/checkin AT ALL".
//! It cannot answer "is this the person who actually has it" — that row-level
//! fact lives in `policy:guard`, evaluated on top of the RBAC check, never
//! instead of it: a staff member with the RBAC permission to check items in
//! still gets a 403 checking in someone ELSE'S loan unless they're admin.
//!
//! Real login (`auth:identity/accounts`), not a fixture token — this is meant
//! to be driven from the SPA in `examples/asset-tracker/dist`, not curl.

#[allow(warnings)]
mod bindings;

use serde::Deserialize;
use serde_json::{json, Value};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::rbac;
use bindings::auth::identity::types::{AuthError, Permission, Principal};
use bindings::audit::log::recorder as audit;
use bindings::audit::log::types::Event;
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::id::generate::generator as ids;
use bindings::policy::guard::guard as policy;
use bindings::policy::guard::guard::Attr;
use bindings::records::store::store as records;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const TENANT: &str = "assettracker";
const ASSETS: &str = "assets";
const CHECKOUTS: &str = "checkouts";
const POLICY_DOMAIN: &str = "assettracker";

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let result = match (&method, seg.as_slice()) {
            (Method::Get, [""]) | (Method::Get, ["api"]) => usage(),
            (Method::Post, ["auth", "register"]) => register(&request),
            (Method::Post, ["auth", "login"]) => login(&request),
            (Method::Get, ["auth", "me"]) => me(&request),
            (Method::Post, ["api", "assets"]) => create_asset(&request),
            (Method::Get, ["api", "assets"]) => list_assets(&request),
            (Method::Delete, ["api", "assets", id]) => delete_asset(&request, id),
            (Method::Post, ["api", "assets", id, "checkout"]) => checkout(&request, id),
            (Method::Post, ["api", "assets", id, "checkin"]) => checkin(&request, id),
            _ => Outcome::NotFound,
        };
        emit(response_out, result);
    }
}

enum Outcome {
    Json(u16, String),
    Auth(AuthError),
    Bad(String),
    Err(u16, String),
    Forbidden(String),
    NotFound,
}

fn usage() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "asset-tracker",
            "auth": "POST /auth/register {email,password,role?:staff|admin}, POST /auth/login, GET /auth/me",
            "assets": "POST /api/assets {name} (admin), GET /api/assets, DELETE /api/assets/{id} (admin)",
            "checkout": "POST /api/assets/{id}/checkout — any staff/admin, must be available",
            "checkin": "POST /api/assets/{id}/checkin — only the holder, or admin",
        })
        .to_string(),
    )
}

// ---- seeding ----------------------------------------------------------------

/// Idempotent: role->permission mappings + the one row-level rule. Gated on a
/// marker record so steady-state requests pay a single `count` read.
fn ensure_seeded() {
    if records::count("meta").map(|n| n > 0).unwrap_or(false) {
        return;
    }
    let _ = rbac::set_role_permissions(
        TENANT,
        "staff",
        &[
            Permission { target: "assets".into(), action: "read".into() },
            Permission { target: "checkouts".into(), action: "write".into() },
        ],
    );
    let _ = rbac::set_role_permissions(
        TENANT,
        "admin",
        &[Permission { target: "*".into(), action: "*".into() }],
    );
    let _ = policy::set_rules(
        POLICY_DOMAIN,
        &[
            // The row-level fact RBAC cannot express: only the person who
            // actually holds the asset may check it back in.
            policy::Rule {
                id: "holder-checks-in".into(),
                action: "checkin".into(),
                effect: policy::Effect::Allow,
                conditions: vec![policy::Condition {
                    left: "resource.holder".into(),
                    op: policy::Op::Eq,
                    right: "principal.subject".into(),
                }],
                priority: 10,
            },
            // Admin overrides — lost badges, an employee who left, etc.
            policy::Rule {
                id: "admin-overrides".into(),
                action: "checkin".into(),
                effect: policy::Effect::Allow,
                conditions: vec![policy::Condition {
                    left: "principal.roles".into(),
                    op: policy::Op::Has,
                    right: "admin".into(),
                }],
                priority: 5,
            },
        ],
    );
    let _ = records::create("meta", &json!({"seeded": true}).to_string(), &[]);
}

// ---- auth --------------------------------------------------------------------

#[derive(Deserialize)]
struct RegisterReq {
    email: String,
    password: String,
    #[serde(default)]
    role: Option<String>,
}

fn register(request: &IncomingRequest) -> Outcome {
    ensure_seeded();
    let req: RegisterReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    let principal = match accounts::register(&req.email, &req.password, TENANT) {
        Ok(p) => p,
        Err(e) => return Outcome::Auth(e),
    };
    let role = match req.role.as_deref() {
        Some("admin") => "admin",
        _ => "staff",
    };
    let _ = rbac::assign_role(&principal.tenant, &principal.subject, role);
    Outcome::Json(201, json!({"subject": principal.subject, "role": role}).to_string())
}

#[derive(Deserialize)]
struct LoginReq {
    email: String,
    password: String,
}

fn login(request: &IncomingRequest) -> Outcome {
    let req: LoginReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    match accounts::login(&req.email, &req.password, TENANT) {
        Ok(tp) => Outcome::Json(200, json!({"access_token": tp.access_token}).to_string()),
        Err(e) => Outcome::Auth(e),
    }
}

fn me(request: &IncomingRequest) -> Outcome {
    match introspect(request) {
        Ok(p) => Outcome::Json(200, json!({"subject": p.subject, "roles": p.roles}).to_string()),
        Err(o) => o,
    }
}

fn introspect(request: &IncomingRequest) -> Result<Principal, Outcome> {
    let Some(token) = bearer(request) else {
        return Err(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())));
    };
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

/// `authorize`, mapped straight onto the request's bearer — the one place a
/// route says which permission it needs.
fn require(request: &IncomingRequest, target: &str, action: &str) -> Result<Principal, Outcome> {
    let Some(token) = bearer(request) else {
        return Err(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())));
    };
    authorizer::authorize(&token, &Permission { target: target.into(), action: action.into() })
        .map_err(Outcome::Auth)
}

// ---- assets -------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateAssetReq {
    name: String,
}

fn create_asset(request: &IncomingRequest) -> Outcome {
    ensure_seeded();
    let p = match require(request, "assets", "write") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let req: CreateAssetReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if req.name.is_empty() || req.name.len() > 200 {
        return Outcome::Bad("name must be 1..200 chars".into());
    }
    let tag = format!("TAG-{}", ids::short_code(6));
    let data = json!({"name": req.name, "tag": tag, "status": "available"});
    let entry = match records::create(ASSETS, &data.to_string(), &["status".to_string()]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    audit_log(&p, "asset:create", &entry.id, "allow");
    Outcome::Json(201, json!({"id": entry.id, "name": req.name, "tag": tag, "status": "available"}).to_string())
}

fn list_assets(request: &IncomingRequest) -> Outcome {
    ensure_seeded();
    if let Err(o) = require(request, "assets", "read") {
        return o;
    }
    let page = match records::list_records(ASSETS, 200, "") {
        Ok(p) => p,
        Err(e) => return store_err(e),
    };
    let open_checkouts = records::find_by(CHECKOUTS, "returned_at", "null").unwrap_or_default();
    let assets: Vec<Value> = page
        .entries
        .iter()
        .filter_map(|e| {
            let mut v: Value = serde_json::from_str(&e.data).ok()?;
            let holder = open_checkouts.iter().find_map(|c| {
                let cv: Value = serde_json::from_str(&c.data).ok()?;
                (cv.get("asset_id")?.as_str()? == e.id)
                    .then(|| cv.get("holder")?.as_str().map(str::to_string))
                    .flatten()
            });
            v["id"] = json!(e.id);
            v["holder"] = json!(holder);
            Some(v)
        })
        .collect();
    Outcome::Json(200, json!({"assets": assets}).to_string())
}

fn delete_asset(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match require(request, "assets", "delete") {
        Ok(p) => p,
        Err(o) => return o,
    };
    if let Err(e) = records::delete(ASSETS, id) {
        return store_err(e);
    }
    audit_log(&p, "asset:delete", id, "allow");
    Outcome::Json(204, Value::Null.to_string())
}

// ---- checkout / checkin --------------------------------------------------------

fn checkout(request: &IncomingRequest, asset_id: &str) -> Outcome {
    let p = match require(request, "checkouts", "write") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let asset = match records::get(ASSETS, asset_id) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    let mut asset_data: Value = serde_json::from_str(&asset.data).unwrap_or(json!({}));
    if asset_data.get("status").and_then(Value::as_str) != Some("available") {
        return Outcome::Err(409, "not_available".into());
    }
    let now = wall_clock::now().seconds;
    let checkout_data = json!({
        "asset_id": asset_id,
        "holder": p.subject,
        "checked_out_at": now,
        "returned_at": Value::Null,
    });
    let entry = match records::create(
        CHECKOUTS,
        &checkout_data.to_string(),
        &["asset_id".to_string(), "returned_at".to_string()],
    ) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    asset_data["status"] = json!("checked_out");
    if let Err(e) = records::update(ASSETS, asset_id, &asset_data.to_string(), asset.revision) {
        return store_err(e);
    }
    audit_log(&p, "asset:checkout", asset_id, "allow");
    Outcome::Json(201, json!({"checkout_id": entry.id, "holder": p.subject}).to_string())
}

fn checkin(request: &IncomingRequest, asset_id: &str) -> Outcome {
    let p = match require(request, "checkouts", "write") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let open = match records::find_by(CHECKOUTS, "asset_id", &format!("\"{asset_id}\"")) {
        Ok(entries) => entries
            .into_iter()
            .find(|e| serde_json::from_str::<Value>(&e.data).ok().and_then(|v| v.get("returned_at").cloned()) == Some(Value::Null)),
        Err(e) => return store_err(e),
    };
    let Some(entry) = open else {
        return Outcome::Err(409, "not_checked_out".into());
    };
    let mut checkout_data: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let holder = checkout_data.get("holder").and_then(Value::as_str).unwrap_or("").to_string();

    let principal_attrs =
        vec![Attr { key: "subject".into(), value: p.subject.clone() }, Attr { key: "roles".into(), value: p.roles.join(",") }];
    let resource_attrs = vec![Attr { key: "holder".into(), value: holder.clone() }];
    if !policy::enforce(POLICY_DOMAIN, "checkin", &principal_attrs, &resource_attrs) {
        audit_log(&p, "asset:checkin", asset_id, "deny");
        return Outcome::Forbidden("only the holder or an admin may check this in".into());
    }

    let now = wall_clock::now().seconds;
    checkout_data["returned_at"] = json!(now);
    if let Err(e) = records::update(CHECKOUTS, &entry.id, &checkout_data.to_string(), entry.revision) {
        return store_err(e);
    }
    let asset = match records::get(ASSETS, asset_id) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    let mut asset_data: Value = serde_json::from_str(&asset.data).unwrap_or(json!({}));
    asset_data["status"] = json!("available");
    if let Err(e) = records::update(ASSETS, asset_id, &asset_data.to_string(), asset.revision) {
        return store_err(e);
    }
    audit_log(&p, "asset:checkin", asset_id, "allow");
    Outcome::Json(200, json!({"asset_id": asset_id, "returned_by": p.subject}).to_string())
}

// ---- plumbing -----------------------------------------------------------------

fn audit_log(p: &Principal, action: &str, target: &str, outcome: &str) {
    let e = Event {
        id: "".into(),
        trace_id: "".into(),
        span_id: "".into(),
        timestamp: 0,
        event: action.into(),
        outcome: outcome.into(),
        tenant: p.tenant.clone(),
        subject: p.subject.clone(),
        detail: target.into(),
    };
    let _ = audit::record_event(&e);
}

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::NotFound,
        records::StoreError::InvalidJson(m) => Outcome::Bad(m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn auth_error(e: &AuthError) -> (u16, &'static str) {
    match e {
        AuthError::InvalidCredentials => (401, "invalid_credentials"),
        AuthError::AlreadyExists => (409, "already_exists"),
        AuthError::RateLimited(_) => (429, "rate_limited"),
        AuthError::InsufficientScope(_) => (403, "insufficient_scope"),
        AuthError::Expired => (401, "expired"),
        AuthError::InvalidToken(_) => (401, "invalid_token"),
        AuthError::UnknownTenant => (403, "unknown_tenant"),
        AuthError::Malformed(_) => (400, "malformed"),
        AuthError::BackendUnavailable(_) => (503, "backend_unavailable"),
        AuthError::Internal(_) => (500, "internal"),
    }
}

fn parse<T: for<'a> Deserialize<'a>>(request: &IncomingRequest) -> Result<T, String> {
    let body = read_body(request).map_err(|_| "could not read body".to_string())?;
    serde_json::from_slice(&body).map_err(|e| format!("bad json: {e}"))
}

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_read_body!(MAX_BODY_BYTES);
guestio::guest_write_all!();
guestio::guest_bearer!();

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond(response_out, code, &[], body.as_bytes()),
        Outcome::Auth(e) => {
            if let AuthError::RateLimited(secs) = e {
                respond(
                    response_out,
                    429,
                    &[("retry-after", &secs.to_string())],
                    format!("{{\"error\":\"rate_limited\",\"retryAfter\":{secs}}}").as_bytes(),
                );
            } else {
                let (code, msg) = auth_error(&e);
                respond(response_out, code, &[], format!("{{\"error\":\"{msg}\"}}").as_bytes());
            }
        }
        Outcome::Bad(msg) => respond(response_out, 400, &[], json!({ "error": msg }).to_string().as_bytes()),
        Outcome::Err(code, msg) => respond(response_out, code, &[], json!({ "error": msg }).to_string().as_bytes()),
        Outcome::Forbidden(msg) => respond(response_out, 403, &[], json!({ "error": msg }).to_string().as_bytes()),
        Outcome::NotFound => respond(response_out, 404, &[], b"{\"error\":\"not_found\"}"),
    }
}

fn respond(response_out: ResponseOutparam, status: u16, extra: &[(&str, &str)], body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    for (k, v) in extra {
        let _ = headers.set(k.as_ref(), &[v.as_bytes().to_vec()]);
    }
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        if let Ok(stream) = out.write() {
            let _ = write_all(&stream, body);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
