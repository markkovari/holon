//! portal:app — developer portal / API-key service over composed contracts.
//!
//! Two-layer authorization by design: RBAC (auth:identity) gates role-level
//! verbs (admin drain), policy:guard ABAC gates per-project access via
//! owner/member attribute rules. API keys are stored as sha256 hashes and
//! shown in full exactly once, at mint time; the gateway verifies a key with
//! one indexed lookup and meters it against a per-key hourly quota.

#[allow(warnings)]
mod bindings;

use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::rbac;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Permission, Principal};
use bindings::id::generate::generator as ids;
use bindings::notify::dispatch::dispatcher as notify;
use bindings::outbox::dispatch::queue as outbox;
use bindings::policy::guard::guard as policy;
use bindings::quota::meter::meter as quota;
use bindings::records::store::store as records;
use bindings::webhook::sign::signer as sign;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const TENANT: &str = "portal";
const PROJECTS: &str = "projects";
const KEYS: &str = "apikeys";
const POLICY_DOMAIN: &str = "project";
const DEFAULT_KEY_LIMIT: u64 = 100; // requests per hour
const QUOTA_PERIOD: u64 = 3600;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let result = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage_json(),
            (Method::Post, ["auth", "register"]) => register(&request),
            (Method::Post, ["auth", "login"]) => login(&request),
            (Method::Get, ["auth", "me"]) => me(&request),
            (Method::Post, ["auth", "logout"]) => logout(&request),

            (Method::Post, ["api", "projects"]) => create_project(&request),
            (Method::Get, ["api", "projects"]) => list_projects(&request),
            (Method::Get, ["api", "projects", id]) => get_project(&request, id),
            (Method::Post, ["api", "projects", id, "members"]) => add_member(&request, id),
            (Method::Post, ["api", "projects", id, "webhook"]) => set_webhook(&request, id),
            (Method::Post, ["api", "projects", id, "keys"]) => mint_key(&request, id),
            (Method::Get, ["api", "projects", id, "keys"]) => list_keys(&request, id),
            (Method::Delete, ["api", "keys", id]) => revoke_key(&request, id),
            (Method::Get, ["api", "keys", id, "usage"]) => key_usage(&request, id),

            (Method::Post, ["api", "gateway", "echo"]) => gateway_echo(&request),
            (Method::Post, ["api", "admin", "drain"]) => admin_drain(&request),
            _ => Outcome::NotFound,
        };
        emit(response_out, result);
    }
}

enum Outcome {
    Json(u16, String),
    Auth(AuthError),
    /// 429 with a Retry-After of the payload seconds (quota exhausted).
    Limited(u64),
    Bad(String),
    Err(u16, String),
    Forbidden(String),
    NotFound,
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "dev-portal",
            "auth": "POST /auth/register|login, GET /auth/me",
            "projects": "POST|GET /api/projects, GET /api/projects/{id}",
            "keys": "POST|GET /api/projects/{id}/keys, DELETE /api/keys/{id}",
            "gateway": "POST /api/gateway/echo (x-api-key)",
            "admin": "POST /api/admin/drain"
        })
        .to_string(),
    )
}

// ---- seeding ---------------------------------------------------------------

/// Idempotent: ABAC rules for the project domain + RBAC perms for admin.
/// Gated on one record so steady-state requests pay a single count() read.
fn ensure_seeded() {
    if records::count("meta").map(|n| n > 0).unwrap_or(false) {
        return;
    }
    let owner_cond = policy::Condition {
        left: "resource.owner".into(),
        op: policy::Op::Eq,
        right: "principal.subject".into(),
    };
    let member_cond = policy::Condition {
        left: "resource.members".into(),
        op: policy::Op::Has,
        right: "principal.subject".into(),
    };
    let mut rules = Vec::new();
    // owners can do everything; members can read + use keys.
    for action in ["project.read", "project.write", "project.admin"] {
        rules.push(policy::Rule {
            id: format!("owner-{action}"),
            action: action.into(),
            effect: policy::Effect::Allow,
            conditions: vec![owner_cond.clone()],
            priority: 10,
        });
    }
    for action in ["project.read", "project.write"] {
        rules.push(policy::Rule {
            id: format!("member-{action}"),
            action: action.into(),
            effect: policy::Effect::Allow,
            conditions: vec![member_cond.clone()],
            priority: 20,
        });
    }
    let _ = policy::set_rules(POLICY_DOMAIN, &rules);
    let _ = rbac::set_role_permissions(
        TENANT,
        "admin",
        &[Permission { target: "portal".into(), action: "admin".into() }],
    );
    let _ = records::create("meta", "{\"seeded\":true}", &[]);
}

