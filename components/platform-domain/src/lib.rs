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
mod req;
mod orgs;

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
use bindings::secrets::vault::vault;
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
/// How long a join code is good for. Long enough to hand over, short enough that a
/// leaked one in a chat log is usually already dead.
const INVITE_TTL: u64 = 7 * 24 * 3600;
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

            (Method::Post, ["api", "orgs"]) => org_create(&request),
            (Method::Get, ["api", "orgs"]) => org_list(&request),
            (Method::Post, ["api", "orgs", "join"]) => org_join(&request),
            (Method::Get, ["api", "orgs", org, "members"]) => org_members(&request, org),
            (Method::Post, ["api", "orgs", org, "invites"]) => org_invite(&request, org),
            (Method::Delete, ["api", "orgs", org, "members", subject]) => {
                org_remove(&request, org, subject)
            }

            (Method::Post, ["api", "components"]) => component_add(&request, &query),
            (Method::Get, ["api", "components"]) => components_list(&request),
            (Method::Post, ["api", "components", "publish"]) => component_publish(&request),
            (Method::Post, ["api", "components", "satisfies"]) => components_satisfies(&request),
            (Method::Get, ["api", "market"]) => market_search(&request, &query),

            (Method::Post, ["api", "internal", "fetch-token"]) => fetch_token_mint(&request),
            (Method::Get, ["api", "internal", "secret"]) => secret_fetch(&request, &query),

            (Method::Post, ["api", "secrets"]) => secret_put(&request, &query),
            (Method::Get, ["api", "secrets"]) => secrets_list(&request, &query),
            (Method::Delete, ["api", "secrets", name]) => secret_delete(&request, name, &query),

            (Method::Post, ["api", "deployments"]) => deployment_create(&request, &query),
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

pub(crate) fn now() -> u64 {
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
            "catalog": "POST /api/components?id=NAME&config=key!,key2 (raw .wasm), GET /api/components, POST /api/components/publish {id,visibility}, POST /api/components/satisfies {socket,plug}",
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
                              "constraints": {} },
                });
                let _ = records::create(ACCOUNTS, &doc.to_string(), &["tenant".to_string()]);
            }
            // A solo organisation, so there is never a code path where someone has
            // no org to deploy into and no second shape for "personal" work. If it
            // already exists this account is joining an existing tenant, and the
            // first account there owns it.
            if orgs::role_of(&p.subject, &tenant).is_none() {
                let role = match find_one(orgs::ORGS, "id", &tenant) {
                    None => {
                        let _ = orgs::create(&tenant, &p.subject, &email);
                        orgs::Role::Owner
                    }
                    // An existing org is NOT joined automatically by anyone who
                    // guesses its tenant name — that would make registration an
                    // access-control bypass. They need an invite.
                    Some(_) => orgs::Role::Viewer,
                };
                let _ = role;
            }
            Outcome::Structured(
                201,
                json!({ "subject": p.subject, "tenant": p.tenant,
                        "orgs": orgs::memberships(&p.subject) }),
            )
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
    // A row uploaded before orgs existed has none; it belongs to its uploader's
    // personal org, which is named after the tenant. Guessing anything else would
    // silently widen who can see old rows.
    let row_org = row["org"].as_str().unwrap_or_else(|| row["tenant"].as_str().unwrap_or_default());
    let resource = attrs(&[
        ("tenant", row["tenant"].as_str().unwrap_or_default()),
        ("org", row_org),
        ("visibility", visibility_of(row)),
    ]);
    let decide = |org: &str| {
        let principal = attrs(&[("tenant", &p.tenant), ("subject", &p.subject), ("org", org)]);
        // Default is deny; the rule set is seeded at first use.
        policy::enforce("catalog", "use", &principal, &resource)
    };

    // Own and public need no org, so they are decided once and cheaply.
    if decide("") {
        return true;
    }
    if visibility_of(row) != "org" {
        return false;
    }
    // A person can belong to several organisations (ADR-0031), and `policy:guard`
    // compares single values — so the decision is asked once per membership rather
    // than reshaping the rule engine around a list. Nobody is in enough orgs for
    // this to matter, and the alternative is a hand-rolled conditional next to a
    // policy engine that exists to avoid exactly that.
    orgs::memberships(&p.subject)
        .iter()
        .filter_map(|m| m["id"].as_str().map(String::from))
        .any(|org| org == row_org && decide(&org))
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
        // anyone in the same ORGANISATION may use an org-visible row.
        //
        // ADR-0007's middle row, specified and never built — so `visibility: "org"`
        // was accepted by publish and then did nothing, which is worse than
        // rejecting it: the uploader believes they shared something.
        //
        // Between own (10) and public (5): more specific than "anyone", less than
        // "mine", which is the order the table describes.
        policy::Rule {
            id: "catalog-org".into(),
            action: "use".into(),
            effect: policy::Effect::Allow,
            priority: 7,
            conditions: vec![
                cond("resource.visibility", policy::Op::Eq, "org"),
                cond("principal.org", policy::Op::Eq, "resource.org"),
            ],
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

/// `grace-period-secs!,retries` -> `[{key, required}]`.
fn declared_config(query: &Map<String, Value>) -> Vec<Value> {
    query
        .get("config")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| match s.strip_suffix('!') {
            Some(k) => json!({ "key": k, "required": true }),
            None => json!({ "key": s, "required": false }),
        })
        .collect()
}

