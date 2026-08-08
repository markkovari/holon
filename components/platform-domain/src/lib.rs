//! `platform-domain` — the platform control plane (docs/adr/) as ONE composed wasm
//! HTTP component. Exports `wasi:http`; imports only contracts.
//!
//! It owns every decision and none of the infrastructure access. On save it builds
//! a desired-state manifest (`manifest.rs`, pure) and stores it as a revision. It
//! pushes nothing anywhere: the native reconciler polls the current revisions and
//! makes the lattice match them (ADR-0022). This component holds no lattice
//! credential and is not supposed to be able to start code on any node.
//!
//! That split is the same one ADR-0003 drew around the Kubernetes credential — the
//! dangerous capability lives in a small native process that holds no business
//! logic, because this one is what tenants send HTTP to. Only the substrate changed.
//!
//! Config (wasi:config/store):
//!   applier-secret   shared secret the reconciler presents on `/api/internal/*`
//!                    (name kept for compatibility with existing deployments)
//!   ingress-suffix   DNS suffix an app is reachable on, e.g. `apps.local`;
//!                    an app answers to `<app>.<tenant>.<suffix>`

#[allow(warnings)]
mod bindings;
mod manifest;

use serde_json::{json, Map, Value};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::types as auth_types;
use bindings::blob::store::blobstore as blob;
use bindings::policy::guard::guard as policy;
use bindings::quota::meter::meter as quota;
use bindings::records::store::store as records;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::config::store as config;
use bindings::wit::reflect::composer;
use bindings::wit::reflect::inspector;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

use manifest::{HostIface, ManifestInput, Part, Plan, Strategy};

struct Component;

const ACCOUNTS: &str = "tenants";
const CATALOG: &str = "catalog";
const DEPLOYMENTS: &str = "deployments";
const REVISIONS: &str = "revisions";
const BIN: &str = "wasm";
/// Deployments per tenant in slice 1. Enforced through `quota:meter`, whose limit
/// is a caller-supplied parameter — the platform is the entitlement store.
const DEPLOYMENT_BUDGET: u64 = 5;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = split_query(&path);
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage(),
            (Method::Post, ["api", "register"]) => register(&request),
            (Method::Post, ["api", "login"]) => login(&request),
            (Method::Get, ["api", "me"]) => me(&request),

            (Method::Post, ["api", "components"]) => component_add(&request, &query),
            (Method::Get, ["api", "components"]) => components_list(&request),
            (Method::Post, ["api", "components", "publish"]) => component_publish(&request),

            (Method::Post, ["api", "deployments"]) => deployment_create(&request),
            (Method::Get, ["api", "deployments"]) => deployments_list(&request),
            (Method::Get, ["api", "deployments", id]) => deployment_get(&request, id),
            (Method::Post, ["api", "deployments", id, "save"]) => deployment_save(&request, id),
            (Method::Get, ["api", "deployments", id, "manifests"]) => manifests(&request, id),
            (Method::Delete, ["api", "deployments", id]) => deployment_delete(&request, id, &query),

            // The applier polls this to re-apply current revisions (ADR-0004).
            (Method::Get, ["api", "internal", "revisions"]) => internal_revisions(&request),
            // The push path (ADR-0017), reconciled like everything else: the applier
            // asks what needs pushing, fetches the bytes, pushes, then reports back.
            (Method::Get, ["api", "internal", "pending-pushes"]) => internal_pending(&request),
            (Method::Get, ["api", "internal", "artifact"]) => internal_artifact(&request, &query),
            // The seam the push step calls once an artifact is in the registry.
            (Method::Post, ["api", "internal", "pushed"]) => internal_pushed(&request),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    Text(u16, String, String),
    /// A body that is not text — the staged `.wasm` the pusher fetches.
    Bytes(u16, String, Vec<u8>),
    Err(u16, String),
    /// An error with structure. `Err` renders `{"error": "<sentence>"}`, which is
    /// all a human needs; a client that has to DO something with the failure —
    /// highlight a port, offer a component to add — needs the parts named.
    Structured(u16, Value),
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn cfg(key: &str, default: &str) -> String {
    config::get(key).ok().flatten().unwrap_or_else(|| default.to_string())
}

fn usage() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "platform",
            "about": "multi-tenant wasm deployment platform — sign in, build a component graph, pick a strategy, save, and it deploys (docs/adr/)",
            "auth": "POST /api/register {email,password,tenant?}, POST /api/login, GET /api/me",
            "catalog": "POST /api/components?id=NAME (raw .wasm), GET /api/components, POST /api/components/publish {id,visibility}",
            "deployments": "POST /api/deployments {name,nodes,edges,strategy}, POST /api/deployments/{id}/save, GET /api/deployments/{id}/manifests",
            "strategies": ["fused", "linked"],
            "adr": "docs/adr/"
        })
        .to_string(),
    )
}

// ---- identity (ADR-0009) ----------------------------------------------------

/// A tenant slug from the email's local part. Slice 1 is single-tenant and
/// invite-only in spirit; this keeps sign-up usable without inventing an org UI.
fn tenant_of_email(email: &str) -> String {
    let local = email.split('@').next().unwrap_or("tenant");
    let s: String = local
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    s.trim_matches('-').to_string()
}