// ---- auth ------------------------------------------------------------------

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
    let wanted = req.role.unwrap_or_else(|| "developer".into());
    let role =
        if ["developer", "admin"].contains(&wanted.as_str()) { wanted } else { "developer".into() };
    let _ = rbac::assign_role(&principal.tenant, &principal.subject, &role);
    Outcome::Json(201, json!({"subject": principal.subject, "roles": [role]}).to_string())
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
        Ok(tp) => Outcome::Json(
            200,
            json!({
                "access_token": tp.access_token,
                "refresh_token": tp.refresh_token,
                "expires_in": tp.expires_in,
                "session_id": tp.session_id,
            })
            .to_string(),
        ),
        Err(e) => Outcome::Auth(e),
    }
}

fn me(request: &IncomingRequest) -> Outcome {
    match introspect(request) {
        Ok(p) => Outcome::Json(
            200,
            json!({"subject": p.subject, "tenant": p.tenant, "roles": p.roles}).to_string(),
        ),
        Err(o) => o,
    }
}

fn logout(request: &IncomingRequest) -> Outcome {
    let Some(token) = bearer(request) else {
        return Outcome::Auth(AuthError::InvalidToken("missing bearer".into()));
    };
    match session::revoke(&token) {
        Ok(()) => Outcome::Json(204, String::new()),
        Err(e) => Outcome::Auth(e),
    }
}

fn introspect(request: &IncomingRequest) -> Result<Principal, Outcome> {
    let Some(token) = bearer(request) else {
        return Err(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())));
    };
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

// ---- ABAC helper -------------------------------------------------------------

/// Load the project and enforce `action` for the principal via policy:guard.
fn authorize_project(
    p: &Principal,
    project_id: &str,
    action: &str,
) -> Result<records::Entry, Outcome> {
    let entry = match records::get(PROJECTS, project_id) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Err(Outcome::NotFound),
        Err(e) => return Err(store_err(e)),
    };
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    let principal_attrs = vec![policy::Attr { key: "subject".into(), value: p.subject.clone() }];
    let target_attrs = vec![
        policy::Attr { key: "owner".into(), value: data["owner"].as_str().unwrap_or("").into() },
        policy::Attr {
            key: "members".into(),
            value: data["members"].as_str().unwrap_or("").into(),
        },
    ];
    if policy::enforce(POLICY_DOMAIN, action, &principal_attrs, &target_attrs) {
        Ok(entry)
    } else {
        Err(Outcome::Forbidden(format!("{action} denied")))
    }
}

// ---- projects ----------------------------------------------------------------

fn create_project(request: &IncomingRequest) -> Outcome {
    ensure_seeded();
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    #[derive(Deserialize)]
    struct Req {
        name: String,
    }
    let req: Req = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if req.name.is_empty() || req.name.len() > 100 {
        return Outcome::Bad("name must be 1..100 chars".into());
    }
    // members is a comma-joined string so policy:guard's `has` op can test it.
    let data = json!({
        "name": req.name,
        "owner": p.subject,
        "members": "",
        "webhook_url": "",
        "webhook_secret": "",
    });
    match records::create(PROJECTS, &data.to_string(), &["owner".to_string()]) {
        Ok(e) => Outcome::Json(201, project_json(&e).to_string()),
        Err(e) => store_err(e),
    }
}

fn list_projects(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    match records::find_by(PROJECTS, "owner", &json!(p.subject).to_string()) {
        Ok(entries) => {
            let projects: Vec<Value> = entries.iter().map(project_json).collect();
            Outcome::Json(200, json!({ "projects": projects }).to_string())
        }
        Err(e) => store_err(e),
    }
}