/// Check one component's config against the keys its uploader declared.
///
/// Two errors, and both name what to do about it. An unknown key is almost always a
/// typo, so the message lists the legal ones — the difference between "rejected" and
/// "you wrote `grace-period-sec`, it is `grace-period-secs`". A missing required key
/// is named outright, because the alternative is a component that starts and then
/// fails on its first request in front of a user.
///
/// A component that declared nothing accepts nothing: silence means "reads no
/// config", not "reads anything". Deny by omission, as everywhere else here.
fn check_config(id: &str, row: &Value, given: &Map<String, Value>) -> Result<(), String> {
    let declared: Vec<(&str, bool)> = row["config_keys"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|d| {
                    Some((d["key"].as_str()?, d["required"].as_bool().unwrap_or(false)))
                })
                .collect()
        })
        .unwrap_or_default();

    let legal: Vec<&str> = declared.iter().map(|(k, _)| *k).collect();
    for key in given.keys() {
        if !legal.contains(&key.as_str()) {
            return Err(if legal.is_empty() {
                format!("`{id}` declares no config keys, so it cannot take `{key}`")
            } else {
                format!("`{id}` has no config key `{key}` — it takes {legal:?}")
            });
        }
    }
    for (key, required) in &declared {
        if *required && !given.contains_key(*key) {
            return Err(format!("`{id}` requires config `{key}`, which is not set"));
        }
    }
    Ok(())
}

/// Would plugging `plug` into `socket` actually work?
///
/// This is `wac`'s own subtype check on the real bytes, not a name comparison. Two
/// components can both talk about `records:store/store@0.1.0` and still not fit: the
/// version, the record fields, a resource's methods all have to line up. Name
/// matching is what `plan` does at save time, and it is the weaker test — this is the
/// one that decides whether `wac plug` will succeed.
///
/// It belongs at edge-draw time, which is why it is a separate endpoint: the answer
/// is wanted while someone is dragging a line between two boxes, not after they hit
/// deploy. A UI that can only find out at save is a UI that lets you build something
/// invalid and then explains why.
///
/// The reply also carries every interface the plug would satisfy, not just the one
/// asked about — `wac plug` matches EVERY common interface between a plug's exports
/// and the socket's imports and cannot be told to satisfy just one. A UI that draws
/// one edge while three were wired is a UI that lies.
fn components_satisfies(request: &IncomingRequest) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    seed_policy();
    let b: req::Satisfies = match read_body(request)
        .map_err(|_| Outcome::Err(400, "could not read body".into()))
        .and_then(|raw| req::parse(&raw))
    {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (socket_id, plug_id) = (b.socket.trim().to_string(), b.plug.trim().to_string());
    if socket_id.is_empty() || plug_id.is_empty() {
        return Outcome::Err(422, "socket and plug are both required".into());
    }
    if socket_id == plug_id {
        return Outcome::Err(422, "a component cannot plug into itself".into());
    }

    // Same visibility rule as a deploy: own row first, then anything this principal
    // may use. Asking whether two components fit must not become a way to learn that
    // a private component exists.
    let find = |id: &str| -> Option<Value> {
        find_one(CATALOG, "key", &format!("{}/{}", p.tenant, id))
            .map(|(_, _, v)| v)
            .or_else(|| {
                records::list_records(CATALOG, 500, "")
                    .map(|page| page.entries)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
                    .find(|r| r["id"] == json!(id) && may_use(&p, r))
            })
    };
    let bytes_of = |id: &str| -> Result<Vec<u8>, Outcome> {
        let Some(row) = find(id) else {
            return Err(Outcome::Err(422, format!("component `{id}` is unknown or not visible to you")));
        };
        let key = format!("{}/{}", row["tenant"].as_str().unwrap_or_default(), id);
        blob::get(BIN, &key)
            .map_err(|_| Outcome::Err(409, format!("component `{id}` has no staged bytes — re-upload it")))
    };

    let socket = match bytes_of(&socket_id) {
        Ok(b) => b,
        Err(o) => return o,
    };
    let plug = match bytes_of(&plug_id) {
        Ok(b) => b,
        Err(o) => return o,
    };

    match composer::satisfies(&socket, &plug) {
        Ok(ifaces) => Outcome::Json(
            200,
            json!({
                "socket": socket_id,
                "plug": plug_id,
                "fits": !ifaces.is_empty(),
                "satisfies": ifaces,
                // Said out loud because an empty list is the surprising answer, and
                // "the names matched" is exactly what the person drawing the line
                // just checked by eye.
                "detail": if ifaces.is_empty() {
                    format!("`{plug_id}` exports nothing that `{socket_id}` imports — matching interface NAMES is not enough, the types have to fit too")
                } else {
                    format!("`{plug_id}` would satisfy {} import(s) of `{socket_id}`", ifaces.len())
                },
            })
            .to_string(),
        ),
        Err(e) => Outcome::Err(
            422,
            match e {
                inspector::ReflectError::NotAComponent(m) => format!("not a component: {m}"),
                inspector::ReflectError::BadWasm(m) => format!("bad wasm: {m}"),
            },
        ),
    }
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
    // What config this component reads, declared by whoever uploaded it:
    // `?config=grace-period-secs!,retries` — a trailing `!` marks it required.
    //
    // The uploader is the right person to ask: they own the component and can see
    // what it calls `wasi:config` for. Nobody else can know, and a platform that
    // guesses would either reject valid config or accept typos.
    let config_keys = declared_config(query);
    // Which organisation owns this component. `?org=` or the uploader's personal
    // one, and membership is checked here rather than after the row exists.
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Member) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    let doc = json!({
        "key": key, "id": id, "tenant": p.tenant, "uploader": p.subject,
        "org": org,
        "visibility": "private", "uploaded": now(),
        "config_keys": config_keys,
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
                "id": row["id"], "tenant": row["tenant"], "org": row["org"],
                "visibility": row["visibility"],
                "uploaded": row["uploaded"], "digest": row["digest"],
                "deployable": row["digest"].as_str().unwrap_or("").starts_with("sha256:"),
                "surface": row["surface"],
                // What config this component reads, so a caller can render a form
                // and fill it in before deploying instead of discovering the keys
                // from a 422. Null on rows uploaded before declarations existed,
                // which reads correctly as "declared nothing".
                "config_keys": row["config_keys"],
            })
        })
        .collect();
    Outcome::Json(200, json!({ "components": rows }).to_string())
}