fn register(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (email, password) = (str_of(&b, "email"), str_of(&b, "password"));
    if email.is_empty() || password.is_empty() {
        return Outcome::Err(422, "email and password required".into());
    }
    let tenant = match b["tenant"].as_str().filter(|s| !s.is_empty()) {
        Some(t) => t.to_string(),
        None => tenant_of_email(&email),
    };
    match accounts::register(&email, &password, &tenant) {
        Ok(p) => {
            // Record the tenant so the platform can scaffold its namespace and
            // hold its plan. First account in a tenant owns it.
            if find_one(ACCOUNTS, "tenant", &tenant).is_none() {
                let doc = json!({
                    "tenant": tenant, "owner": p.subject, "created": now(),
                    "plan": { "replicas": 1, "pool_size": 8, "max_invocations": 200,
                              "max_deployments": DEPLOYMENT_BUDGET, "egress": [],
                              "storage": "1Gi", "host_cpu": "100m", "host_memory": "256Mi" },
                    "namespace_applied": false
                });
                let _ = records::create(ACCOUNTS, &doc.to_string(), &["tenant".to_string()]);
            }
            Outcome::Json(201, json!({ "subject": p.subject, "tenant": p.tenant }).to_string())
        }
        Err(e) => auth_err(e),
    }
}

fn login(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (email, password) = (str_of(&b, "email"), str_of(&b, "password"));
    let tenant = match b["tenant"].as_str().filter(|s| !s.is_empty()) {
        Some(t) => t.to_string(),
        None => tenant_of_email(&email),
    };
    match accounts::login(&email, &password, &tenant) {
        Ok(tp) => Outcome::Json(
            200,
            json!({ "token": tp.access_token, "expires_in": tp.expires_in, "tenant": tenant })
                .to_string(),
        ),
        Err(e) => auth_err(e),
    }
}

/// The verified caller. Roles come from the RBAC store, never from the token.
fn caller(request: &IncomingRequest) -> Option<auth_types::Principal> {
    let raw = request.headers().get(&"authorization".to_string());
    let header = String::from_utf8(raw.into_iter().next()?).ok()?;
    let token = header
        .strip_prefix("bearer ")
        .or_else(|| header.strip_prefix("Bearer "))?
        .trim();
    authorizer::introspect(token).ok()
}

fn me(request: &IncomingRequest) -> Outcome {
    match caller(request) {
        Some(p) => Outcome::Json(
            200,
            json!({ "subject": p.subject, "tenant": p.tenant, "roles": p.roles }).to_string(),
        ),
        None => Outcome::Err(401, "no session".into()),
    }
}

fn auth_err(e: auth_types::AuthError) -> Outcome {
    use auth_types::AuthError as E;
    let (code, msg) = match e {
        E::InvalidCredentials => (401, "invalid credentials".to_string()),
        E::AlreadyExists => (409, "already exists".to_string()),
        E::Malformed(m) => (422, m),
        E::Expired => (401, "expired".to_string()),
        E::InsufficientScope(perm) => {
            (403, format!("missing permission {}:{}", perm.target, perm.action))
        }
        E::UnknownTenant => (403, "unknown tenant".to_string()),
        E::BackendUnavailable(m) => (503, m),
        E::RateLimited(secs) => (429, format!("rate limited, retry in {secs}s")),
        E::InvalidToken(m) => (401, m),
        E::Internal(m) => (500, m),
    };
    Outcome::Err(code, msg)
}

// ---- catalog (ADR-0006 / ADR-0007) -----------------------------------------

fn visibility_of(v: &Value) -> &str {
    v["visibility"].as_str().unwrap_or("private")
}

/// Can this principal reference this catalog row? A `policy:guard` decision, not a
/// hand-rolled conditional (ADR-0007/0009).
fn may_use(p: &auth_types::Principal, row: &Value) -> bool {
    let attrs = |pairs: &[(&str, &str)]| -> Vec<policy::Attr> {
        pairs
            .iter()
            .map(|(k, v)| policy::Attr { key: (*k).to_string(), value: (*v).to_string() })
            .collect()
    };
    let principal = attrs(&[("tenant", &p.tenant), ("subject", &p.subject)]);
    let resource = attrs(&[
        ("tenant", row["tenant"].as_str().unwrap_or_default()),
        ("visibility", visibility_of(row)),
    ]);
    // Default is deny; the rule set is seeded at first use.
    policy::enforce("catalog", "use", &principal, &resource)
}

/// The rules that implement ADR-0007's visibility table. Registered idempotently.
fn seed_policy() {
    let cond = |left: &str, op: policy::Op, right: &str| policy::Condition {
        left: left.to_string(),
        op,
        right: right.to_string(),
    };
    let rules = vec![
        // your own tenant's rows, whatever their visibility
        policy::Rule {
            id: "catalog-own".into(),
            action: "use".into(),
            effect: policy::Effect::Allow,
            priority: 10,
            conditions: vec![cond("principal.tenant", policy::Op::Eq, "resource.tenant")],
        },
        // anyone may use a public row
        policy::Rule {
            id: "catalog-public".into(),
            action: "use".into(),
            effect: policy::Effect::Allow,
            priority: 5,
            conditions: vec![cond("resource.visibility", policy::Op::Eq, "public")],
        },
    ];
    let _ = policy::set_rules("catalog", &rules);
}