fn get_project(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    match authorize_project(&p, id, "project.read") {
        Ok(e) => Outcome::Json(200, project_json(&e).to_string()),
        Err(o) => o,
    }
}

fn add_member(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    #[derive(Deserialize)]
    struct Req {
        subject: String,
    }
    let req: Req = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    let entry = match authorize_project(&p, id, "project.admin") {
        Ok(e) => e,
        Err(o) => return o,
    };
    let mut data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    let members = data["members"].as_str().unwrap_or("");
    if !members.split(',').any(|m| m == req.subject) {
        let joined = if members.is_empty() {
            req.subject.clone()
        } else {
            format!("{members},{}", req.subject)
        };
        data["members"] = json!(joined);
    }
    match records::update(PROJECTS, id, &data.to_string(), entry.revision) {
        Ok(e) => Outcome::Json(200, project_json(&e).to_string()),
        Err(e) => store_err(e),
    }
}

fn set_webhook(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    #[derive(Deserialize)]
    struct Req {
        url: String,
        secret: String,
    }
    let req: Req = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if !(req.url.starts_with("http://") || req.url.starts_with("https://")) {
        return Outcome::Bad("url must be http(s)".into());
    }
    let entry = match authorize_project(&p, id, "project.admin") {
        Ok(e) => e,
        Err(o) => return o,
    };
    let mut data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    data["webhook_url"] = json!(req.url);
    data["webhook_secret"] = json!(req.secret);
    match records::update(PROJECTS, id, &data.to_string(), entry.revision) {
        Ok(e) => Outcome::Json(200, project_json(&e).to_string()),
        Err(e) => store_err(e),
    }
}

// ---- api keys ------------------------------------------------------------------

fn mint_key(request: &IncomingRequest, project_id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    #[derive(Deserialize)]
    struct Req {
        name: String,
        #[serde(default)]
        limit: Option<u64>,
    }
    let req: Req = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if let Err(o) = authorize_project(&p, project_id, "project.write") {
        return o;
    }
    let key = format!("dk_{}", ids::nanoid(32));
    let data = json!({
        "project": project_id,
        "name": req.name,
        "prefix": &key[..10],
        "hash": hash_key(&key),
        "limit": req.limit.unwrap_or(DEFAULT_KEY_LIMIT),
        "revoked": false,
    });
    let entry = match records::create(
        KEYS,
        &data.to_string(),
        &["hash".to_string(), "project".to_string()],
    ) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    enqueue_event("key.created", project_id, &entry.id, &key[..10]);
    // the ONLY response that ever contains the full key.
    Outcome::Json(
        201,
        json!({
            "id": entry.id,
            "key": key,
            "name": req.name,
            "limit": req.limit.unwrap_or(DEFAULT_KEY_LIMIT),
            "note": "store this key now — it is not retrievable again",
        })
        .to_string(),
    )
}

fn list_keys(request: &IncomingRequest, project_id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if let Err(o) = authorize_project(&p, project_id, "project.read") {
        return o;
    }
    match records::find_by(KEYS, "project", &json!(project_id).to_string()) {
        Ok(entries) => {
            let keys: Vec<Value> = entries.iter().map(key_json).collect();
            Outcome::Json(200, json!({ "keys": keys }).to_string())
        }
        Err(e) => store_err(e),
    }
}

fn revoke_key(request: &IncomingRequest, key_id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let entry = match records::get(KEYS, key_id) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Outcome::NotFound,
        Err(e) => return store_err(e),
    };
    let mut data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    let project_id = data["project"].as_str().unwrap_or("").to_string();
    if let Err(o) = authorize_project(&p, &project_id, "project.admin") {
        return o;
    }
    data["revoked"] = json!(true);
    if let Err(e) = records::update(KEYS, key_id, &data.to_string(), entry.revision) {
        return store_err(e);
    }
    enqueue_event("key.revoked", &project_id, key_id, data["prefix"].as_str().unwrap_or(""));
    Outcome::Json(200, "{\"revoked\":true}".into())
}