/// Everything this caller may use, filtered.
///
/// `?q=` matches the id or description; `?iface=` matches an EXPORT, because that is
/// the question someone actually has — "who can fill this gap in my graph" — and it
/// is the same match the 422 uses to suggest candidates. `?org=` narrows to one
/// organisation.
///
/// Substring and set matching over rows the catalogue listing already loads. No
/// search engine, no index: a catalogue of this size does not need one, and adding
/// one would be inventing an answer to a question nobody has asked yet.
/// ponytail: linear scan; add an index when the catalogue outgrows a page.
/// Secrets are named per ORGANISATION, never globally.
///
/// One vault backs the whole platform, so the org has to be part of the name or two
/// tenants would share a namespace — the same mistake ADR-0012 measured with storage
/// buckets, in a place where the consequence is worse.
fn vault_name(org: &str, name: &str) -> String {
    format!("{org}/{name}")
}

/// `vault://<org>/<name>` — the only form a manifest may contain (ADR-0010).
fn parse_ref(r: &str) -> Option<(String, String)> {
    let rest = r.strip_prefix("vault://")?;
    let (org, name) = rest.split_once('/')?;
    if org.is_empty() || name.is_empty() {
        return None;
    }
    Some((org.to_string(), name.to_string()))
}

/// Store a secret for an org. The value is written straight through to the vault,
/// which seals it before it touches storage — nothing here keeps it, logs it, or puts
/// it in a response.
/// Where fetch tokens live. One row per instance that was granted a secret.
const FETCH_TOKENS: &str = "fetch_tokens";