fn component_add(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let bytes = match read_body(request) {
        Ok(b) if !b.is_empty() => b,
        Ok(_) => return Outcome::Err(422, "empty body — POST the raw .wasm".into()),
        Err(_) => return Outcome::Err(400, "could not read body".into()),
    };
    // Reflection IS validation (ADR-0006): a truncated upload or a core module is
    // refused here instead of becoming a broken catalog row.
    let surface = match inspector::inspect(&bytes) {
        Ok(s) => s,
        Err(e) => {
            return Outcome::Err(
                422,
                match e {
                    inspector::ReflectError::NotAComponent(m) => format!("not a component: {m}"),
                    inspector::ReflectError::BadWasm(m) => format!("bad wasm: {m}"),
                },
            )
        }
    };
    let id = query
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| Some(surface.name.clone()).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| format!("component-{}", surface.sha256));
    let key = format!("{}/{}", p.tenant, id);

    if blob::put(BIN, &key, &bytes, "application/wasm").is_err() {
        return Outcome::Err(500, "could not stage the component bytes".into());
    }
    let doc = json!({
        "key": key, "id": id, "tenant": p.tenant, "uploader": p.subject,
        "visibility": "private", "uploaded": now(),
        // The OCI reference. Empty until the push step records a digest — and a
        // deployment cannot render without one (ADR-0006).
        "oci_ref": "",
        "surface": surface_json(&surface),
    });
    let existing = find_one(CATALOG, "key", &key);
    let ok = match existing {
        Some((rec, rev, _)) => records::update(CATALOG, &rec, &doc.to_string(), rev).is_ok(),
        None => records::create(
            CATALOG,
            &doc.to_string(),
            &["key".to_string(), "tenant".to_string(), "visibility".to_string()],
        )
        .is_ok(),
    };
    if !ok {
        return Outcome::Err(500, "could not store the catalog row".into());
    }
    Outcome::Json(201, doc.to_string())
}

fn components_list(request: &IncomingRequest) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    seed_policy();
    let rows: Vec<Value> = records::list_records(CATALOG, 500, "")
        .map(|page| page.entries)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .filter(|row| may_use(&p, row))
        .map(|row| {
            json!({
                "id": row["id"], "tenant": row["tenant"], "visibility": row["visibility"],
                "uploaded": row["uploaded"], "digest": row["digest"],
                "deployable": row["digest"].as_str().unwrap_or("").starts_with("sha256:"),
                "surface": row["surface"],
            })
        })
        .collect();
    Outcome::Json(200, json!({ "components": rows }).to_string())
}

fn component_publish(request: &IncomingRequest) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let visibility = str_of(&b, "visibility");
    if !matches!(visibility.as_str(), "private" | "org" | "public") {
        return Outcome::Err(422, "visibility must be private|org|public".into());
    }
    // ADR-0007: public requires a signature, and signing does not exist yet. An
    // honest refusal beats an unsigned public catalog.
    if visibility == "public" {
        return Outcome::Err(
            501,
            "public requires a signed digest — signing is not implemented (ADR-0007); private and org work".into(),
        );
    }
    let key = format!("{}/{}", p.tenant, str_of(&b, "id"));
    match find_one(CATALOG, "key", &key) {
        Some((rec, rev, mut row)) if row["tenant"] == json!(p.tenant) => {
            row["visibility"] = json!(visibility);
            match records::update(CATALOG, &rec, &row.to_string(), rev) {
                Ok(_) => Outcome::Json(200, row.to_string()),
                Err(_) => Outcome::Err(409, "revision conflict — retry".into()),
            }
        }
        _ => Outcome::Err(404, "not_found".into()),
    }
}