fn key_usage(request: &IncomingRequest, key_id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let entry = match records::get(KEYS, key_id) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Outcome::NotFound,
        Err(e) => return store_err(e),
    };
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    let project_id = data["project"].as_str().unwrap_or("");
    if let Err(o) = authorize_project(&p, project_id, "project.read") {
        return o;
    }
    let limit = data["limit"].as_u64().unwrap_or(DEFAULT_KEY_LIMIT);
    match quota::peek(&format!("key:{key_id}"), limit, QUOTA_PERIOD) {
        Ok(b) => Outcome::Json(
            200,
            json!({"used": b.used, "limit": b.limit, "remaining": b.remaining, "resets_at": b.resets_at})
                .to_string(),
        ),
        // peek never consumes, so exceeded shouldn't occur; belt-and-braces.
        Err(quota::QuotaError::Exceeded(_)) => Outcome::Limited(0),
        Err(quota::QuotaError::BackendUnavailable(m)) => Outcome::Err(503, m),
    }
}

// ---- gateway (the metered data plane) ------------------------------------------

fn gateway_echo(request: &IncomingRequest) -> Outcome {
    let Some(key) = header(request, "x-api-key") else {
        return Outcome::Err(401, "missing x-api-key".into());
    };
    let hits = match records::find_by(KEYS, "hash", &json!(hash_key(&key)).to_string()) {
        Ok(h) => h,
        Err(e) => return store_err(e),
    };
    let Some(entry) = hits.first() else {
        return Outcome::Err(401, "unknown api key".into());
    };
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    if data["revoked"].as_bool().unwrap_or(false) {
        return Outcome::Err(401, "key revoked".into());
    }
    let limit = data["limit"].as_u64().unwrap_or(DEFAULT_KEY_LIMIT);
    // reserve = check-and-consume; record-usage only meters and never rejects.
    let subject = format!("key:{}", entry.id);
    let balance = match quota::reserve(&subject, 1, limit, QUOTA_PERIOD) {
        Ok(b) => b,
        Err(quota::QuotaError::Exceeded(_)) => {
            // exceeded carries remaining (0 here); peek for the window reset.
            let resets =
                quota::peek(&subject, limit, QUOTA_PERIOD).map(|b| b.resets_at).unwrap_or(0);
            return Outcome::Limited(resets);
        }
        Err(quota::QuotaError::BackendUnavailable(m)) => return Outcome::Err(503, m),
    };
    let body = read_body(request).unwrap_or_default();
    Outcome::Json(
        200,
        json!({
            "echo": String::from_utf8_lossy(&body),
            "project": data["project"],
            "remaining": balance.remaining,
            "resets_at": balance.resets_at,
        })
        .to_string(),
    )
}

// ---- outbox + signed webhook delivery -------------------------------------------

fn enqueue_event(topic: &str, project_id: &str, key_id: &str, prefix: &str) {
    // durable intent; delivery happens on drain. A lost enqueue is logged by
    // its absence at drain time, not worth failing the mutation over.
    let payload = json!({"project": project_id, "key": key_id, "prefix": prefix}).to_string();
    let _ = outbox::enqueue(topic, payload.as_bytes(), 0);
}