/// Mint a capability for one instance: exactly these references, for a bounded time.
///
/// Issued BY the platform rather than signed by the reconciler, which is the simpler
/// and stronger arrangement — no shared signing key, and revocation is deleting a
/// row rather than waiting out a signature. The reconciler authenticates with the
/// platform secret it already holds (ADR-0003).
///
/// The token is a capability, not a secret value: it is worth exactly what this
/// manifest was worth, which is why the host may keep it in a ledger on disk
/// (ADR-0022).
fn fetch_token_mint(request: &IncomingRequest) -> Outcome {
    if !internal_ok(request) {
        return Outcome::Err(401, "bad platform secret".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let instance = str_of(&b, "instance");
    let refs: Vec<String> = b["refs"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if instance.is_empty() || refs.is_empty() {
        return Outcome::Err(422, "instance and refs are required".into());
    }
    // Long enough to outlive an instance's useful life, short enough that a leaked
    // token is not a standing grant. A restart mints a new one, and a start costs
    // 0.43ms (ADR-0040), so a short life is cheap here in a way it usually is not.
    let ttl = b["ttl"].as_u64().unwrap_or(3600);
    // The record id is the token: unguessable, unique, and already stored — the same
    // trick the invite codes use (ADR-0031).
    let doc = json!({
        "instance": instance, "refs": refs, "expires": now() + ttl, "issued": now(),
    });
    match records::create(FETCH_TOKENS, &doc.to_string(), &["instance".to_string()]) {
        Ok(rec) => Outcome::Json(201, json!({ "token": rec.id, "expires": now() + ttl }).to_string()),
        Err(_) => Outcome::Err(500, "could not mint a fetch token".into()),
    }
}

/// Resolve one reference for a host holding a valid token.
///
/// The plaintext leaves the platform here and nowhere else. Three checks, in this
/// order, because each is cheaper than the next: does the token exist, has it
/// expired, and does it authorise THIS reference.
fn secret_fetch(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let token = request
        .headers()
        .get(&"x-fetch-token".to_string())
        .into_iter()
        .next()
        .and_then(|v| String::from_utf8(v).ok())
        .unwrap_or_default();
    if token.is_empty() {
        return Outcome::Err(401, "no fetch token".into());
    }
    let reference = query.get("ref").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let Ok(entry) = records::get(FETCH_TOKENS, &token) else {
        return Outcome::Err(401, "unknown fetch token".into());
    };
    let Ok(doc) = serde_json::from_str::<Value>(&entry.data) else {
        return Outcome::Err(401, "unreadable fetch token".into());
    };
    if doc["expires"].as_u64().unwrap_or(0) < now() {
        // 401 so the host can tell "restart me" from "your manifest is wrong".
        let _ = records::delete(FETCH_TOKENS, &token);
        return Outcome::Err(401, "fetch token expired".into());
    }
    let granted = doc["refs"]
        .as_array()
        .map(|a| a.iter().any(|r| r.as_str() == Some(reference.as_str())))
        .unwrap_or(false);
    if !granted {
        // 403, not 404: this token is real and this reference is not on it. Saying
        // so does not leak whether the secret exists, only that this instance was
        // not granted it — which the instance's own manifest already told it.
        return Outcome::Err(403, "this instance was not granted that reference".into());
    }
    let Some((org, name)) = parse_ref(&reference) else {
        return Outcome::Err(422, "not a secret reference".into());
    };
    // `?probe=1` is a host asking "does this resolve", which it does at START for
    // every reference in a manifest (ADR-0051). Identical authorisation — the token
    // checks above are the same ones — answered from `describe`, so no plaintext is
    // read, logged, or put on the wire for a secret nothing has revealed yet.
    if query.get("probe").is_some() {
        return match vault::describe(&vault_name(&org, &name)) {
            Ok(_) => Outcome::Json(200, json!({ "resolves": true }).to_string()),
            Err(vault::VaultError::NotFound) => Outcome::Err(404, "no such secret".into()),
            Err(e) => Outcome::Err(500, vault_detail(&e)),
        };
    }
    match vault::get(&vault_name(&org, &name)) {
        // Bytes, not JSON: a plaintext should not pass through a serialiser that
        // might log or escape it.
        Ok(v) => Outcome::Bytes(200, "application/octet-stream".into(), v),
        Err(vault::VaultError::NotFound) => Outcome::Err(404, "no such secret".into()),
        Err(e) => Outcome::Err(500, vault_detail(&e)),
    }
}

fn secret_put(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    // Writing a secret is not a viewer's job.
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Member) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    let b: req::PutSecret = match read_body(request)
        .map_err(|_| Outcome::Err(400, "could not read body".into()))
        .and_then(|raw| req::parse(&raw))
    {
        Ok(v) => v,
        Err(o) => return o,
    };
    let name = b.name.trim();
    if name.is_empty() || b.value.is_empty() {
        return Outcome::Err(422, "name and value are both required".into());
    }
    match vault::put(&vault_name(&org, name), b.value.as_bytes()) {
        // The reply is metadata, deliberately: a caller that just wrote a secret has
        // the value already, and echoing it back puts it in one more place.
        Ok(meta) => Outcome::Json(
            201,
            json!({
                "ref": format!("vault://{org}/{name}"),
                "name": name, "org": org,
                "version": meta.version, "updated": meta.updated,
            })
            .to_string(),
        ),
        Err(e) => Outcome::Err(500, format!("vault refused the write: {}", vault_detail(&e))),
    }
}

/// Names only. There is no endpoint that returns a value: the platform stores
/// secrets so that workloads can use them, not so that a browser can display them.
fn secrets_list(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Viewer) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    let prefix = format!("{org}/");
    match vault::list_names(500) {
        Ok(names) => {
            let mine: Vec<Value> = names
                .iter()
                .filter_map(|n| n.strip_prefix(&prefix))
                .map(|n| json!({ "name": n, "ref": format!("vault://{org}/{n}") }))
                .collect();
            Outcome::Json(200, json!({ "secrets": mine, "count": mine.len() }).to_string())
        }
        Err(e) => Outcome::Err(500, format!("vault unreadable: {}", vault_detail(&e))),
    }
}

fn secret_delete(request: &IncomingRequest, name: &str, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Member) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    match vault::delete(&vault_name(&org, name)) {
        Ok(()) => Outcome::Json(200, json!({ "deleted": name, "org": org }).to_string()),
        Err(e) => Outcome::Err(500, format!("vault refused the delete: {}", vault_detail(&e))),
    }
}

fn vault_detail(e: &vault::VaultError) -> String {
    match e {
        vault::VaultError::NotFound => "no such secret".into(),
        vault::VaultError::Crypto(m) => format!("crypto: {m}"),
        vault::VaultError::BackendUnavailable(m) => format!("backend unavailable: {m}"),
    }
}

/// Every secret a component asks for must resolve, and must belong to the org
/// deploying it.
///
/// `describe` is the whole reason this is safe: it answers "is there a secret by this
/// name" WITHOUT decrypting, so a save can be validated without the platform ever
/// holding a plaintext it has no use for (ADR-0010).
fn check_secrets(id: &str, org: &str, secrets: &[Value]) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for s in secrets {
        let key = s["key"].as_str().unwrap_or_default().trim();
        let reference = s["ref"].as_str().unwrap_or_default().trim();
        if key.is_empty() || reference.is_empty() {
            return Err(format!("`{id}`: every secret needs a key and a ref"));
        }
        let Some((ref_org, name)) = parse_ref(reference) else {
            return Err(format!(
                "`{id}`: `{reference}` is not a secret reference — it must look like `vault://{org}/<name>`"
            ));
        };
        // Refusing another org's reference is the whole boundary. Without it a
        // manifest could name any secret on the platform and the vault would happily
        // resolve it.
        if ref_org != org {
            return Err(format!(
                "`{id}`: `{reference}` belongs to `{ref_org}`, and this deployment is for `{org}`"
            ));
        }
        if vault::describe(&vault_name(&ref_org, &name)).is_err() {
            return Err(format!(
                "`{id}`: `{reference}` does not resolve — store it first with POST /api/secrets"
            ));
        }
        // BY REFERENCE ONLY. The value is never read here, so it cannot reach a
        // manifest, a revision, or a log line.
        out.push(json!({ "key": key, "ref": reference }));
    }
    Ok(out)
}