/// Record that an artifact reached the registry. This is the seam the push step
/// calls; until it is called, the row is not deployable (ADR-0006).
/// Components with no `oci_ref` yet — the work queue for the pusher.
///
/// Derived state, not a queue with its own lifecycle: "needs pushing" IS "has no
/// registry reference", so a lost or duplicated message cannot desynchronise
/// anything, and a re-push after the registry loses a blob needs no bookkeeping.
///
/// The exports and imports travel with it because the OCI config blob `wkg` writes
/// contains them, and the pusher must produce a byte-identical shape or the operator
/// gets an artifact it cannot read. They are already known from upload-time
/// reflection, so the pusher never has to parse the wasm.
fn internal_pending(request: &IncomingRequest) -> Outcome {
    if !internal_ok(request) {
        return Outcome::Err(401, "internal endpoint".into());
    }
    let raws = |v: &Value| -> Vec<Value> {
        v.as_array()
            .map(|a| a.iter().filter_map(|r| r["raw"].as_str().map(|s| json!(s))).collect())
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for e in records::list_records(CATALOG, 1000, "").map(|p| p.entries).unwrap_or_default() {
        let Ok(row) = serde_json::from_str::<Value>(&e.data) else { continue };
        if !row["digest"].as_str().unwrap_or_default().is_empty() {
            continue;
        }
        out.push(json!({
            "key": row["key"],
            "repo": row["key"],
            "sha256": row["surface"]["sha256"],
            "size_bytes": row["surface"]["size_bytes"],
            "exports": raws(&row["surface"]["exports"]),
            "imports": raws(&row["surface"]["imports"]),
        }));
    }
    Outcome::Json(200, json!({ "pending": out }).to_string())
}

/// The staged bytes for one component, so the pusher can send them to the registry.
///
/// A pull, not a push: the wasm side never streams megabytes outward (it has one
/// awkward outgoing-body handshake and no reason to exercise it), and it matches the
/// direction the applier already polls in.
fn internal_artifact(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    if !internal_ok(request) {
        return Outcome::Err(401, "internal endpoint".into());
    }
    let key = query.get("key").and_then(|v| v.as_str()).unwrap_or_default();
    if key.is_empty() {
        return Outcome::Err(422, "?key=<tenant>/<id> required".into());
    }
    match blob::get(BIN, key) {
        Ok(bytes) => Outcome::Bytes(200, "application/wasm".into(), bytes),
        Err(_) => Outcome::Err(404, "no staged bytes for that key".into()),
    }
}

fn internal_pushed(request: &IncomingRequest) -> Outcome {
    if !internal_ok(request) {
        return Outcome::Err(401, "internal endpoint".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (key, digest) = (str_of(&b, "key"), str_of(&b, "digest"));
    if !digest.starts_with("sha256:") {
        return Outcome::Err(422, "digest must be sha256:...".into());
    }
    match find_one(CATALOG, "key", &key) {
        Some((rec, rev, mut row)) => {
            // The bare content address, with no registry host in front of it
            // (ADR-0024). A node fetches by digest from the object store, so a
            // reference that named a registry would name something no node can
            // reach — and would make the same bytes have two identities.
            row["digest"] = json!(digest);
            let _ = records::update(CATALOG, &rec, &row.to_string(), rev);
            Outcome::Json(200, json!({ "key": key, "digest": row["digest"] }).to_string())
        }
        None => Outcome::Err(404, "not_found".into()),
    }
}

fn internal_ok(request: &IncomingRequest) -> bool {
    let want = cfg("applier-secret", "");
    if want.is_empty() {
        return false;
    }
    request
        .headers()
        .get(&"x-platform-secret".to_string())
        .into_iter()
        .next()
        .and_then(|v| String::from_utf8(v).ok())
        .map(|got| got == want)
        .unwrap_or(false)
}

// ---- deployments ------------------------------------------------------------

fn deployment_create(request: &IncomingRequest) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let name = str_of(&b, "name");
    let strategy = match Strategy::parse(b["strategy"].as_str().unwrap_or("fused")) {
        Some(s) => s,
        None => return Outcome::Err(422, "strategy must be fused|linked".into()),
    };
    if name.is_empty() {
        return Outcome::Err(422, "name required".into());
    }
    // Per-tenant deployment budget. `quota:meter`'s limit is a parameter, so the
    // platform is the entitlement store (ADR-0008).
    let budget = plan_of(&p.tenant).max_deployments;
    if let Err(quota::QuotaError::Exceeded(remaining)) =
        quota::reserve(&format!("deployments/{}", p.tenant), 1, budget, 0)
    {
        return Outcome::Err(
            402,
            format!("deployment budget reached ({budget}); {remaining} remaining"),
        );
    }
    let doc = json!({
        "tenant": p.tenant, "name": name, "owner": p.subject,
        "strategy": strategy.as_str(),
        "nodes": b["nodes"].clone(), "edges": b["edges"].clone(),
        "created": now(), "revision": 0, "status": "draft",
    });
    match records::create(DEPLOYMENTS, &doc.to_string(), &["tenant".to_string()]) {
        Ok(rec) => Outcome::Json(201, json!({ "id": rec.id, "name": name }).to_string()),
        Err(_) => Outcome::Err(500, "could not create".into()),
    }
}

struct TenantPlan {
    plan: Plan,
    max_deployments: u64,
}

impl std::ops::Deref for TenantPlan {
    type Target = Plan;
    fn deref(&self) -> &Plan {
        &self.plan
    }
}

fn plan_of(tenant: &str) -> TenantPlan {
    let row = find_one(ACCOUNTS, "tenant", tenant).map(|(_, _, v)| v).unwrap_or_else(|| json!({}));
    let p = &row["plan"];
    TenantPlan {
        plan: Plan {
            replicas: p["replicas"].as_u64().unwrap_or(1) as u32,
            pool_size: p["pool_size"].as_u64().unwrap_or(8) as u32,
            max_invocations: p["max_invocations"].as_u64().unwrap_or(200) as u32,
            egress: p["egress"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default(),
            // An application is no longer a pod, so a plan no longer prices one.
            // What it prices instead is WHERE the app may run: node labels the
            // reconciler matches, which is the multicloud/multiregion knob.
            constraints: p["constraints"]
                .as_object()
                .map(|o| {
                    o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect()
                })
                .unwrap_or_default(),
        },
        max_deployments: p["max_deployments"].as_u64().unwrap_or(DEPLOYMENT_BUDGET),
    }
}

fn owned_deployment(p: &auth_types::Principal, id: &str) -> Option<(String, u64, Value)> {
    let (rec, rev, doc) = records::get(DEPLOYMENTS, id)
        .ok()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok().map(|v| (e.id, e.revision, v)))?;
    (doc["tenant"] == json!(p.tenant)).then_some((rec, rev, doc))
}

fn deployments_list(request: &IncomingRequest) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let rows: Vec<Value> = records::find_by(DEPLOYMENTS, "tenant", &json!(p.tenant).to_string())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| {
            serde_json::from_str::<Value>(&e.data).ok().map(|v| {
                json!({ "id": e.id, "name": v["name"], "strategy": v["strategy"],
                        "revision": v["revision"], "status": v["status"] })
            })
        })
        .collect();
    Outcome::Json(200, json!({ "deployments": rows }).to_string())
}