/// Deliver pending events as signed webhooks. RBAC-gated: admin role only —
/// wasip2 has no background tasks, so drain is an explicit admin verb (the
/// same explicit-pump pattern as cache flush + vet's run-reminders).
fn admin_drain(request: &IncomingRequest) -> Outcome {
    ensure_seeded();
    let Some(token) = bearer(request) else {
        return Outcome::Auth(AuthError::InvalidToken("missing bearer".into()));
    };
    let perm = Permission { target: "portal".into(), action: "admin".into() };
    if let Err(e) = authorizer::authorize(&token, &perm) {
        return Outcome::Auth(e);
    }

    let events = match outbox::claim(25, 60) {
        Ok(evs) => evs,
        Err(e) => return Outcome::Err(503, format!("outbox: {e:?}")),
    };
    let (mut delivered, mut dropped, mut failed) = (0u32, 0u32, 0u32);
    for ev in &events {
        let payload: Value = serde_json::from_slice(&ev.payload).unwrap_or(Value::Null);
        let project = payload["project"].as_str().unwrap_or("");
        let (url, secret) = match records::get(PROJECTS, project) {
            Ok(e) => {
                let d: Value = serde_json::from_str(&e.data).unwrap_or(Value::Null);
                (
                    d["webhook_url"].as_str().unwrap_or("").to_string(),
                    d["webhook_secret"].as_str().unwrap_or("").to_string(),
                )
            }
            Err(_) => (String::new(), String::new()),
        };
        if url.is_empty() {
            // no endpoint configured -> drop the event, don't retry forever.
            let _ = outbox::ack(&ev.id);
            dropped += 1;
            continue;
        }
        let body = json!({"id": ev.id, "topic": ev.topic, "data": payload}).to_string();
        // stripe-scheme HMAC; the signature rides inside the envelope because
        // notify:dispatch's message has no header field.
        let envelope = match sign::sign(body.as_bytes(), &secret, sign::Scheme::Stripe) {
            Ok(s) => json!({"payload": body, "signature": s.header, "timestamp": s.timestamp})
                .to_string(),
            Err(e) => {
                let _ = outbox::fail(&ev.id);
                failed += 1;
                let _ = e;
                continue;
            }
        };
        let msg = notify::Message {
            channel: notify::Channel::Webhook,
            target: url,
            subject: ev.topic.clone(),
            body: envelope,
        };
        match notify::send(&msg) {
            Ok(status) if (200..300).contains(&status) => {
                let _ = outbox::ack(&ev.id);
                delivered += 1;
            }
            _ => {
                let _ = outbox::fail(&ev.id);
                failed += 1;
            }
        }
    }
    Outcome::Json(
        200,
        json!({"claimed": events.len(), "delivered": delivered, "dropped": dropped, "failed": failed})
            .to_string(),
    )
}

// ---- helpers ---------------------------------------------------------------------

fn hash_key(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn project_json(entry: &records::Entry) -> Value {
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    json!({
        "id": entry.id,
        "name": data["name"],
        "owner": data["owner"],
        "members": data["members"].as_str().unwrap_or("").split(',').filter(|s| !s.is_empty()).collect::<Vec<_>>(),
        "webhook_url": data["webhook_url"],
        "created": entry.created,
    })
}

fn key_json(entry: &records::Entry) -> Value {
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    json!({
        "id": entry.id,
        "name": data["name"],
        "prefix": data["prefix"],
        "limit": data["limit"],
        "revoked": data["revoked"],
        "created": entry.created,
    })
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

/// The most a request body may be, before the component stops reading it.
///
/// There was no ceiling anywhere: when this was measured, 148 of the tree's 150
/// components accumulated whatever arrived until the guest hit wasmtime's 64 MiB
/// per-store memory cap and TRAPPED, which reaches the caller as a closed
/// connection saying nothing about a size.
/// A component that answers JSON has no business reading sixteen megabytes, and
/// the ones that legitimately handle uploads police it themselves with a 413 and a
/// granted max-size — those are left alone.
///
/// Generous on purpose. This is a backstop against an unbounded read, not a
/// content policy; an API that needs a real limit should state its own and say 413.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_read_body!(MAX_BODY_BYTES);
guestio::guest_write_all!();

guestio::guest_bearer!();

fn header(request: &IncomingRequest, name: &str) -> Option<String> {
    request.headers().get(name).into_iter().find_map(|v| String::from_utf8(v).ok())
}

// ---- responses --------------------------------------------------------------------

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
        // resetsAt is absolute unix seconds (no clock import here to derive a
        // relative Retry-After from it).
        Outcome::Limited(resets_at) => respond(
            response_out,
            429,
            &[],
            format!("{{\"error\":\"quota_exceeded\",\"resetsAt\":{resets_at}}}").as_bytes(),
        ),
        Outcome::Bad(msg) => {
            respond(response_out, 400, &[], json!({ "error": msg }).to_string().as_bytes())
        }
        Outcome::Err(code, msg) => {
            respond(response_out, code, &[], json!({ "error": msg }).to_string().as_bytes())
        }
        Outcome::Forbidden(msg) => {
            respond(response_out, 403, &[], json!({ "error": msg }).to_string().as_bytes())
        }
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
        let stream = out.write().expect("write stream");
        let _ = write_all(&stream, body);
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