fn market_search(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    seed_policy();
    let want = |k: &str| {
        query.get(k).and_then(|v| v.as_str()).unwrap_or_default().trim().to_lowercase()
    };
    let (q, iface, org) = (want("q"), want("iface"), want("org"));

    let rows: Vec<Value> = records::list_records(CATALOG, 500, "")
        .map(|page| page.entries)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        // Visibility first, always. A search that filtered afterwards would let a
        // caller learn a private component exists by how many results came back.
        .filter(|r| may_use(&p, r))
        .filter(|r| {
            if q.is_empty() {
                return true;
            }
            let id = r["id"].as_str().unwrap_or_default().to_lowercase();
            let desc = r["description"].as_str().unwrap_or_default().to_lowercase();
            id.contains(&q) || desc.contains(&q)
        })
        .filter(|r| {
            if iface.is_empty() {
                return true;
            }
            r["surface"]["exports"]
                .as_array()
                .map(|a| {
                    a.iter().any(|x| {
                        x["raw"].as_str().unwrap_or_default().to_lowercase().contains(&iface)
                    })
                })
                .unwrap_or(false)
        })
        .filter(|r| org.is_empty() || r["org"].as_str().unwrap_or_default().to_lowercase() == org)
        .map(|r| {
            json!({
                "id": r["id"], "tenant": r["tenant"], "org": r["org"],
                "visibility": r["visibility"], "description": r["description"],
                "deprecated": r["deprecated"].as_bool().unwrap_or(false),
                "uploaded": r["uploaded"], "digest": r["digest"],
                "deployable": r["digest"].as_str().unwrap_or("").starts_with("sha256:"),
                "config_keys": r["config_keys"],
                // The surface is the point of a marketplace listing: what it exports
                // is what someone is shopping for.
                "surface": r["surface"],
            })
        })
        .collect();
    Outcome::Json(200, json!({ "components": rows, "count": rows.len() }).to_string())
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
            // Optional, and only overwritten when given: a publish that changes
            // visibility should not silently erase a description.
            if let Some(d) = b["description"].as_str() {
                row["description"] = json!(d);
            }
            // ADR-0007: deprecation, never deletion. A component someone deployed
            // must keep resolving, so the strongest thing an author can do is say
            // "do not start anything new with this".
            if let Some(d) = b["deprecated"].as_bool() {
                row["deprecated"] = json!(d);
            }
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


// ---- organisations ----------------------------------------------------------

/// The org a caller acts on behalf of when they name none: their own.
///
/// Created at registration so there is never a code path where someone has no org.
fn personal_org(p: &auth_types::Principal) -> String {
    p.tenant.clone()
}

fn org_create(request: &IncomingRequest) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let name = str_of(&b, "name");
    match orgs::create(&name, &p.subject, &p.subject) {
        Ok(doc) => Outcome::Json(201, doc.to_string()),
        Err(e) => Outcome::Err(422, e),
    }
}

fn org_list(request: &IncomingRequest) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    Outcome::Structured(200, json!({ "orgs": orgs::memberships(&p.subject) }))
}

fn org_invite(request: &IncomingRequest, org: &str) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    // Only an owner may widen who can touch an org's deployments.
    let q: Map<String, Value> = [("org".to_string(), json!(org))].into_iter().collect();
    if let Err((code, msg)) = orgs::acting(&p.subject, &personal_org(&p), &q, orgs::Role::Owner) {
        return Outcome::Err(code, msg);
    }
    let role = body(request)
        .ok()
        .and_then(|b| b["role"].as_str().and_then(orgs::Role::parse))
        .unwrap_or(orgs::Role::Member);
    match orgs::invite(org, role, &p.subject, INVITE_TTL) {
        Ok(v) => Outcome::Structured(201, v),
        Err(e) => Outcome::Err(422, e),
    }
}

fn org_join(request: &IncomingRequest) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    match orgs::redeem(&str_of(&b, "code"), &p.subject, &p.subject) {
        Ok(v) => Outcome::Structured(200, v),
        Err(e) => Outcome::Err(422, e),
    }
}

fn org_members(request: &IncomingRequest, org: &str) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let q: Map<String, Value> = [("org".to_string(), json!(org))].into_iter().collect();
    if let Err((code, msg)) = orgs::acting(&p.subject, &personal_org(&p), &q, orgs::Role::Viewer) {
        return Outcome::Err(code, msg);
    }
    Outcome::Structured(200, json!({ "org": org, "members": orgs::members(org) }))
}

fn org_remove(request: &IncomingRequest, org: &str, subject: &str) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let q: Map<String, Value> = [("org".to_string(), json!(org))].into_iter().collect();
    // Leaving is always allowed; removing someone else needs owner.
    let need = if subject == p.subject { orgs::Role::Viewer } else { orgs::Role::Owner };
    if let Err((code, msg)) = orgs::acting(&p.subject, &personal_org(&p), &q, need) {
        return Outcome::Err(code, msg);
    }
    match orgs::remove_member(org, subject) {
        Ok(()) => Outcome::Structured(200, json!({ "org": org, "removed": subject })),
        Err(e) => Outcome::Err(422, e),
    }
}

// ---- deployments ------------------------------------------------------------