fn deployment_get(request: &IncomingRequest, id: &str) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    match owned_deployment(&p, id) {
        Some((_, _, mut doc)) => {
            doc["id"] = json!(id);
            Outcome::Json(200, doc.to_string())
        }
        None => Outcome::Err(404, "not_found".into()),
    }
}

/// Resolve a deployment's nodes into renderable parts: check visibility, require a
/// digest, and (for `fused`) compose the graph into one artifact first.
fn resolve_parts(
    p: &auth_types::Principal,
    doc: &Value,
    strategy: Strategy,
) -> Result<Vec<Part>, Outcome> {
    seed_policy();
    let node_ids: Vec<String> = doc["nodes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from).or_else(|| v["id"].as_str().map(String::from)))
                .collect()
        })
        .unwrap_or_default();
    if node_ids.is_empty() {
        return Err(Outcome::Err(422, "the graph has no components".into()));
    }

    let mut rows = Vec::new();
    for id in &node_ids {
        // Own row first, then any row this principal may use (public/org).
        let row = find_one(CATALOG, "key", &format!("{}/{}", p.tenant, id))
            .map(|(_, _, v)| v)
            .or_else(|| {
                records::list_records(CATALOG, 500, "")
                    .map(|page| page.entries)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
                    .find(|r| r["id"] == json!(id) && may_use(p, r))
            });
        let Some(row) = row else {
            return Err(Outcome::Err(422, format!("component `{id}` is unknown or not visible to you")));
        };
        rows.push(row);
    }

    let edges: Vec<composer::Edge> = doc["edges"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| composer::Edge {
                    plug: e["plug"].as_str().unwrap_or_default().to_string(),
                    socket: e["socket"].as_str().unwrap_or_default().to_string(),
                    iface: e["iface"].as_str().unwrap_or_default().to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    // The plan validates the strategy: a graph that cannot support the choice is
    // refused with the reason, not silently switched (ADR-0005).
    let nodes: Vec<composer::Node> = rows
        .iter()
        .map(|r| composer::Node {
            id: r["id"].as_str().unwrap_or_default().to_string(),
            surface: surface_from(&r["surface"]),
        })
        .collect();
    let plan = composer::plan(&nodes, &edges);
    if let Some(problem) = plan.problems.iter().find(|pr| pr.kind == "cycle") {
        if strategy == Strategy::Fused {
            return Err(Outcome::Err(422, format!("fused: {}", problem.detail)));
        }
    }
    if strategy == Strategy::Fused && plan.over_instance_limit {
        return Err(Outcome::Err(
            422,
            format!(
                "fused: ~{} nested instances exceeds wasmtime's limit — deploy linked, or keep the stateful capabilities out of the fuse",
                plan.instance_count
            ),
        ));
    }
    if !plan.unsatisfied.is_empty() {
        // Every gap, and for each one the components that would fill it.
        //
        // This used to return `unsatisfied[0]` as a sentence. The difference
        // matters: one string lets a UI say "wire it", a gap with candidates lets
        // it say "add this". The candidates cost one pass over rows the catalog
        // listing already loads.
        let visible: Vec<Value> = records::list_records(CATALOG, 500, "")
            .map(|page| page.entries)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
            .filter(|r| may_use(p, r))
            .collect();
        let gaps: Vec<Value> = plan
            .unsatisfied
            .iter()
            .map(|g| {
                let candidates: Vec<Value> = visible
                    .iter()
                    .filter(|r| {
                        r["surface"]["exports"]
                            .as_array()
                            .map(|a| a.iter().any(|x| x["raw"] == json!(g.iface.raw)))
                            .unwrap_or(false)
                    })
                    .filter_map(|r| r["id"].as_str().map(|s| json!(s)))
                    .collect();
                json!({ "component": g.node, "interface": g.iface.raw, "candidates": candidates })
            })
            .collect();
        return Err(Outcome::Structured(
            422,
            json!({ "error": "unsatisfied_imports", "gaps": gaps }),
        ));
    }

    let part_of = |row: &Value| -> Result<Part, Outcome> {
        let surface = surface_from(&row["surface"]);
        let digest = row["digest"].as_str().unwrap_or_default().to_string();
        if !digest.starts_with("sha256:") {
            return Err(Outcome::Err(
                409,
                format!(
                    "component `{}` has not been distributed yet — it has no content address, and a deployment can only name bytes by digest (ADR-0006)",
                    row["id"].as_str().unwrap_or_default()
                ),
            ));
        }
        Ok(Part {
            name: row["id"].as_str().unwrap_or_default().to_string(),
            digest,
            host_imports: surface
                .host_imports
                .iter()
                .map(|h| HostIface {
                    namespace: h.namespace.clone(),
                    pkg: h.pkg.clone(),
                    iface: h.name.clone(),
                })
                .collect(),
            nested_instances: surface.nested_instances,
            serves_http: surface.exports.iter().any(|e| e.name == "incoming-handler"),
        })
    };

    match strategy {
        Strategy::Linked => rows.iter().map(part_of).collect(),
        Strategy::Fused => {
            // The fused artifact is one component: the root, with everything else
            // composed into it. Its host imports are the union, and it is the root
            // that must already be in the registry as a composed artifact.
            let root = plan
                .roots
                .first()
                .cloned()
                .ok_or_else(|| Outcome::Err(422, "no root: something is plugged into every component".into()))?;
            let root_row = rows
                .iter()
                .find(|r| r["id"] == json!(root))
                .ok_or_else(|| Outcome::Err(500, "root vanished".into()))?;
            // COMPOSE, for real.
            //
            // This branch used to pin the root's OWN digest and return — `compose`
            // was imported and never called, so a "fused" deployment shipped the
            // root alone with its imports unsatisfied. Nothing caught it because
            // the manifest looked identical either way.
            let mut cparts = Vec::new();
            for row in &rows {
                let id = row["id"].as_str().unwrap_or_default().to_string();
                let key = format!("{}/{}", row["tenant"].as_str().unwrap_or_default(), id);
                let Ok(bytes) = blob::get(BIN, &key) else {
                    return Err(Outcome::Err(
                        409,
                        format!("component `{id}` has no staged bytes to compose from — re-upload it"),
                    ));
                };
                cparts.push(composer::Part { id, bytes });
            }
            let fused = composer::compose(&cparts, &edges, &root).map_err(|e| {
                Outcome::Err(422, format!("fused: {}", compose_detail(&e)))
            })?;

            // The composed artifact is a new component with a new identity, staged
            // like any other. The EXISTING pending-push queue then distributes it
            // with no new machinery: "pending" is still just "has no digest".
            let fused_id = format!("{root}-fused");
            let key = format!("{}/{}", p.tenant, fused_id);
            if blob::put(BIN, &key, &fused, "application/wasm").is_err() {
                return Err(Outcome::Err(500, "could not stage the composed artifact".into()));
            }

            let mut part = Part {
                name: fused_id.clone(),
                digest: String::new(),
                host_imports: Vec::new(),
                nested_instances: 0,
                serves_http: true,
            };
            // The composed artifact needs every host interface in the graph.
            let mut host: Vec<HostIface> = Vec::new();
            for h in &plan.host_needs {
                let entry = HostIface {
                    namespace: h.namespace.clone(),
                    pkg: h.pkg.clone(),
                    iface: h.name.clone(),
                };
                if !host.contains(&entry) {
                    host.push(entry);
                }
            }
            part.host_imports = host;
            part.nested_instances = plan.instance_count;
            // The composed artifact is a component like any other: a catalog row
            // with staged bytes and no digest. The EXISTING distribution queue then
            // picks it up with no new machinery at all, because "pending" is still
            // derived from "has no digest" rather than from a queue someone has to
            // keep in step.
            part.digest = stage_fused(&p.tenant, &fused_id, &key, root_row, &plan);
            if !part.digest.starts_with("sha256:") {
                return Err(Outcome::Err(
                    409,
                    format!(
                        "`{fused_id}` was composed and staged, but has not been distributed yet — save again in a moment"
                    ),
                ));
            }
            Ok(vec![part])
        }
    }
}

/// A readable sentence for a `compose` failure.
fn compose_detail(e: &composer::ComposeError) -> String {
    match e {
        composer::ComposeError::MissingPart(s) => format!("a component has no bytes to compose: {s}"),
        composer::ComposeError::Unbuildable(s) => format!("the graph cannot be composed statically: {s}"),
        composer::ComposeError::PlugFailed(s) => format!("wac refused the plug: {s}"),
        composer::ComposeError::EncodeFailed(s) => format!("the composed graph could not be encoded: {s}"),
    }
}

/// Record the composed artifact as a catalog row, and return its digest once the
/// distributor has given it one.
///
/// Deliberately NOT hashed here. The component has no sha2, and it does not need
/// one: the distributor content-addresses the bytes it actually fetched, which is a
/// stronger check than a hash computed by the same process that wrote them.
/// Omitting `surface.sha256` also skips the distributor's prefix check, which
/// exists to catch a mangled transfer of a hash the catalog already knew.
fn stage_fused(
    tenant: &str,
    fused_id: &str,
    key: &str,
    root_row: &Value,
    plan: &composer::CompositionPlan,
) -> String {
    // The fused surface is the root's exports (composition preserves them) over the
    // graph's host needs (composition unions them).
    let host_imports: Vec<Value> = plan
        .host_needs
        .iter()
        .map(|h| json!({ "raw": format!("{}:{}/{}", h.namespace, h.pkg, h.name),
                         "namespace": h.namespace, "pkg": h.pkg, "name": h.name }))
        .collect();
    let surface = json!({
        "exports": root_row["surface"]["exports"],
        "imports": host_imports,
        "host_imports": host_imports,
        "nested_instances": plan.instance_count,
    });
    match find_one(CATALOG, "key", key) {
        Some((rec, rev, mut row)) => {
            let digest = row["digest"].as_str().unwrap_or_default().to_string();
            // Re-staging the same graph must not orphan a digest that already
            // describes different bytes.
            if row["surface"] != surface {
                row["surface"] = surface;
                row["digest"] = json!("");
                let _ = records::update(CATALOG, &rec, &row.to_string(), rev);
                return String::new();
            }
            digest
        }
        None => {
            let row = json!({
                "key": key, "id": fused_id, "tenant": tenant,
                "uploader": root_row["uploader"], "visibility": "private",
                "description": "composed artifact, generated by a fused deployment",
                "surface": surface, "digest": "", "generated": true, "added": now(),
            });
            let _ = records::create(CATALOG, &row.to_string(),
                &["key".to_string(), "id".to_string(), "tenant".to_string()]);
            String::new()
        }
    }
}

fn deployment_save(request: &IncomingRequest, id: &str) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let Some((rec, rev, mut doc)) = owned_deployment(&p, id) else {
        return Outcome::Err(404, "not_found".into());
    };
    // A save may also update the graph in the same request.
    if let Ok(b) = body(request) {
        for key in ["nodes", "edges", "strategy"] {
            if !b[key].is_null() {
                doc[key] = b[key].clone();
            }
        }
    }
    let strategy = match Strategy::parse(doc["strategy"].as_str().unwrap_or("fused")) {
        Some(s) => s,
        None => return Outcome::Err(422, "strategy must be fused|linked".into()),
    };
    let parts = match resolve_parts(&p, &doc, strategy) {
        Ok(parts) => parts,
        Err(o) => return o,
    };
    let tenant_plan = plan_of(&p.tenant);
    let name = doc["name"].as_str().unwrap_or("app").to_string();
    let suffix = cfg("ingress-suffix", "apps.local");
    let ingress_host = format!("{}.{}.{}", manifest::dns_label(&name), manifest::dns_label(&p.tenant), suffix);

    let edges: Vec<(String, String, String)> = doc["edges"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| {
                    (
                        e["plug"].as_str().unwrap_or_default().to_string(),
                        e["socket"].as_str().unwrap_or_default().to_string(),
                        e["iface"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let root = parts
        .iter()
        .find(|p| p.serves_http)
        .or_else(|| parts.first())
        .map(|p| p.name.clone())
        .unwrap_or_default();

    let doc_manifest = match manifest::build(&ManifestInput {
        tenant: &p.tenant,
        name: &name,
        strategy,
        parts: &parts,
        plan: &tenant_plan,
        edges: &edges,
        root: &root,
        ingress_host: &ingress_host,
    }) {
        Ok(m) => m,
        Err(e) => return Outcome::Err(422, e.detail()),
    };

    // A revision is the unit of rollback: the desired state, verbatim (ADR-0004).
    let next = doc["revision"].as_u64().unwrap_or(0) + 1;
    let revision_doc = json!({
        "deployment": id, "tenant": p.tenant, "revision": next,
        "strategy": strategy.as_str(), "manifest": doc_manifest,
        "saved": now(), "env": manifest::env_for(&p.tenant, &name),
    });
    let _ = records::create(REVISIONS, &revision_doc.to_string(), &["deployment".to_string(), "tenant".to_string()]);

    doc["revision"] = json!(next);
    // There is nothing to apply. The reconciler polls the current revision and
    // makes the lattice match it, so a save is complete when it is stored — which
    // also means a save can no longer half-succeed against a cluster that was
    // reachable a moment ago (ADR-0022).
    doc["status"] = json!("saved");
    doc["last_saved"] = json!(now());
    let _ = records::update(DEPLOYMENTS, &rec, &doc.to_string(), rev);

    Outcome::Json(
        200,
        json!({ "id": id, "revision": next, "strategy": strategy.as_str(),
                "app": name, "ingress": ingress_host,
                "components": doc_manifest["components"].as_array().map(|a| a.len()).unwrap_or(0) })
        .to_string(),
    )
}

fn manifests(request: &IncomingRequest, id: &str) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    if owned_deployment(&p, id).is_none() {
        return Outcome::Err(404, "not_found".into());
    }
    let latest = records::find_by(REVISIONS, "deployment", &json!(id).to_string())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .max_by_key(|v| v["revision"].as_u64().unwrap_or(0));
    match latest {
        // The desired-state document itself, not a rendered string. It used to be
        // YAML for a human to `kubectl apply`; nothing applies it now, so it is
        // returned as the object the reconciler actually consumes.
        Some(v) => Outcome::Structured(
            200,
            json!({ "revision": v["revision"], "saved": v["saved"], "manifest": v["manifest"] }),
        ),
        None => Outcome::Err(404, "no revision yet — deploy first".into()),
    }
}

/// What the applier re-applies on its interval (ADR-0004's drift correction).
fn internal_revisions(request: &IncomingRequest) -> Outcome {
    if !internal_ok(request) {
        return Outcome::Err(401, "internal endpoint".into());
    }
    let mut current: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    for e in records::list_records(REVISIONS, 1000, "").map(|p| p.entries).unwrap_or_default() {
        if let Ok(v) = serde_json::from_str::<Value>(&e.data) {
            let key = v["deployment"].as_str().unwrap_or_default().to_string();
            let better = current
                .get(&key)
                .map(|old| v["revision"].as_u64().unwrap_or(0) > old["revision"].as_u64().unwrap_or(0))
                .unwrap_or(true);
            if better {
                current.insert(key, v);
            }
        }
    }
    Outcome::Json(
        200,
        json!({ "revisions": current.into_values().collect::<Vec<_>>() }).to_string(),
    )
}


/// Delete a deployment: drop its records, and the lattice follows.
///
/// This used to prune a Kubernetes footprint first, because the platform forgetting
/// an app that was still running left an orphan nothing would ever clean up. That
/// whole hazard is gone: the reconciler derives desired state from these records
/// every pass, so an app that leaves them stops on its own within a pass or two.
/// ADR-0016's two-signals-before-reaping apparatus goes with it.
fn deployment_delete(
    request: &IncomingRequest,
    id: &str,
    query: &Map<String, Value>,
) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "unauthorized".into());
    };
    let Some((rec, _rev, doc)) = owned_deployment(&p, id) else {
        return Outcome::Err(404, "not_found".into());
    };
    let name = doc["name"].as_str().unwrap_or("app").to_string();

    // This destroys the app's storage claim, and nothing here can bring it back
    // (ADR-0016). So the caller has to name what they are deleting: `428 Precondition
    // Required` is exactly "the request must be made conditional", and an accidental
    // or replayed DELETE cannot satisfy it.
    if query.get("confirm").and_then(|v| v.as_str()) != Some(name.as_str()) {
        return Outcome::Err(
            428,
            format!(
                "deleting `{name}` also destroys its storage, permanently. Re-send with ?confirm={name} to mean it."
            ),
        );
    }
    let env = manifest::env_for(&p.tenant, &name);

    for e in records::list_records(REVISIONS, 1000, "").map(|pg| pg.entries).unwrap_or_default() {
        if let Ok(v) = serde_json::from_str::<Value>(&e.data) {
            if v["deployment"] == json!(id) {
                let _ = records::delete(REVISIONS, &e.id);
            }
        }
    }
    let _ = records::delete(DEPLOYMENTS, &rec);
    Outcome::Json(
        200,
        json!({ "deleted": id, "env": env,
                "note": "the lattice stops it on the next reconcile pass" })
        .to_string(),
    )
}

// ---- the applier hop: removed ---------------------------------------------
//
// `apply_via_applier` / `prune_via_applier` / `post_to_applier` used to live here.
// They are gone with Kubernetes: the platform no longer pushes anything anywhere.
// It stores desired state and the reconciler pulls it (ADR-0022), which is why a
// save can no longer half-succeed and why deleting an app needs no prune call.

fn surface_json(s: &inspector::Surface) -> Value {
    let refs = |list: &Vec<inspector::IfaceRef>| -> Vec<Value> {
        list.iter()
            .map(|r| json!({ "raw": r.raw, "namespace": r.namespace, "pkg": r.pkg, "name": r.name, "version": r.version }))
            .collect()
    };
    json!({
        "name": s.name, "exports": refs(&s.exports), "imports": refs(&s.imports),
        "host_imports": refs(&s.host_imports), "size_bytes": s.size_bytes,
        "sha256": s.sha256, "nested_instances": s.nested_instances,
    })
}

fn iface_from(v: &Value) -> inspector::IfaceRef {
    inspector::IfaceRef {
        raw: v["raw"].as_str().unwrap_or_default().to_string(),
        namespace: v["namespace"].as_str().unwrap_or_default().to_string(),
        pkg: v["pkg"].as_str().unwrap_or_default().to_string(),
        name: v["name"].as_str().unwrap_or_default().to_string(),
        version: v["version"].as_str().unwrap_or_default().to_string(),
    }
}