fn deployment_create(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    // Which org this is for. `?org=` or the caller's own, and membership at member
    // level is checked here rather than after the record exists.
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Member) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    // Typed, so a misspelled field is refused with the legal ones rather than
    // quietly creating a deployment that is not what was asked for.
    let b: req::CreateDeployment = match read_body(request)
        .map_err(|_| Outcome::Err(400, "could not read body".into()))
        .and_then(|raw| req::parse(&raw))
    {
        Ok(v) => v,
        Err(o) => return o,
    };
    let name = b.name.trim().to_string();
    let strategy = match Strategy::parse(b.strategy.as_deref().unwrap_or("fused")) {
        Some(s) => s,
        None => return Outcome::Err(422, "strategy must be fused|linked".into()),
    };
    if name.is_empty() {
        return Outcome::Err(422, "name required".into());
    }
    // Per-ORG deployment budget: three people in one company share one allowance,
    // and one person in three companies does not carry theirs between them.
    // `quota:meter`'s limit is a parameter, so the platform is the entitlement
    // store (ADR-0008).
    let budget = plan_of(&org).max_deployments;
    if let Err(quota::QuotaError::Exceeded(remaining)) =
        quota::reserve(&format!("deployments/{org}"), 1, budget, 0)
    {
        return Outcome::Err(
            402,
            format!("deployment budget reached ({budget}); {remaining} remaining"),
        );
    }
    let doc = json!({
        "org": org, "tenant": p.tenant, "name": name, "owner": p.subject,
        "strategy": strategy.as_str(),
        "nodes": b.nodes, "edges": b.edges,
        "created": now(), "revision": 0, "status": "draft",
    });
    match records::create(DEPLOYMENTS, &doc.to_string(), &["org".to_string(), "tenant".to_string()]) {
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

/// A deployment the caller may act on, and at what level.
///
/// Ownership is by ORG membership, not by tenant equality. That is the whole point
/// of orgs: three people in one company must reach the same deployments, and one
/// person in three companies must not carry access between them.
///
/// Falls back to the record's `tenant` for rows written before orgs existed, so a
/// migration is not required to keep an old deployment reachable by its author.
fn owned_deployment(
    p: &auth_types::Principal,
    id: &str,
    need: orgs::Role,
) -> Option<(String, u64, Value)> {
    let (rec, rev, doc) = records::get(DEPLOYMENTS, id)
        .ok()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok().map(|v| (e.id, e.revision, v)))?;
    let owner = doc["org"]
        .as_str()
        .or_else(|| doc["tenant"].as_str())
        .unwrap_or_default()
        .to_string();
    match orgs::role_of(&p.subject, &owner) {
        Some(have) if have >= need => Some((rec, rev, doc)),
        _ => None,
    }
}

fn deployments_list(request: &IncomingRequest) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    // Every org the caller belongs to, not just their own. A person contracting
    // for three companies sees three sets of deployments in one listing, which is
    // the thing orgs exist to make possible.
    let mine: Vec<String> = orgs::memberships(&p.subject)
        .iter()
        .filter_map(|m| m["id"].as_str().map(String::from))
        .collect();
    let mut rows: Vec<Value> = Vec::new();
    for org in &mine {
        for e in records::find_by(DEPLOYMENTS, "org", &json!(org).to_string()).unwrap_or_default() {
            if let Ok(v) = serde_json::from_str::<Value>(&e.data) {
                rows.push(json!({ "id": e.id, "org": org, "name": v["name"],
                                  "strategy": v["strategy"], "revision": v["revision"],
                                  "status": v["status"] }));
            }
        }
    }
    Outcome::Json(200, json!({ "deployments": rows }).to_string())
}