fn surface_from(v: &Value) -> inspector::Surface {
    let list = |k: &str| -> Vec<inspector::IfaceRef> {
        v[k].as_array().map(|a| a.iter().map(iface_from).collect()).unwrap_or_default()
    };
    inspector::Surface {
        name: v["name"].as_str().unwrap_or_default().to_string(),
        exports: list("exports"),
        imports: list("imports"),
        host_imports: list("host_imports"),
        size_bytes: v["size_bytes"].as_u64().unwrap_or(0),
        sha256: v["sha256"].as_str().unwrap_or_default().to_string(),
        nested_instances: v["nested_instances"].as_u64().unwrap_or(0) as u32,
    }
}

fn find_one(coll: &str, field: &str, value: &str) -> Option<(String, u64, Value)> {
    records::find_by(coll, field, &json!(value).to_string())
        .ok()?
        .into_iter()
        .next()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok().map(|v| (e.id, e.revision, v)))
}

fn str_of(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().trim().to_string()
}

fn split_query(path: &str) -> (String, Map<String, Value>) {
    let mut parts = path.splitn(2, '?');
    let route = parts.next().unwrap_or("/").to_string();
    let mut q = Map::new();
    if let Some(raw) = parts.next() {
        for pair in raw.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                q.insert(k.to_string(), json!(v));
            }
        }
    }
    (route, q)
}

fn body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let raw = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let b = request.consume().map_err(|_| ())?;
    let stream = b.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(65536) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => buf.extend_from_slice(&chunk),
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(_) => return Err(()),
        }
    }
    Ok(buf)
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    let (code, ctype, body) = match result {
        Outcome::Json(c, b) => (c, "application/json".to_string(), b.into_bytes()),
        Outcome::Text(c, ct, b) => (c, ct, b.into_bytes()),
        Outcome::Bytes(c, ct, b) => (c, ct, b),
        Outcome::Err(c, m) => (
            c,
            "application/json".to_string(),
            json!({ "error": m }).to_string().into_bytes(),
        ),
        Outcome::Structured(c, v) => (c, "application/json".to_string(), v.to_string().into_bytes()),
    };
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[ctype.into_bytes()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(code);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in body.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