fn deployment_get(request: &IncomingRequest, id: &str) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    match owned_deployment(&p, id, orgs::Role::Viewer) {
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
    // The org this deployment belongs to, which is what a secret reference is checked
    // against. Stored on the record at creation, so it cannot be changed by the body
    // of a save.
    let deploy_org = doc["org"].as_str().unwrap_or_else(|| doc["tenant"].as_str().unwrap_or_default()).to_string();
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

    // `{"id": "gate", "config": {"grace-period-secs": "5"}}` on the graph node.
    // Read once here so both strategies get the same treatment.
    let given_config = |id: &str| -> Map<String, Value> {
        doc["nodes"]
            .as_array()
            .and_then(|a| {
                a.iter().find(|n| n["id"].as_str() == Some(id)).and_then(|n| n["config"].as_object())
            })
            .cloned()
            .unwrap_or_default()
    };

    // `{"id": "gate", "secrets": [{"key": "stripe", "ref": "vault://acme/stripe"}]}`
    let given_secrets = |id: &str| -> Vec<Value> {
        doc["nodes"]
            .as_array()
            .and_then(|a| {
                a.iter().find(|n| n["id"].as_str() == Some(id)).and_then(|n| n["secrets"].as_array())
            })
            .cloned()
            .unwrap_or_default()
    };

    let part_of = |row: &Value| -> Result<Part, Outcome> {
        let surface = surface_from(&row["surface"]);
        // Refused HERE, before anything is built or composed: a config error is the
        // author's to fix and costs nothing to find, while the same mistake reaching
        // a node becomes a component that starts and then fails in front of a user.
        let id = row["id"].as_str().unwrap_or_default().to_string();
        let given = given_config(&id);
        if let Err(why) = check_config(&id, row, &given) {
            return Err(Outcome::Err(422, why));
        }
        let asked = given_secrets(&id);
        let secrets = match check_secrets(&id, &deploy_org, &asked) {
            Ok(v) => v,
            Err(why) => return Err(Outcome::Err(422, why)),
        };

        // Checked LAST of the three, on purpose. A missing digest is a transient
        // pipeline state — "save again in a moment" — while a bad config key or an
        // unresolvable secret is a permanent authoring error. Reporting the transient
        // one first makes an author wait for distribution only to be told they had a
        // typo all along.
        let digest = row["digest"].as_str().unwrap_or_default().to_string();
        if !digest.starts_with("sha256:") {
            return Err(Outcome::Err(
                409,
                format!(
                    "component `{id}` has not been distributed yet — it has no content address, and a deployment can only name bytes by digest (ADR-0006)"
                ),
            ));
        }
        Ok(Part {
            name: id,
            secrets: secrets
                .iter()
                .filter_map(|s| {
                    Some((s["key"].as_str()?.to_string(), s["ref"].as_str()?.to_string()))
                })
                .collect(),
            config: given
                .iter()
                .map(|(k, v)| {
                    // Values are strings on the wire: `wasi:config/store` hands the
                    // guest a string, so a number here would be a lie about what the
                    // component will actually read.
                    (k.clone(), v.as_str().map(String::from).unwrap_or_else(|| v.to_string()))
                })
                .collect(),
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

            // A fused artifact is ONE component with one `wasi:config/store`, so the
            // graph's configs merge into a single namespace. That makes a key used by
            // two components with different values genuinely ambiguous — there is no
            // "whose" about it once wac has composed them — so it is refused rather
            // than resolved by whichever iterated last.
            let mut merged: std::collections::BTreeMap<String, (String, String)> =
                std::collections::BTreeMap::new();
            for row in &rows {
                let id = row["id"].as_str().unwrap_or_default().to_string();
                let given = given_config(&id);
                if let Err(why) = check_config(&id, row, &given) {
                    return Err(Outcome::Err(422, why));
                }
                for (k, v) in &given {
                    let val = v.as_str().map(String::from).unwrap_or_else(|| v.to_string());
                    if let Some((other, prev)) = merged.get(k) {
                        if *prev != val {
                            return Err(Outcome::Err(
                                422,
                                format!(
                                    "fused: `{other}` and `{id}` both set config `{k}`, to different values — a fused artifact has ONE config namespace, so deploy linked or make them agree"
                                ),
                            ));
                        }
                    }
                    merged.insert(k.clone(), (id.clone(), val));
                }
            }

            // Secrets merge like config does, and for the same reason: one artifact
            // asks with one identity. Two components wanting the same KEY from
            // different refs is the ambiguous case.
            let mut fused_secrets: std::collections::BTreeMap<String, (String, String)> =
                std::collections::BTreeMap::new();
            for row in &rows {
                let id = row["id"].as_str().unwrap_or_default().to_string();
                let checked = match check_secrets(&id, &deploy_org, &given_secrets(&id)) {
                    Ok(v) => v,
                    Err(why) => return Err(Outcome::Err(422, why)),
                };
                for sec in checked {
                    let (k, r) = (
                        sec["key"].as_str().unwrap_or_default().to_string(),
                        sec["ref"].as_str().unwrap_or_default().to_string(),
                    );
                    if let Some((other, prev)) = fused_secrets.get(&k) {
                        if *prev != r {
                            return Err(Outcome::Err(
                                422,
                                format!("fused: `{other}` and `{id}` both want secret `{k}`, from different refs — a fused artifact asks with one identity, so deploy linked or make them agree"),
                            ));
                        }
                    }
                    fused_secrets.insert(k, (id.clone(), r));
                }
            }

            let mut part = Part {
                name: fused_id.clone(),
                secrets: fused_secrets.into_iter().map(|(k, (_, r))| (k, r)).collect(),
                config: merged.into_iter().map(|(k, (_, v))| (k, v)).collect(),
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
    // Saving deploys code on the org's behalf; a viewer must not.
    let Some((rec, rev, mut doc)) = owned_deployment(&p, id, orgs::Role::Member) else {
        return Outcome::Err(404, "not_found".into());
    };
    // A save may also update the graph in the same request. An unknown field here
    // used to be ignored, so `{"noodes": [...]}` saved the OLD graph and reported
    // success — the most expensive shape of this bug, because it looks like it worked.
    let update: req::SaveDeployment = match read_body(request)
        .map_err(|_| Outcome::Err(400, "could not read body".into()))
        .and_then(|raw| req::parse(&raw))
    {
        Ok(v) => v,
        Err(o) => return o,
    };
    if let Some(nodes) = update.nodes {
        doc["nodes"] = json!(nodes);
    }
    if let Some(edges) = update.edges {
        doc["edges"] = json!(edges);
    }
    if let Some(strategy) = update.strategy {
        doc["strategy"] = json!(strategy);
    }
    let strategy = match Strategy::parse(doc["strategy"].as_str().unwrap_or("fused")) {
        Some(s) => s,
        None => return Outcome::Err(422, "strategy must be fused|linked".into()),
    };
    let parts = match resolve_parts(&p, &doc, strategy) {
        Ok(parts) => parts,
        Err(o) => return o,
    };
    // THE isolation change. Everything downstream that used the personal tenant —
    // the plan, the hostname, and critically `env_for`, which becomes the storage
    // bucket a running instance gets — is keyed by the owning ORG instead.
    //
    // ADR-0012's property is unchanged and now wider: two orgs cannot see each
    // other's data for the same reason two tenants could not, because the host
    // still names the bucket from a control-plane record the guest cannot write.
    let owner_org = doc["org"]
        .as_str()
        .or_else(|| doc["tenant"].as_str())
        .unwrap_or(&p.tenant)
        .to_string();
    let tenant_plan = plan_of(&owner_org);
    let name = doc["name"].as_str().unwrap_or("app").to_string();
    let suffix = cfg("ingress-suffix", "apps.local");
    let ingress_host =
        format!("{}.{}.{}", manifest::dns_label(&name), manifest::dns_label(&owner_org), suffix);

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
        tenant: &owner_org,
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
        "deployment": id, "tenant": owner_org, "revision": next,
        "strategy": strategy.as_str(), "manifest": doc_manifest,
        "org": owner_org, "saved": now(), "env": manifest::env_for(&owner_org, &name),
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
    if owned_deployment(&p, id, orgs::Role::Viewer).is_none() {
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
    // Deleting destroys the app's storage permanently (ADR-0016), so it is an
    // owner action rather than a member one.
    let Some((rec, _rev, doc)) = owned_deployment(&p, id, orgs::Role::Owner) else {
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
    let owner_org =
        doc["org"].as_str().or_else(|| doc["tenant"].as_str()).unwrap_or(&p.tenant).to_string();
    let env = manifest::env_for(&owner_org, &name);

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

pub(crate) fn find_one(coll: &str, field: &str, value: &str) -> Option<(String, u64, Value)> {
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
                q.insert(percent_decode(k), json!(percent_decode(v)));
            }
        }
    }
    (route, q)
}

/// Undo percent-encoding, and `+` for spaces.
///
/// Values used to be stored raw, so anything a caller escaped stayed escaped: a
/// secret reference arrived as `vault%3A%2F%2Facme%2Fstripe` and compared unequal to
/// the reference it named, which read as "this instance was not granted that
/// reference" for a reference it plainly was. Any query value containing a space,
/// a slash or a colon had the same problem — the market search just never happened
/// to be given one.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    // Not a valid escape: keep it verbatim rather than dropping it,
                    // so a stray `%` in a search term is a search term.
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

#[cfg(test)]
mod config_tests {
    use super::*;

    fn row(keys: &[(&str, bool)]) -> Value {
        json!({
            "id": "gate",
            "config_keys": keys
                .iter()
                .map(|(k, r)| json!({ "key": k, "required": r }))
                .collect::<Vec<_>>(),
        })
    }

    fn given(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), json!(v))).collect()
    }

    #[test]
    fn a_declaration_marks_required_keys_with_a_bang() {
        let q: Map<String, Value> =
            [("config".to_string(), json!("grace-period-secs!, retries"))].into_iter().collect();
        let got = declared_config(&q);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], json!({ "key": "grace-period-secs", "required": true }));
        assert_eq!(got[1], json!({ "key": "retries", "required": false }), "no bang, not required");
        // An absent `?config=` is not an error; it declares nothing.
        assert!(declared_config(&Map::new()).is_empty());
    }

    #[test]
    fn a_typo_is_refused_and_the_message_lists_the_legal_keys() {
        // THE error ADR-0010 promised. "Rejected" is useless; "you wrote
        // grace-period-sec, it takes grace-period-secs" is the whole value.
        let err = check_config("gate", &row(&[("grace-period-secs", false)]),
                               &given(&[("grace-period-sec", "5")]))
            .unwrap_err();
        assert!(err.contains("grace-period-sec"), "{err}");
        assert!(err.contains("grace-period-secs"), "the legal key must be offered: {err}");
    }

    #[test]
    fn a_missing_required_key_is_named() {
        let err = check_config("gate", &row(&[("token", true)]), &given(&[])).unwrap_err();
        assert!(err.contains("token"), "{err}");
        // Optional keys may be omitted, or the declaration would be pointless.
        check_config("gate", &row(&[("token", false)]), &given(&[])).unwrap();
    }

    #[test]
    fn a_component_that_declares_nothing_accepts_nothing() {
        // Silence means "reads no config", not "reads anything" — deny by omission,
        // as everywhere else here. The message has to say so, because an empty list
        // of legal keys reads as a bug otherwise.
        let err = check_config("gate", &json!({ "id": "gate" }), &given(&[("anything", "1")]))
            .unwrap_err();
        assert!(err.contains("declares no config keys"), "{err}");
        // ...and giving it nothing is still fine.
        check_config("gate", &json!({ "id": "gate" }), &given(&[])).unwrap();
    }

    #[test]
    fn a_full_and_correct_config_passes() {
        check_config(
            "gate",
            &row(&[("token", true), ("retries", false)]),
            &given(&[("token", "abc"), ("retries", "3")]),
        )
        .unwrap();
    }
}

#[cfg(test)]
mod query_tests {
    use super::*;

    #[test]
    fn a_reference_survives_being_a_query_value() {
        // The bug: an escaped ref compared unequal to the ref it named, so a token
        // was told it had not been granted a reference it plainly had.
        let (route, q) = split_query("/api/internal/secret?ref=vault%3A%2F%2Facme%2Fstripe");
        assert_eq!(route, "/api/internal/secret");
        assert_eq!(q["ref"], json!("vault://acme/stripe"));
    }

    #[test]
    fn a_search_term_with_a_space_arrives_as_a_space() {
        let (_, q) = split_query("/api/market?q=key%20value&org=acme");
        assert_eq!(q["q"], json!("key value"));
        assert_eq!(q["org"], json!("acme"));
        let (_, plus) = split_query("/api/market?q=key+value");
        assert_eq!(plus["q"], json!("key value"), "+ is a space in a query string");
    }

    #[test]
    fn a_stray_percent_is_kept_rather_than_swallowed() {
        let (_, q) = split_query("/api/market?q=100%");
        assert_eq!(q["q"], json!("100%"));
    }
}
