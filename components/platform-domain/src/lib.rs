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
mod orgs;
mod req;

use serde_json::{json, Map, Value};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::types as auth_types;
use bindings::blob::store::blobstore as blob;
use bindings::comp::store::cas;
use bindings::policy::guard::guard as policy;
use bindings::quota::meter::meter as quota;
use bindings::records::store::store as records;
use bindings::secrets::vault::vault;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::config::store as config;
use bindings::wasi::keyvalue::store as kv;
use bindings::wit::reflect::composer;
use bindings::wit::reflect::inspector;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;

use manifest::{HostIface, ManifestInput, Part, Plan, Strategy};

guestio::guest_bearer!();
guestio::guest_write_all!();

struct Component;

const ACCOUNTS: &str = "tenants";
const CATALOG: &str = "catalog";
/// Publisher verifying keys, one row per key, indexed by org (ADR-0073).
const ORGKEYS: &str = "orgkeys";
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

            (Method::Post, ["api", "projects"]) => project_create(&request, &query),
            (Method::Get, ["api", "projects"]) => projects_list(&request, &query),
            (Method::Post, ["api", "projects", project, "goals"]) => {
                goal_create(&request, project, &query)
            }
            (Method::Get, ["api", "projects", project, "goals"]) => {
                goals_list(&request, project, &query)
            }
            // A human starts every goal; there is no loop that does (ADR-0082).
            (Method::Post, ["api", "goals", id, "start"]) => {
                goal_transition(&request, id, "running", &query)
            }
            (Method::Post, ["api", "goals", id, "fail"]) => {
                goal_transition(&request, id, "failed", &query)
            }
            (Method::Post, ["api", "goals", id, "done"]) => {
                goal_transition(&request, id, "done", &query)
            }
            (Method::Post, ["api", "goals", id, "review"]) => {
                goal_transition(&request, id, "awaiting-human", &query)
            }
            (Method::Delete, ["api", "goals", id]) => {
                goal_transition(&request, id, "abandoned", &query)
            }

            (Method::Post, ["api", "deployments"]) => deployment_create(&request, &query),
            (Method::Get, ["api", "deployments"]) => deployments_list(&request),
            (Method::Get, ["api", "deployments", id]) => deployment_get(&request, id),
            (Method::Post, ["api", "deployments", id, "save"]) => {
                deployment_save(&request, id, &query)
            }
            (Method::Get, ["api", "deployments", id, "manifests"]) => manifests(&request, id),
            (Method::Delete, ["api", "deployments", id]) => deployment_delete(&request, id, &query),

            // The applier polls this to re-apply current revisions (ADR-0004).
            (Method::Get, ["api", "internal", "revisions"]) => internal_revisions(&request),
            (Method::Post, ["api", "internal", "status"]) => internal_status_put(&request),
            // The push path (ADR-0017), reconciled like everything else: the applier
            // asks what needs pushing, fetches the bytes, pushes, then reports back.
            (Method::Get, ["api", "internal", "pending-pushes"]) => internal_pending(&request),
            (Method::Get, ["api", "internal", "artifact"]) => internal_artifact(&request, &query),
            // The seam the push step calls once an artifact is in the registry.
            (Method::Post, ["api", "internal", "pushed"]) => internal_pushed(&request),
            (Method::Post, ["api", "internal", "repair"]) => internal_repair(&request, &query),
            (Method::Get, ["api", "internal", "verify"]) => internal_verify(&request, &query),
            (Method::Post, ["api", "environments"]) => env_spawn(&request, &query),
            (Method::Post, ["api", "internal", "environments"]) => {
                internal_env_spawn(&request, &query)
            }
            (Method::Get, ["api", "environments"]) => env_list(&request, &query),
            (Method::Delete, ["api", "environments"]) => env_despawn(&request, &query),
            (Method::Post, ["api", "keys"]) => key_add(&request, &query),
            (Method::Get, ["api", "keys"]) => key_list(&request, &query),
            (Method::Post, ["api", "keys", "revoke"]) => key_revoke(&request, &query),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
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
    authorizer::introspect(&bearer(request)?).ok()
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
        find_one(CATALOG, "key", &format!("{}/{}", p.tenant, id)).map(|(_, _, v)| v).or_else(|| {
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
            return Err(Outcome::Err(
                422,
                format!("component `{id}` is unknown or not visible to you"),
            ));
        };
        blob::get(BIN, &staged_key(&row)).map_err(|_| {
            Outcome::Err(409, format!("component `{id}` has no staged bytes — re-upload it"))
        })
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

    // STAGED BY CONTENT, not by name.
    //
    // `tenant/id` is a mutable pointer, and staging under it made an upload
    // destructive: a second build overwrote the first, so two workers pushing
    // different builds of one component raced and the loser's bytes were simply
    // gone. Under a content key that cannot happen — identical bytes write
    // identical bytes to the same place, and different bytes go somewhere else,
    // so neither writer can lose.
    //
    // The row stays keyed by `tenant/id`, because that is the name a person and a
    // deployment use. It now POINTS at the content rather than being it.
    let content = surface.sha256.clone();
    let blob_key = format!("sha256/{content}");
    if blob::put(BIN, &blob_key, &bytes, "application/wasm").is_err() {
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
    let existing = find_one(CATALOG, "key", &key);

    // Tags accumulate. A row is a set of pointers, and moving one must not drop
    // the others — an older tag still names bytes that are still staged, so
    // forgetting it would strand them behind a name nobody can spell.
    let mut tag_map = existing
        .as_ref()
        .and_then(|(_, _, row)| row["tags"].as_object().cloned())
        .unwrap_or_default();
    if let Some(t) = query.get("tag").and_then(|v| v.as_str()).filter(|t| !t.is_empty()) {
        tag_map.insert(t.to_string(), json!(content));
    }
    let tags = Value::Object(tag_map);

    let doc = json!({
        "key": key, "id": id, "tenant": p.tenant, "uploader": p.subject,
        "org": org,
        "visibility": "private", "uploaded": now(),
        "config_keys": config_keys,
        // The content address of what was uploaded, and where those bytes are
        // staged. The row is a pointer; this is what it points at.
        "content": content,
        "blob_key": blob_key,
        // name -> content, so a tag can be resolved later. Merged rather than
        // replaced: tagging a new build must not forget where the old tags point.
        "tags": tags,
        // The OCI reference. Empty until the push step records a digest — and a
        // deployment cannot render without one (ADR-0006).
        "oci_ref": "",
        "surface": surface_json(&surface),
    });
    // Re-uploading the SAME BYTES is a no-op, not a new version.
    //
    // Without this, an upload always cleared the digest and forced the whole
    // distribution round again — for bytes the fleet already has, byte for byte.
    // A retry, a re-run of a build script, or two workers landing on the same
    // output all did that. Content-addressing makes "is this actually different"
    // answerable, so it is answered.
    if let Some((_, _, row)) = &existing {
        if row["content"].as_str() == Some(content.as_str())
            && row["digest"].as_str().unwrap_or_default().starts_with("sha256:")
        {
            return Outcome::Json(
                200,
                json!({
                    "key": key, "id": id, "content": content,
                    "digest": row["digest"],
                    "unchanged": true,
                })
                .to_string(),
            );
        }
    }

    let ok = match existing {
        // Guarded on the revision that was read. Two workers moving one name at
        // the same moment is the case content-addressing does NOT solve — the
        // bytes are safe either way, the pointer is one value — so the loser is
        // told rather than silently overwritten.
        Some((rec, rev, _)) => match records::update(CATALOG, &rec, &doc.to_string(), rev) {
            Ok(_) => true,
            Err(records::StoreError::RevisionConflict(_)) => {
                return Outcome::Err(
                    409,
                    format!(
                        "`{id}` was uploaded by someone else while this upload was in flight — \
                         the bytes are staged and safe, re-run to move the pointer"
                    ),
                )
            }
            Err(_) => false,
        },
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
    // `refs` may be empty. This started as a SECRETS credential, minted only for
    // instances that had any — but it is really an instance's proof of who it is,
    // and ADR-0079 needs that for an instance with no secrets at all. An empty
    // ref list simply authorises no secret.
    if instance.is_empty() {
        return Outcome::Err(422, "instance is required".into());
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
        Ok(rec) => {
            Outcome::Json(201, json!({ "token": rec.id, "expires": now() + ttl }).to_string())
        }
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
        .get("x-fetch-token")
        .into_iter()
        .next()
        .and_then(|v| String::from_utf8(v).ok())
        .unwrap_or_default();
    if token.is_empty() {
        return Outcome::Err(401, "no fetch token".into());
    }
    // Replay protection (ADR-0071). Without it a captured fetch could be replayed
    // against the platform for the rest of the token's life — the gap ADR-0051
    // named and did not close.
    if let Err(o) = claim_fetch_nonce(request) {
        return o;
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
    let want =
        |k: &str| query.get(k).and_then(|v| v.as_str()).unwrap_or_default().trim().to_lowercase();
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
                // ADR-0007 rule 5: public means the provenance is public too —
                // which key vouched for these bytes, and when. Absent on private
                // and org rows, which need no signature.
                "signed_by": r["signed_by"], "signed_at": r["signed_at"],
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
    let key = format!("{}/{}", p.tenant, str_of(&b, "id"));
    match find_one(CATALOG, "key", &key) {
        Some((rec, rev, mut row)) if row["tenant"] == json!(p.tenant) => {
            // ADR-0007 rule 3, built in ADR-0073: public requires a signature over
            // the digest, by a key the owning organisation registered.
            if visibility == "public" {
                let digest = str_of(&row, "digest");
                if !digest.starts_with("sha256:") {
                    return Outcome::Err(
                        409,
                        "nothing to sign yet — this component has no digest until its bytes are pushed".into(),
                    );
                }
                let org = str_of(&row, "org");
                let sig = str_of(&b, "signature");
                if sig.is_empty() {
                    return Outcome::Err(
                        422,
                        format!(
                            "public requires `signature`: an ECDSA P-256 signature over {digest}"
                        ),
                    );
                }
                match verify_publish(&org, &digest, &sig) {
                    Some(name) => {
                        // Bound to the digest it covers. A later push replaces the
                        // digest, and `internal_pushed` demotes the row rather than
                        // letting new bytes inherit somebody's signature on old ones
                        // — which is ADR-0007 rule 1 ("visibility only ever widens by
                        // an explicit act") held by the data instead of by a version
                        // in the key, which this catalogue does not have.
                        row["signed_digest"] = json!(digest);
                        row["signed_by"] = json!(name);
                        row["signed_at"] = json!(now());
                    }
                    None => {
                        return Outcome::Err(
                            403,
                            format!(
                                "no key registered to `{org}` verifies that signature over {digest}"
                            ),
                        )
                    }
                }
            }
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
            // The CONTENT key when there is one, so the distributor fetches the
            // bytes this row was created from rather than whatever is staged
            // under the name now — which a concurrent upload may already have
            // replaced. Composed artifacts are staged by name and fall back to
            // it, which is why this goes through `staged_key` instead of reading
            // the field: reading it directly handed the distributor an empty key
            // for every fused row.
            "key": staged_key(&row),
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
        return Outcome::Err(422, "?key= required (a content key, `sha256/<hex>`)".into());
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
    // The distributor reports the STAGED key — `staged_key(row)`, which is the
    // content key `sha256/<hash>` for anything uploaded raw, and the name key
    // `tenant/id` only for a composed artifact staged by name. Match the row
    // whose staged location IS this key, falling back to the name index.
    //
    // Matching the name index ALONE is why a linked multi-component graph deployed
    // through this API never distributed: every part is content-staged, reports
    // `sha256/<hash>`, and found no row here — a 404 on each, forever. Composed
    // single-artifact deployments happened to stage by name and slipped through,
    // which is why it was never caught.
    let found = find_one(CATALOG, "key", &key).or_else(|| {
        all_records(CATALOG, 100_000).into_iter().find_map(|e| {
            let row = serde_json::from_str::<Value>(&e.data).ok()?;
            (staged_key(&row) == key).then(|| (e.id.clone(), e.revision, row))
        })
    });
    match found {
        Some((rec, rev, mut row)) => {
            // The bare content address, with no registry host in front of it
            // (ADR-0024). A node fetches by digest from the object store, so a
            // reference that named a registry would name something no node can
            // reach — and would make the same bytes have two identities.
            // New bytes, so any signature on the old ones no longer says anything
            // about this row. Demoted rather than refused: the upload is legitimate,
            // it is the PUBLIC claim that is not, and re-publishing with a fresh
            // signature is one call away (ADR-0073).
            if row["visibility"] == json!("public") && str_of(&row, "signed_digest") != digest {
                row["visibility"] = json!("private");
                row["unpublished_reason"] = json!("new bytes pushed; re-sign to publish again");
            }
            row["digest"] = json!(digest);
            let _ = records::update(CATALOG, &rec, &row.to_string(), rev);
            Outcome::Json(200, json!({ "key": key, "digest": row["digest"] }).to_string())
        }
        None => Outcome::Err(404, "not_found".into()),
    }
}

/// Rebuild a collection's id index from the records themselves (ADR-0068).
///
/// The platform's own collections are the ones whose loss hurts most — the
/// catalogue, deployments, orgs — and until this route existed an index that had
/// dropped an id was permanent: the record was still in the store and no longer
/// in any listing, with nothing able to put it back.
///
/// Internal, not a user route. It scans the whole bucket, and "rebuild the index"
/// is an operator action even when it is safe — which it is: it converges on what
/// the records say, so running it twice reports zero the second time.
///
/// A tenant's app has its own records in its own bucket and must expose its own;
/// this one can only reach the platform's.
fn internal_repair(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    if !internal_ok(request) {
        return Outcome::Err(401, "internal endpoint".into());
    }
    let collection = query.get("collection").and_then(|v| v.as_str()).unwrap_or_default();
    if collection.is_empty() {
        return Outcome::Err(422, "?collection=<name> required".into());
    }
    match records::repair(collection) {
        Ok(r) => Outcome::Json(
            200,
            json!({
                "collection": collection,
                "readded": r.readded,
                "pruned": r.pruned,
                "total": r.total,
                "indexes": r.indexes,
                "indexes_dropped": r.indexes_dropped,
            })
            .to_string(),
        ),
        Err(e) => Outcome::Err(500, format!("repair {collection}: {e:?}")),
    }
}

/// Register a verifying key for an organisation (ADR-0073).
///
/// The PUBLIC half only — the platform never sees a private key, and a publisher
/// who loses theirs registers another rather than asking anyone to recover one.
/// Member, not viewer: adding a key is the act that decides whose bytes the whole
/// platform will later trust as public.
fn key_add(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Member) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (name, key_b64) = (str_of(&b, "name"), str_of(&b, "public_key"));
    if name.is_empty() || key_b64.is_empty() {
        return Outcome::Err(422, "name and public_key required".into());
    }
    // Parsed here rather than at first use: a key that cannot verify anything
    // should be refused by the call that adds it, not by a publish six weeks later.
    let raw = match B64.decode(key_b64.trim()) {
        Ok(r) => r,
        Err(_) => return Outcome::Err(422, "public_key must be base64".into()),
    };
    if p256::ecdsa::VerifyingKey::from_sec1_bytes(&raw).is_err() {
        return Outcome::Err(422, "public_key must be a SEC1 P-256 point (33 or 65 bytes)".into());
    }
    let doc = json!({
        "org": org, "name": name, "public_key": key_b64.trim(),
        "added_by": p.subject, "added": now(),
    });
    match records::create(ORGKEYS, &doc.to_string(), &["org".to_string()]) {
        Ok(rec) => {
            Outcome::Json(201, json!({ "id": rec.id, "org": org, "name": name }).to_string())
        }
        Err(e) => Outcome::Err(500, format!("storing the key: {e:?}")),
    }
}

/// The keys an organisation publishes under. Public information by construction —
/// a verifying key is what lets anyone else check the signature.
fn key_list(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Viewer) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    // Revoked keys are listed too, marked. A key that vanished from the listing
    // would leave anyone auditing an old signature unable to find out what
    // happened to the key that made it.
    let keys: Vec<Value> = records::find_by(ORGKEYS, "org", &json!(org).to_string())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .map(|v| {
            json!({
                "name": str_of(&v, "name"),
                "public_key": str_of(&v, "public_key"),
                "revoked": v["revoked"].as_bool().unwrap_or(false),
                "revoked_at": v["revoked_at"],
            })
        })
        .collect();
    Outcome::Json(200, json!({ "org": org, "count": keys.len(), "keys": keys }).to_string())
}

/// The keys an org can publish under RIGHT NOW. A revoked key is skipped, so it
/// stops verifying new publishes the moment it is revoked — the easy half.
fn org_keys(org: &str) -> Vec<(String, String)> {
    records::find_by(ORGKEYS, "org", &json!(org).to_string())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .filter(|v| !v["revoked"].as_bool().unwrap_or(false))
        .map(|v| (str_of(&v, "name"), str_of(&v, "public_key")))
        .collect()
}

/// Revoke a key and unpublish everything it vouched for (ADR-0076).
///
/// ADR-0073 built signing and left this open: "removing a key does not un-publish
/// what it signed, and 'distrust everything this key signed' has no answer". It
/// does now, and the answer is only possible because a public row records WHICH
/// key vouched for it — provenance is what makes revocation actionable rather
/// than a gesture.
///
/// Demoted to private, not deleted. ADR-0007 rule 4 says a digest anything
/// references must stay resolvable; revocation says "stop offering this to
/// strangers", not "break whoever already deployed it". A consumer who pinned the
/// digest keeps running, which is the whole point of pinning — what they lose is
/// the platform's word that it is still trusted.
fn key_revoke(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    // Owner, not member. Adding a key widens what the org can publish; revoking
    // one retracts published bytes, which is the louder act of the two.
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Owner) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let name = str_of(&b, "name");
    if name.is_empty() {
        return Outcome::Err(422, "name required".into());
    }

    let mut found = false;
    for e in records::find_by(ORGKEYS, "org", &json!(org).to_string()).unwrap_or_default() {
        let Ok(mut row) = serde_json::from_str::<Value>(&e.data) else { continue };
        if str_of(&row, "name") != name {
            continue;
        }
        found = true;
        row["revoked"] = json!(true);
        row["revoked_at"] = json!(now());
        row["revoked_by"] = json!(p.subject);
        let _ = records::update(ORGKEYS, &e.id, &row.to_string(), e.revision);
    }
    if !found {
        return Outcome::Err(404, format!("`{org}` has no key called `{name}`"));
    }

    // Everything that key vouched for. Walked rather than indexed: revocation is
    // rare and a catalogue scan is cheap next to being wrong about which rows a
    // compromised key touched.
    let mut unpublished = Vec::new();
    for e in records::list_records(CATALOG, 1000, "").map(|p| p.entries).unwrap_or_default() {
        let Ok(mut row) = serde_json::from_str::<Value>(&e.data) else { continue };
        if str_of(&row, "org") != org
            || row["visibility"] != json!("public")
            || str_of(&row, "signed_by") != name
        {
            continue;
        }
        row["visibility"] = json!("private");
        row["unpublished_reason"] = json!(format!("the key `{name}` that signed it was revoked"));
        if records::update(CATALOG, &e.id, &row.to_string(), e.revision).is_ok() {
            unpublished.push(str_of(&row, "id"));
        }
    }
    Outcome::Json(
        200,
        json!({
            "org": org, "key": name, "revoked": true,
            "unpublished": unpublished, "count": unpublished.len(),
        })
        .to_string(),
    )
}

/// Does `signature` cover `digest` under any key this org registered?
///
/// The message signed is the digest STRING, exactly as the catalogue stores it —
/// `sha256:…`. Signing the content address rather than a manifest is what makes
/// the promise checkable by anyone later: the bytes are the digest.
///
/// Returns the name of the key that verified, for provenance (ADR-0007 rule 5).
fn verify_publish(org: &str, digest: &str, signature_b64: &str) -> Option<String> {
    use p256::ecdsa::signature::Verifier;
    let sig_raw = B64.decode(signature_b64.trim()).ok()?;
    // Both encodings, because a signer using the p256 crate emits fixed-width and
    // one using OpenSSL emits DER, and refusing either would be a papercut with a
    // very confusing error message.
    let sig = p256::ecdsa::Signature::from_slice(&sig_raw)
        .ok()
        .or_else(|| p256::ecdsa::Signature::from_der(&sig_raw).ok())?;
    for (name, key_b64) in org_keys(org) {
        let Ok(raw) = B64.decode(key_b64.trim()) else { continue };
        let Ok(vk) = p256::ecdsa::VerifyingKey::from_sec1_bytes(&raw) else { continue };
        if vk.verify(digest.as_bytes(), &sig).is_ok() {
            return Some(name);
        }
    }
    None
}

/// How far apart the host's clock and ours may be, in seconds. Narrow on purpose:
/// it is the only thing bounding how many nonces have to be remembered, and two
/// machines on one tailnet have no excuse for more.
const FETCH_SKEW_SECS: u64 = 60;

/// Claim this request's nonce, exactly once.
///
/// The check is one guarded write whose FAILURE is the answer: `cas::set` with an
/// expected revision of 0 means "must not exist yet", so the first claim commits
/// and every replay conflicts. No lookup, no index, no read-then-check race — the
/// store decides, which is the whole point of ADR-0066.
///
/// A missing header is refused rather than waved through: an old host that does
/// not send one is a host whose requests can be replayed, and silently accepting
/// it would make this decoration.
fn claim_fetch_nonce(request: &IncomingRequest) -> Result<(), Outcome> {
    let header = |name: &str| -> String {
        request
            .headers()
            .get(name)
            .into_iter()
            .next()
            .and_then(|v| String::from_utf8(v).ok())
            .unwrap_or_default()
    };
    let (nonce, ts) = (header("x-fetch-nonce"), header("x-fetch-ts"));
    if nonce.is_empty() || ts.is_empty() {
        return Err(Outcome::Err(409, "this request carries no nonce".into()));
    }
    let Ok(ts) = ts.parse::<u64>() else {
        return Err(Outcome::Err(409, "unreadable timestamp".into()));
    };
    let now = now();
    if now.abs_diff(ts) > FETCH_SKEW_SECS {
        // Outside the window the nonce set no longer proves anything, so the
        // request is refused whether or not it is a replay.
        return Err(Outcome::Err(409, "stale request".into()));
    }
    let Ok(bucket) = kv::open("default") else {
        return Err(Outcome::Err(503, "store unavailable".into()));
    };
    // The timestamp is part of the key so a sweeper can drop a whole window by
    // prefix later, and so two windows cannot collide on one nonce.
    let key = format!("fn_{}_{}", ts / FETCH_SKEW_SECS, sanitize_key(&nonce));
    match cas::set(&bucket, &key, b"1", 0) {
        Ok(cas::Outcome::Committed(_)) => Ok(()),
        Ok(cas::Outcome::Conflict(_)) => {
            Err(Outcome::Err(409, "this request has already been used".into()))
        }
        Err(_) => Err(Outcome::Err(503, "store unavailable".into())),
    }
}

/// Nonces are host-generated and already tame, but a key goes into the store and
/// nothing that reaches a store should be trusted to be well-formed.
fn sanitize_key(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .take(80)
        .collect()
}

/// The app name an environment runs under.
///
/// An environment IS a derived app, which is the whole trick (ADR-0078): the
/// store a component opens is `b-app-{tenant}-{app}` (ADR-0023), so deriving the
/// app name gives each environment its own store with no new isolation
/// machinery. Placement, scaling, links and reaping all keep working because
/// nothing below the platform knows this app was born differently.
fn env_app(app: &str, env: &str) -> String {
    format!("{app}-env-{env}")
}

/// Environment names are restricted because the derived name is collapsed to a
/// DNS label before it becomes a bucket, and two names that collapse together
/// would share a store.
fn valid_env_name(env: &str) -> bool {
    !env.is_empty()
        && env.len() <= 32
        && env.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !env.starts_with('-')
        && !env.ends_with('-')
}

/// Spawn on behalf of a RUNNING INSTANCE, authorised by its own token (ADR-0079).
///
/// The token says which instance is calling — `{tenant}/{app}/{component}@{node}`,
/// minted by the reconciler and never seen by the guest — so a component can fork
/// the app it is part of and nothing else. Scoped by construction rather than by
/// checking a parameter the caller supplied.
fn internal_env_spawn(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let token = request
        .headers()
        .get("x-fetch-token")
        .into_iter()
        .next()
        .and_then(|v| String::from_utf8(v).ok())
        .unwrap_or_default();
    if token.is_empty() {
        return Outcome::Err(401, "no instance token".into());
    }
    let Ok(entry) = records::get(FETCH_TOKENS, &token) else {
        return Outcome::Err(401, "unknown instance token".into());
    };
    let Ok(doc) = serde_json::from_str::<Value>(&entry.data) else {
        return Outcome::Err(401, "unreadable instance token".into());
    };
    if doc["expires"].as_u64().unwrap_or(0) < now() {
        return Outcome::Err(401, "expired".into());
    }
    // `tenant/app/component@node` — the app is the middle segment, and it is the
    // only thing this call is allowed to fork.
    let instance = str_of(&doc, "instance");
    let app = instance.split('/').nth(1).unwrap_or_default().to_string();
    if app.is_empty() {
        return Outcome::Err(422, format!("unreadable instance `{instance}`"));
    }
    let env = query.get("env").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    if !valid_env_name(&env) {
        return Outcome::Err(422, "env must be 1-32 chars of [a-z0-9-]".into());
    }
    spawn_environment(&app, &env)
}

/// Spawn a parallel environment: the same graph, its own store (ADR-0078).
///
/// Recorded as desired state rather than started directly, and that is not a
/// preference. `plan()` stops any instance no manifest asks for — "nothing wanted
/// here at all: take it off this node" — so anything started behind the
/// reconciler's back is reaped within one pass. Desired state is the only durable
/// way to ask for an instance.
fn env_spawn(request: &IncomingRequest, _query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    // BEFORE the lookup, not after. Admission is about whether the fleet can take
    // more work at all, so it must not depend on the request being otherwise
    // valid — and putting it later meant a refusal never fired, because the
    // parent lookup answered first.
    if let Err(refusal) = admit_one_more() {
        return refusal;
    }
    let (app, env) = (str_of(&b, "app"), str_of(&b, "env"));
    if app.is_empty() || !valid_env_name(&env) {
        return Outcome::Err(
            422,
            "app required, and env must be 1-32 chars of [a-z0-9-] not starting or ending with -"
                .into(),
        );
    }

    // The parent's newest revision is what an environment is a copy OF. Spawning
    // from a deployment that does not exist, or one this principal cannot see, is
    // the same 404 either way.
    // Deployments are indexed on `org` and `tenant` only, so neither a name nor an
    // id is a `find_by` — the first two versions of this line looked up fields
    // that carry no index and 404'd on a deployment that plainly existed.
    // Fetching by record id when it looks like one, and otherwise scanning this
    // principal's own deployments by name, which is what a caller actually types.
    // Both halves are needed and they are different strings: revisions are keyed
    // by the deployment's RECORD ID, while a manifest's `app` — and therefore the
    // store name — is its NAME. Losing the id here is what made the revision
    // lookup come up empty for a deployment that had just been saved.
    let parent = records::get(DEPLOYMENTS, &app)
        .ok()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok().map(|d| (e.id, d)))
        .or_else(|| {
            all_records(DEPLOYMENTS, 100_000)
                .into_iter()
                .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok().map(|d| (e.id, d)))
                .find(|(_, d)| str_of(d, "name") == app)
        });
    // A parent may also be an ENVIRONMENT, which is how a search explores from a
    // promising branch rather than only fanning out from the root. An environment
    // has no deployment record — it is a revision — so its owner comes from that.
    // Missing this second case is what capped depth at one.
    let (app, owner) = match parent {
        Some((_, doc)) => {
            // Everything downstream keys on the deployment's NAME, because that
            // is what the manifest's `app` is and therefore what the store is
            // named after.
            (str_of(&doc, "name"), str_of(&doc, "org"))
        }
        None => match newest_revision(&app).filter(|r| !r["environment"].is_null()) {
            Some(rev) => (app.clone(), str_of(&rev, "org")),
            None => return Outcome::Err(404, format!("no deployment or environment `{app}`")),
        },
    };
    if let Err((code, msg)) = orgs::acting(
        &p.subject,
        &personal_org(&p),
        &Map::from_iter([("org".into(), json!(owner))]),
        orgs::Role::Member,
    ) {
        return Outcome::Err(code, msg);
    }
    // Whether there IS a revision to copy is `spawn_environment`'s business; this
    // route only has to establish that the caller may act for the owning org.
    spawn_environment(&app, &env)
}

/// Everything after "who is asking": copy the app's newest revision under a
/// derived name. Shared by the user-facing route and the instance one, because a
/// fork requested by an agent and a fork requested by a person must produce the
/// same thing.
/// The newest revision of `app`, whether it is a deployment or an environment,
/// plus the org that owns it.
///
/// Two lookups because revisions are keyed two ways: a real deployment's
/// revisions carry its RECORD ID in `deployment`, while an environment's carry
/// its derived NAME. That asymmetry is why environments could not nest — the
/// parent lookup only ever searched deployments, so the second level came back
/// `404 no deployment` and a tree search stopped at one.
fn parent_of(app: &str) -> Option<(Value, String)> {
    let deployment = all_records(DEPLOYMENTS, 100_000)
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok().map(|d| (e.id, d)))
        .find(|(_, d)| str_of(d, "name") == app);
    if let Some((id, doc)) = deployment {
        return newest_revision(&id).map(|rev| (rev, str_of(&doc, "org")));
    }
    // Not a deployment. It may be an environment, which is a legitimate parent:
    // exploring FROM a promising branch is what makes a search a search rather
    // than a single fan-out.
    let rev = newest_revision(app)?;
    if rev["environment"].is_null() {
        return None;
    }
    let owner = str_of(&rev, "org");
    Some((rev, owner))
}

fn spawn_environment(app: &str, env: &str) -> Outcome {
    let Some((latest, owner)) = parent_of(app) else {
        return Outcome::Err(404, format!("no deployment or environment `{app}`"));
    };
    let derived = env_app(app, env);
    // A derived name that collides with a real deployment would put two apps in
    // one store. Refused rather than resolved: `shop` + env `x` and an app called
    // `shop-env-x` are indistinguishable once the name is a DNS label.
    let name_taken = all_records(DEPLOYMENTS, 100_000)
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .any(|d| str_of(&d, "name") == derived);
    if name_taken {
        return Outcome::Err(409, format!("`{derived}` is already a deployment"));
    }
    if newest_revision(&derived).is_some() {
        return Outcome::Err(409, format!("environment `{env}` of `{app}` already exists"));
    }

    let mut manifest = latest["manifest"].clone();
    manifest["app"] = json!(derived);
    // An environment gets a DERIVED front door, not the parent's and not none
    // (ADR-0083, amending ADR-0078).
    //
    // The original hazard is real and unchanged: the parent's hostname on two
    // apps makes the ingress route to whichever it saw last. But NO hostname
    // makes an environment undrivable from outside, and a branch of a swarm is
    // something that must be driven — handed a plan, asked for a result. An app
    // with no address cannot be.
    //
    // So the environment's name is prefixed onto the parent's host. It cannot
    // collide with the parent, and two environments collide only if their names
    // do — which `spawn_environment` already refuses. Nesting composes: an
    // environment of an environment is `b.a.parent.org.test`.
    if let Some(host) = manifest["ingress"]["host"].as_str() {
        let derived = format!("{}.{host}", manifest::dns_label(env));
        manifest["ingress"]["host"] = json!(derived);
    }

    let revision_doc = json!({
        "deployment": derived, "tenant": str_of(&latest, "tenant"), "revision": 1,
        "strategy": latest["strategy"], "manifest": manifest,
        "org": owner, "saved": now(),
        // What it is and where it came from, so a listing can show the family and
        // a despawn knows what it is removing.
        "environment": { "of": app, "name": env, "from_revision": latest["revision"] },
    });
    match records::create(
        REVISIONS,
        &revision_doc.to_string(),
        &["deployment".to_string(), "tenant".to_string()],
    ) {
        Ok(_) => Outcome::Json(
            201,
            json!({
                "environment": env, "of": app, "app": derived,
                "from_revision": latest["revision"],
                "note": "the reconciler converges on the next pass",
            })
            .to_string(),
        ),
        Err(e) => Outcome::Err(500, format!("recording the environment: {e:?}")),
    }
}

/// Every environment of an app, with the revision each was copied from.
fn env_list(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(_p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let of = query.get("app").and_then(|v| v.as_str()).unwrap_or_default();
    let envs: Vec<Value> = all_records(REVISIONS, 100_000)
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .filter(|r| {
            !r["environment"].is_null() && (of.is_empty() || r["environment"]["of"] == json!(of))
        })
        .map(|r| {
            json!({
                "environment": r["environment"]["name"],
                "of": r["environment"]["of"],
                "app": r["deployment"],
                "from_revision": r["environment"]["from_revision"],
            })
        })
        .collect();
    Outcome::Json(200, json!({ "count": envs.len(), "environments": envs }).to_string())
}

/// Remove an environment. The reconciler stops its instances on the next pass,
/// for the same reason it started them: nothing wants them any more.
fn env_despawn(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let (app, env) = (
        query.get("app").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        query.get("env").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
    );
    if app.is_empty() || env.is_empty() {
        return Outcome::Err(422, "?app=&env= required".into());
    }
    let derived = env_app(&app, &env);
    let _ = p;

    // Environments nest, so closing one has to close what grew out of it. A
    // descendant left behind is an app still running that nobody can name: its
    // parent is gone, so nothing lists it and no despawn reaches it. It would
    // simply consume a node until someone read a ledger by hand.
    //
    // The naming makes the subtree findable without a graph walk — every
    // descendant of `x` is named `x-env-…` (ADR-0078).
    let prefix = format!("{derived}-env-");
    let mut doomed: Vec<(String, String)> = Vec::new();
    // Paged, not capped at 1000. See `all_records`: the flat limit silently hid
    // every deployment past roughly the five-hundredth, which is how a fleet
    // asked for 3906 apps sat at 500 forever.
    for e in all_records(REVISIONS, 100_000) {
        let Ok(row) = serde_json::from_str::<Value>(&e.data) else { continue };
        let name = str_of(&row, "deployment");
        if name != derived && !name.starts_with(&prefix) {
            continue;
        }
        if row["environment"].is_null() {
            // Not an environment: a real deployment that happens to be named
            // this. Refusing beats deleting somebody's app because the names
            // rhyme.
            return Outcome::Err(409, format!("`{name}` is a deployment, not an environment"));
        }
        doomed.push((e.id, name));
    }
    if doomed.is_empty() {
        return Outcome::Err(404, format!("no environment `{env}` of `{app}`"));
    }

    let mut removed = 0;
    let mut closed: Vec<String> = Vec::new();
    for (id, name) in doomed {
        if records::delete(REVISIONS, &id).is_ok() {
            removed += 1;
            if !closed.contains(&name) {
                closed.push(name);
            }
        }
    }
    closed.sort();
    Outcome::Json(
        200,
        json!({
            "environment": env, "of": app, "app": derived,
            "removed": removed,
            // Named rather than counted: a caller that spawned a subtree wants to
            // know exactly what went with it.
            "closed": closed,
        })
        .to_string(),
    )
}

/// The newest revision row for a deployment id.
fn newest_revision(id: &str) -> Option<Value> {
    records::find_by(REVISIONS, "deployment", &json!(id).to_string())
        .ok()?
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .max_by_key(|v| v["revision"].as_u64().unwrap_or(0))
}

/// What `repair` WOULD do, changing nothing (ADR-0075).
///
/// A GET, because it is a question. Something has to run it on a schedule for a
/// disagreement to be noticed rather than stumbled over — that scheduler is not
/// here, and pretending otherwise would be worse than saying so.
fn internal_verify(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    if !internal_ok(request) {
        return Outcome::Err(401, "internal endpoint".into());
    }
    let collection = query.get("collection").and_then(|v| v.as_str()).unwrap_or_default();
    if collection.is_empty() {
        return Outcome::Err(422, "?collection=<name> required".into());
    }
    match records::verify(collection) {
        Ok(r) => {
            let clean = r.readded == 0 && r.pruned == 0 && r.indexes_dropped == 0;
            Outcome::Json(
                200,
                json!({
                    "collection": collection,
                    "clean": clean,
                    // Named for what they WOULD be, since nothing was written.
                    "records_unindexed": r.readded,
                    "index_entries_dangling": r.pruned,
                    "stale_index_keys": r.indexes_dropped,
                    "total": r.total,
                })
                .to_string(),
            )
        }
        Err(e) => Outcome::Err(500, format!("verify {collection}: {e:?}")),
    }
}

fn internal_ok(request: &IncomingRequest) -> bool {
    let want = cfg("applier-secret", "");
    if want.is_empty() {
        return false;
    }
    request
        .headers()
        .get("x-platform-secret")
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
    match records::create(DEPLOYMENTS, &doc.to_string(), &["org".to_string(), "tenant".to_string()])
    {
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
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
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
    let owner =
        doc["org"].as_str().or_else(|| doc["tenant"].as_str()).unwrap_or_default().to_string();
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
    let deploy_org = doc["org"]
        .as_str()
        .unwrap_or_else(|| doc["tenant"].as_str().unwrap_or_default())
        .to_string();
    seed_policy();
    let node_ids: Vec<String> = doc["nodes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    v.as_str().map(String::from).or_else(|| v["id"].as_str().map(String::from))
                })
                .collect()
        })
        .unwrap_or_default();
    if node_ids.is_empty() {
        return Err(Outcome::Err(422, "the graph has no components".into()));
    }

    let mut rows = Vec::new();
    for raw in &node_ids {
        // A node may name a bare component, a tag, or a digest (see
        // `parse_component_ref`). The NAME is what finds the row; the tag or
        // digest then decides which bytes that row should be read as.
        let r = parse_component_ref(raw);
        let id = &r.name;

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
        let Some(mut row) = row else {
            return Err(Outcome::Err(
                422,
                format!("component `{id}` is unknown or not visible to you"),
            ));
        };

        // Resolve a tag to the content it names. A tag that was never recorded is
        // an error rather than a silent fall-through to `latest` — deploying
        // something other than what was asked for is worse than refusing.
        if let Some(tag) = &r.tag {
            let tagged = row["tags"][tag].as_str().map(str::to_string);
            match tagged {
                Some(sha) => {
                    row["blob_key"] = json!(format!("sha256/{sha}"));
                    row["content"] = json!(sha);
                    row["digest"] = json!("");
                }
                None => {
                    return Err(Outcome::Err(422, format!("component `{id}` has no tag `{tag}`")))
                }
            }
        }

        // A digest overrides everything, and is checked against what is actually
        // staged: a reference to bytes nobody has is a typo, and finding that out
        // at compose time is far better than at start time on a node.
        if let Some(sha) = &r.digest {
            let key = format!("sha256/{sha}");
            if blob::get(BIN, &key).is_err() {
                return Err(Outcome::Err(
                    422,
                    format!("no staged bytes for `{id}@sha256:{sha}` — nothing was ever uploaded with that content"),
                ));
            }
            row["content"] = json!(sha);
            row["blob_key"] = json!(key);
            // The digest on the row describes the CURRENT pointer's distributed
            // artifact, which is not what was asked for. Cleared so the
            // distribution step re-derives it for these bytes.
            row["digest"] = json!("");
        }
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
        req::node_config(doc["nodes"].as_array().map(|a| a.as_slice()).unwrap_or_default(), id)
    };

    // `{"id": "gate", "secrets": [{"key": "stripe", "ref": "vault://acme/stripe"}]}`
    let given_secrets = |id: &str| -> Vec<Value> {
        doc["nodes"]
            .as_array()
            .and_then(|a| {
                a.iter()
                    .find(|n| n["id"].as_str() == Some(id))
                    .and_then(|n| n["secrets"].as_array())
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
            let root = plan.roots.first().cloned().ok_or_else(|| {
                Outcome::Err(422, "no root: something is plugged into every component".into())
            })?;
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
                let Ok(bytes) = blob::get(BIN, &staged_key(row)) else {
                    return Err(Outcome::Err(
                        409,
                        format!(
                            "component `{id}` has no staged bytes to compose from — re-upload it"
                        ),
                    ));
                };
                cparts.push(composer::Part { id, bytes });
            }
            let fused = composer::compose(&cparts, &edges, &root)
                .map_err(|e| Outcome::Err(422, format!("fused: {}", compose_detail(&e))))?;

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
        composer::ComposeError::MissingPart(s) => {
            format!("a component has no bytes to compose: {s}")
        }
        composer::ComposeError::Unbuildable(s) => {
            format!("the graph cannot be composed statically: {s}")
        }
        composer::ComposeError::PlugFailed(s) => format!("wac refused the plug: {s}"),
        composer::ComposeError::EncodeFailed(s) => {
            format!("the composed graph could not be encoded: {s}")
        }
    }
}

/// A component reference, in the shape everyone already knows from registries.
///
///   `shop`                    the moving pointer — whatever was uploaded last
///   `shop:v2`                 a named pointer, which an author may move
///   `shop@sha256:<hex>`       exact bytes, which nothing can move
///
/// The idiom is worth following rather than inventing, and the field this ends up
/// in has been called `oci_ref` since long before any of this. A digest reference
/// is the one that makes a deployment reproducible: it survives somebody else
/// uploading over the name it came from, which — before content-addressed staging
/// — was not merely a stale read but a lost artifact.
pub struct ComponentRef {
    pub name: String,
    pub tag: Option<String>,
    pub digest: Option<String>,
}

fn parse_component_ref(raw: &str) -> ComponentRef {
    // `@` binds tighter than `:` — `shop:v2@sha256:x` is a digest reference that
    // happens to mention where it came from, and the digest wins, because the
    // whole point of naming one is that nothing else gets a say.
    if let Some((left, hex)) = raw.split_once("@sha256:") {
        let (name, tag) = match left.split_once(':') {
            Some((n, t)) => (n.to_string(), Some(t.to_string())),
            None => (left.to_string(), None),
        };
        return ComponentRef { name, tag, digest: Some(hex.to_string()) };
    }
    match raw.split_once(':') {
        Some((n, t)) => {
            ComponentRef { name: n.to_string(), tag: Some(t.to_string()), digest: None }
        }
        None => ComponentRef { name: raw.to_string(), tag: None, digest: None },
    }
}

/// Where a catalogue row's bytes are staged.
///
/// ASK THE ROW; do not rebuild the key. A row is a pointer at content, so
/// reconstructing `tenant/id` and reading that is assuming the name still holds
/// the bytes this row describes — which stopped being true the moment staging
/// became content-addressed, and was never safe under a concurrent upload even
/// before that.
///
/// The fallback covers rows written before content keys existed.
fn staged_key(row: &Value) -> String {
    match row["blob_key"].as_str().filter(|s| !s.is_empty()) {
        Some(k) => k.to_string(),
        None => format!(
            "{}/{}",
            row["tenant"].as_str().unwrap_or_default(),
            row["id"].as_str().unwrap_or_default()
        ),
    }
}

/// What each component in a graph exports, by name.
///
/// Recorded on every revision so the NEXT save has something to compare against.
/// Without it, "did this upgrade break anything" has no answer: the catalogue row
/// is overwritten by the upload, so by the time a save runs, what the previous
/// build exported is already gone.
fn surfaces_of(tenant: &str, parts: &[manifest::Part]) -> Value {
    let mut out = serde_json::Map::new();
    for p in parts {
        let key = format!("{tenant}/{}", p.name);
        let exports: Vec<Value> = find_one(CATALOG, "key", &key)
            .map(|(_, _, row)| row["surface"]["exports"].clone())
            .and_then(|e| e.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| e["raw"].as_str().map(|s| json!(s)))
            .collect();
        out.insert(p.name.clone(), Value::Array(exports));
    }
    Value::Object(out)
}

/// Exports that the previous revision had and this one does not.
///
/// This is the ONLY thing the WIT surface is used for, and the distinction is the
/// one that a version test found the hard way. The surface must never decide
/// whether an artifact CHANGED — two builds differing in a constant have
/// identical surfaces and different bytes, and treating them as the same shipped
/// nothing for the entire life of that bug. What the surface is genuinely good
/// for is whether a change BREAKS something: an export that vanished is an export
/// somebody was linking to, or serving on.
fn lost_exports(before: &Value, after: &Value) -> Vec<String> {
    let mut gone = Vec::new();
    let Some(old) = before.as_object() else { return gone };
    for (component, exports) in old {
        // A component that is no longer in the graph at all is a deliberate
        // removal, not a broken upgrade — the author took it out.
        let Some(now) = after.get(component).and_then(|v| v.as_array()) else { continue };
        for e in exports.as_array().cloned().unwrap_or_default() {
            let Some(raw) = e.as_str() else { continue };
            if !now.iter().any(|n| n.as_str() == Some(raw)) {
                gone.push(format!("{component} no longer exports {raw}"));
            }
        }
    }
    gone.sort();
    gone
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
        .map(|h| {
            json!({ "raw": format!("{}:{}/{}", h.namespace, h.pkg, h.name),
                         "namespace": h.namespace, "pkg": h.pkg, "name": h.name })
        })
        .collect();
    let surface = json!({
        "exports": root_row["surface"]["exports"],
        "imports": host_imports,
        "host_imports": host_imports,
        "nested_instances": plan.instance_count,
    });
    // WHAT THIS WAS COMPOSED FROM, by content.
    //
    // The surface alone cannot answer "is this still the right artifact". Two
    // builds of one component with a changed constant have IDENTICAL exports and
    // imports and completely different bytes — which is what most changes look
    // like, and certainly what an agent's changes look like. Comparing surfaces
    // meant a re-uploaded component was composed once and never again: the
    // manifest kept the first digest, the fleet kept running the first build, and
    // every layer reported success.
    //
    // The uploaded component's own content hash is what actually identifies the
    // input, so that is what is recorded and compared.
    let inputs = json!([root_row["surface"]["sha256"]]);
    match find_one(CATALOG, "key", key) {
        Some((rec, rev, mut row)) => {
            let digest = row["digest"].as_str().unwrap_or_default().to_string();
            // Re-staging the same graph must not orphan a digest that already
            // describes different bytes.
            if row["surface"] != surface || row["inputs"] != inputs {
                row["surface"] = surface;
                row["inputs"] = inputs;
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
                "surface": surface, "inputs": inputs,
                "digest": "", "generated": true, "added": now(),
            });
            let _ = records::create(
                CATALOG,
                &row.to_string(),
                &["key".to_string(), "id".to_string(), "tenant".to_string()],
            );
            String::new()
        }
    }
}

fn deployment_save(request: &IncomingRequest, id: &str, query: &Map<String, Value>) -> Outcome {
    // Removing an export is sometimes the point. An author who means it says so.
    let force = query.get("force").and_then(|v| v.as_str()).is_some_and(|v| v == "true");
    // A save is what creates desired state — it is the other way to ask the fleet
    // for work, and admitting only environment spawns would leave the front door
    // open while watching the side one.
    if let Err(refusal) = admit_one_more() {
        return refusal;
    }
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
    let owner_org =
        doc["org"].as_str().or_else(|| doc["tenant"].as_str()).unwrap_or(&p.tenant).to_string();
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

    // --- the compatibility gate ---------------------------------------------
    //
    // An upgrade that removes an export breaks whatever was linked to it, or the
    // ingress that was serving on it. Refused here, where the author is standing,
    // rather than at start time on a node — where it surfaces as a link failure
    // with no hint that an upload caused it.
    //
    // `?force=true` exists because sometimes removing an export IS the change.
    let surfaces = surfaces_of(&owner_org, &parts);
    if !force {
        if let Some(prev) = newest_revision(id) {
            let gone = lost_exports(&prev["surfaces"], &surfaces);
            if !gone.is_empty() {
                return Outcome::Err(
                    409,
                    format!(
                        "this upgrade removes {} export(s) that revision {} had: {}. Anything \
                         linked to them would fail to start. Re-deploy with `?force=true` if \
                         that is the intent.",
                        gone.len(),
                        prev["revision"].as_u64().unwrap_or(0),
                        gone.join("; ")
                    ),
                );
            }
        }
    }

    // A revision is the unit of rollback: the desired state, verbatim (ADR-0004).
    let next = doc["revision"].as_u64().unwrap_or(0) + 1;
    let revision_doc = json!({
        "deployment": id, "tenant": owner_org, "revision": next,
        "strategy": strategy.as_str(), "manifest": doc_manifest,
        "org": owner_org, "saved": now(), "env": manifest::env_for(&owner_org, &name),
        // What this revision's components export, so the NEXT save can tell an
        // upgrade from a break.
        "surfaces": surfaces,
    });
    let _ = records::create(
        REVISIONS,
        &revision_doc.to_string(),
        &["deployment".to_string(), "tenant".to_string()],
    );

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

/// Every record in a collection, following the cursor.
///
/// `list_records` takes a limit and returns a cursor, and a single call with a
/// big-looking number is a SILENT truncation: the caller gets a plausible answer
/// with the tail missing and no indication that anything was dropped.
///
/// That is not hypothetical. Desired state was read with a flat limit of 1000,
/// and a stress run that grew 3906 environments watched the fleet flatline at
/// exactly 500 running — 1000 revision records, deduplicated to the newest per
/// deployment, is about 500 apps. Every environment past the cap was created,
/// reported as created, and never placed, with nothing anywhere saying so.
///
/// `cap` is a real backstop rather than a silent one: reaching it is reported.
fn all_records(collection: &str, cap: usize) -> Vec<records::Entry> {
    const PAGE: u32 = 500;
    let mut out = Vec::new();
    let mut after = String::new();
    loop {
        let Ok(page) = records::list_records(collection, PAGE, &after) else { break };
        let empty = page.entries.is_empty();
        out.extend(page.entries);
        if out.len() >= cap {
            eprintln!(
                "platform: {collection} has at least {} records and this read stops at {cap} — \
                 the tail is NOT being served, which for desired state means apps that were \
                 accepted and will never start",
                out.len()
            );
            out.truncate(cap);
            break;
        }
        if page.next.is_empty() || empty {
            break;
        }
        after = page.next;
    }
    out
}

fn internal_revisions(request: &IncomingRequest) -> Outcome {
    if !internal_ok(request) {
        return Outcome::Err(401, "internal endpoint".into());
    }
    let mut current: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    // Paged, not capped at 1000. See `all_records`: the flat limit silently hid
    // every deployment past roughly the five-hundredth, which is how a fleet
    // asked for 3906 apps sat at 500 forever.
    for e in all_records(REVISIONS, 100_000) {
        if let Ok(v) = serde_json::from_str::<Value>(&e.data) {
            let key = v["deployment"].as_str().unwrap_or_default().to_string();
            let better = current
                .get(&key)
                .map(|old| {
                    v["revision"].as_u64().unwrap_or(0) > old["revision"].as_u64().unwrap_or(0)
                })
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
fn deployment_delete(request: &IncomingRequest, id: &str, query: &Map<String, Value>) -> Outcome {
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

    for e in all_records(REVISIONS, 100_000) {
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

use guestfmt::percent_decode;

fn body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let raw = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

/// The most a request body may be, before the component stops reading it.
///
/// There was no ceiling anywhere: 148 of 150 components accumulated whatever
/// arrived until the guest hit wasmtime's 64 MiB per-store memory cap and TRAPPED,
/// which reaches the caller as a closed connection saying nothing about a size.
/// A component that answers JSON has no business reading sixteen megabytes, and
/// the ones that legitimately handle uploads police it themselves with a 413 and a
/// granted max-size — those are left alone.
///
/// Generous on purpose. This is a backstop against an unbounded read, not a
/// content policy; an API that needs a real limit should state its own and say 413.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let b = request.consume().map_err(|_| ())?;
    let stream = b.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(65536) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                // A ceiling, not a policy: past this the read stops and the caller
                // is told, rather than growing until the store's memory cap traps
                // the component and the connection just closes.
                if buf.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(());
                }
                buf.extend_from_slice(&chunk);
            }
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(_) => return Err(()),
        }
    }
    Ok(buf)
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    let (code, ctype, body) = match result {
        Outcome::Json(c, b) => (c, "application/json".to_string(), b.into_bytes()),
        Outcome::Bytes(c, ct, b) => (c, ct, b),
        Outcome::Err(c, m) => {
            (c, "application/json".to_string(), json!({ "error": m }).to_string().into_bytes())
        }
        Outcome::Structured(c, v) => {
            (c, "application/json".to_string(), v.to_string().into_bytes())
        }
    };
    let headers = Fields::new();
    let _ = headers.set("content-type", &[ctype.into_bytes()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(code);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        let _ = write_all(&stream, &body);
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
        let err = check_config(
            "gate",
            &row(&[("grace-period-secs", false)]),
            &given(&[("grace-period-sec", "5")]),
        )
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

// ---- projects and goals (ADR-0082) -----------------------------------------

const PROJECTS: &str = "projects";
const GOALS: &str = "goals";

/// The goal lifecycle, as the only legal transitions.
///
/// A table rather than scattered `if state == …` checks, because the illegal
/// moves are the interesting ones: nothing may leave `failed` (a requeue makes a
/// NEW goal, so what was tried stays visible), and nothing may reach `done`
/// without having run.
fn goal_may(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("queued", "running")
            | ("queued", "abandoned")
            | ("running", "awaiting-human")
            | ("running", "failed")
            | ("running", "abandoned")
            | ("awaiting-human", "done")
            | ("awaiting-human", "failed")
            | ("awaiting-human", "abandoned")
    )
}

/// A project name that is safe as part of a store name and a branch name.
fn valid_project_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

fn project_create(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Member) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    let b: req::NewProject = match read_body(request)
        .map_err(|_| Outcome::Err(400, "could not read body".into()))
        .and_then(|raw| req::parse(&raw))
    {
        Ok(v) => v,
        Err(o) => return o,
    };
    if !valid_project_name(&b.name) {
        return Outcome::Err(
            422,
            "name must be 1-40 chars of [a-z0-9-], not starting or ending with -".into(),
        );
    }
    // `owner/name`, checked here rather than at the first forge call, where the
    // answer is a 404 that reads like "the repository does not exist".
    if b.repo.split('/').filter(|s| !s.is_empty()).count() != 2 {
        return Outcome::Err(422, format!("repo must be \"owner/name\", got {:?}", b.repo));
    }
    if projects_of(&org).iter().any(|d| str_of(d, "name") == b.name) {
        return Outcome::Err(409, format!("project `{}` already exists", b.name));
    }

    let doc = json!({
        "name": b.name, "org": org, "repo": b.repo,
        "base": b.base.unwrap_or_else(|| "main".into()),
        "forge_token_ref": b.forge_token_ref.unwrap_or_default(),
        "llm_key_ref": b.llm_key_ref.unwrap_or_default(),
        "budget": b.budget.unwrap_or(0),
        // One at a time, which is the whole answer to concurrent pull requests
        // (ADR-0082). Raising it is what makes that a problem worth solving.
        "max_concurrent_runs": 1,
        "created": now(),
    });
    match records::create(PROJECTS, &doc.to_string(), &["org".to_string()]) {
        Ok(e) => Outcome::Json(
            201,
            json!({ "id": e.id, "name": doc["name"], "repo": doc["repo"], "base": doc["base"] })
                .to_string(),
        ),
        Err(e) => Outcome::Err(500, format!("recording the project: {e:?}")),
    }
}

fn projects_of(org: &str) -> Vec<Value> {
    records::find_by(PROJECTS, "org", &json!(org).to_string())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .collect()
}

fn projects_list(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Viewer) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    let rows: Vec<Value> = projects_of(&org)
        .into_iter()
        .map(|d| {
            let name = str_of(&d, "name");
            let goals = goals_of(&name);
            json!({
                "name": name, "repo": d["repo"], "base": d["base"],
                "queued": goals.iter().filter(|g| str_of(g, "state") == "queued").count(),
                "running": goals.iter().filter(|g| str_of(g, "state") == "running").count(),
                "failed": goals.iter().filter(|g| str_of(g, "state") == "failed").count(),
            })
        })
        .collect();
    Outcome::Json(200, json!({ "count": rows.len(), "projects": rows }).to_string())
}

fn goals_of(project: &str) -> Vec<Value> {
    records::find_by(GOALS, "project", &json!(project).to_string())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| {
            serde_json::from_str::<Value>(&e.data).ok().map(|mut v| {
                v["id"] = json!(e.id);
                v
            })
        })
        .collect()
}

fn goal_create(request: &IncomingRequest, project: &str, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Member) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    if !projects_of(&org).iter().any(|d| str_of(d, "name") == project) {
        return Outcome::Err(404, format!("no project `{project}`"));
    }
    let b: req::NewGoal = match read_body(request)
        .map_err(|_| Outcome::Err(400, "could not read body".into()))
        .and_then(|raw| req::parse(&raw))
    {
        Ok(v) => v,
        Err(o) => return o,
    };
    if b.title.trim().is_empty() {
        return Outcome::Err(422, "a goal needs a title".into());
    }
    // A sub-goal's parent is checked HERE, where the answer is a 422 naming the
    // problem. Stored unchecked, the first thing to notice would be a worklist
    // with a child under a parent in another project, or a chain that never ends.
    let parent = b.parent.unwrap_or_default();
    if !parent.is_empty() {
        if let Err((code, msg)) = parent_is_usable(&parent, project) {
            return Outcome::Err(code, msg);
        }
    }
    let doc = json!({
        "project": project, "org": org,
        "title": b.title.trim(),
        "spec": b.spec.unwrap_or_default(),
        "priority": b.priority.unwrap_or(100),
        // Empty when this is a goal in its own right, which is most of them. A
        // field rather than a separate table: a sub-goal IS a goal — same
        // lifecycle, same queue, same "a human starts it" — and giving it its own
        // table would mean two of every query that reads a worklist.
        "parent": parent,
        // Queued, and it stays there. A human starts every goal (ADR-0082): there
        // is no loop that drains this, on purpose.
        "state": "queued",
        "created": now(),
    });
    match records::create(GOALS, &doc.to_string(), &["project".to_string(), "org".to_string()]) {
        Ok(e) => Outcome::Json(
            201,
            json!({ "id": e.id, "project": project, "state": "queued", "title": doc["title"] })
                .to_string(),
        ),
        Err(e) => Outcome::Err(500, format!("recording the goal: {e:?}")),
    }
}

/// How deep a decomposition may go.
///
/// A bound rather than a promise that cycles cannot happen: the walk below
/// catches an actual cycle, and this catches the chain that is technically a tree
/// and still means nobody will ever run the leaves.
const MAX_GOAL_DEPTH: usize = 8;

/// May this goal be a parent, for a child in `project`?
///
/// Three refusals, all of them 422 because each is a mistake in the request that
/// the caller can fix:
///
///   * no such goal — usually an id from another environment;
///   * a different project — a worklist is per project, and a child under a
///     parent nobody in that project can see is a row that reads as corrupt;
///   * too deep, or a cycle — walked rather than assumed, because the id is the
///     caller's and a chain that closes on itself would hang every reader of it.
fn parent_is_usable(parent: &str, project: &str) -> Result<(), (u16, String)> {
    let Ok(entry) = records::get(GOALS, parent) else {
        return Err((422, format!("no goal `{parent}` to be a part of")));
    };
    let Ok(doc) = serde_json::from_str::<Value>(&entry.data) else {
        return Err((500, "the parent goal's record is unreadable".into()));
    };
    if str_of(&doc, "project") != project {
        return Err((
            422,
            format!(
                "goal `{parent}` belongs to project `{}`, not `{project}` — a part and the                  goal it serves live in one worklist",
                str_of(&doc, "project")
            ),
        ));
    }
    // Walk up. `seen` is what turns an infinite loop into a message: a record
    // written before this check existed, or edited around it, can still close a
    // cycle, and the reader must not be the thing that discovers it.
    let mut seen = vec![parent.to_string()];
    let mut at = str_of(&doc, "parent");
    while !at.is_empty() {
        if seen.contains(&at) {
            return Err((422, format!("goal `{parent}` is already part of a cycle through `{at}`")));
        }
        seen.push(at.clone());
        // A runaway walk is bounded here as well as by the check below, because
        // the two protect different things: this stops the LOOP, that stops the
        // GOAL. A chain longer than the bound is refused either way.
        if seen.len() > MAX_GOAL_DEPTH {
            break;
        }
        let Ok(up) = records::get(GOALS, &at) else { break };
        let Ok(updoc) = serde_json::from_str::<Value>(&up.data) else { break };
        at = str_of(&updoc, "parent");
    }
    // `seen` is the PARENT's own chain, so a child hung off it sits one deeper.
    // Checked after the walk rather than during it: the question is how deep the
    // NEW goal would be, and that is not known until the walk is done.
    if seen.len() >= MAX_GOAL_DEPTH {
        return Err((
            422,
            format!(
                "goal `{parent}` is already {} levels deep, and {MAX_GOAL_DEPTH} is the bound \u{2014} a decomposition this deep is one nobody will reach the bottom of",
                seen.len()
            ),
        ));
    }
    Ok(())
}

fn goals_list(request: &IncomingRequest, project: &str, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    if let Err((code, msg)) = orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Viewer)
    {
        return Outcome::Err(code, msg);
    }
    let want = query.get("state").and_then(|v| v.as_str()).unwrap_or_default();
    // `?parent=<id>` lists one goal's parts; `?parent=` (empty, explicitly given)
    // lists only goals that are nobody's part, which is the top-level worklist.
    // Absent means every goal, which is what every existing caller asks for.
    let by_parent = query.get("parent").and_then(|v| v.as_str());
    let mut rows: Vec<Value> = goals_of(project)
        .into_iter()
        .filter(|g| want.is_empty() || str_of(g, "state") == want)
        .filter(|g| by_parent.is_none_or(|p| str_of(g, "parent") == p))
        .collect();
    // Priority first, then oldest — a worklist someone reads top-down.
    rows.sort_by(|a, b| {
        a["priority"]
            .as_i64()
            .unwrap_or(100)
            .cmp(&b["priority"].as_i64().unwrap_or(100))
            .then(str_of(a, "created").cmp(&str_of(b, "created")))
    });
    Outcome::Json(200, json!({ "count": rows.len(), "goals": rows }).to_string())
}

/// Move a goal, refusing anything the lifecycle does not allow.
fn goal_transition(
    request: &IncomingRequest,
    id: &str,
    to: &str,
    query: &Map<String, Value>,
) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    if let Err((code, msg)) = orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Member)
    {
        return Outcome::Err(code, msg);
    }
    let Ok(entry) = records::get(GOALS, id) else {
        return Outcome::Err(404, format!("no goal `{id}`"));
    };
    let Ok(mut doc) = serde_json::from_str::<Value>(&entry.data) else {
        return Outcome::Err(500, "the goal record is unreadable".into());
    };
    // A goal with live parts may not be finished or thrown away out from under
    // them. Both leave rows nobody will ever look at again: parts of a goal that
    // is done read as already handled, and parts of an abandoned one read as
    // work still to do for a goal that no longer exists.
    //
    // Refused rather than cascaded. Cascading is a destructive multi-record
    // operation inferred from one click, and the caller has more information than
    // this function does about whether the parts are worth keeping.
    if matches!(to, "done" | "abandoned") {
        let live: Vec<String> = goals_of(&str_of(&doc, "project"))
            .into_iter()
            .filter(|g| str_of(g, "parent") == id)
            .filter(|g| !matches!(str_of(g, "state").as_str(), "done" | "abandoned" | "failed"))
            .map(|g| str_of(&g, "title"))
            .collect();
        if !live.is_empty() {
            return Outcome::Err(
                409,
                format!(
                    "goal `{id}` still has {} unfinished part(s) — {}. Finish or abandon them                      first: parts of a `{to}` goal are rows nobody looks at again.",
                    live.len(),
                    live.join(", ")
                ),
            );
        }
    }

    let from = str_of(&doc, "state");
    if !goal_may(&from, to) {
        // Naming both ends beats "invalid transition": the caller usually has the
        // wrong idea about where the goal currently IS.
        return Outcome::Err(409, format!("a goal cannot go from `{from}` to `{to}`"));
    }

    doc["state"] = json!(to);
    match to {
        "running" => {
            doc["started"] = json!(now());
            // A goal is FROZEN once it starts (ADR-0081): the spec it was judged
            // against must not change under a run. Editing a running goal forks
            // it into a new one instead.
            doc["frozen_spec"] = doc["spec"].clone();
        }
        "failed" => {
            let reason = read_body(request)
                .ok()
                .and_then(|raw| req::parse::<req::FailGoal>(&raw).ok())
                .map(|b| b.reason)
                .unwrap_or_else(|| "no reason given".into());
            doc["reason"] = json!(reason);
            doc["failed_at"] = json!(now());
        }
        "done" => doc["finished"] = json!(now()),
        _ => {}
    }

    // Guarded on the revision we READ, so two people starting the same goal at the
    // same moment cannot both win. Without it the second write silently overwrites
    // the first and two runs believe they own one goal — which, with one run per
    // project, is exactly the case this design exists to prevent.
    match records::update(GOALS, id, &doc.to_string(), entry.revision) {
        Err(records::StoreError::RevisionConflict(_)) => {
            Outcome::Err(409, format!("`{id}` moved while you were looking at it — read it again"))
        }
        Ok(_) => Outcome::Json(
            200,
            json!({ "id": id, "from": from, "state": to, "title": doc["title"] }).to_string(),
        ),
        Err(e) => Outcome::Err(500, format!("moving the goal: {e:?}")),
    }
}

// ---- fleet status and admission control ------------------------------------

const FLEET: &str = "fleet";

/// Where the reconciler's last report is kept. One row, overwritten.
const FLEET_ROW: &str = "status";

/// The reconciler POSTs here every pass. Until now nothing received it: the
/// endpoint did not exist, and the reconciler's `let _ = …send()` swallowed the
/// 404, so `unschedulable` and `at_ceiling` had been reported into the void.
fn internal_status_put(request: &IncomingRequest) -> Outcome {
    if !internal_ok(request) {
        return Outcome::Err(401, "internal endpoint".into());
    }
    let Ok(raw) = read_body(request) else {
        return Outcome::Err(400, "could not read body".into());
    };
    let Ok(mut body) = serde_json::from_slice::<Value>(&raw) else {
        return Outcome::Err(400, "status must be JSON".into());
    };
    body["at"] = json!(now());
    // A new report accounts for everything admitted before it, so the running
    // count starts again from zero.
    body["admitted"] = json!(0);

    // One row, replaced. History would be a metrics system's job, and keeping it
    // here would grow without bound in the collection the admission check reads
    // on every spawn.
    let existing = records::find_by(FLEET, "row", &json!(FLEET_ROW).to_string())
        .unwrap_or_default()
        .into_iter()
        .next();
    body["row"] = json!(FLEET_ROW);
    let stored = match existing {
        Some(e) => records::update(FLEET, &e.id, &body.to_string(), e.revision).map(|_| ()),
        None => records::create(FLEET, &body.to_string(), &["row".to_string()]).map(|_| ()),
    };
    match stored {
        Ok(()) => Outcome::Json(200, json!({ "recorded": true }).to_string()),
        Err(e) => Outcome::Err(500, format!("recording fleet status: {e:?}")),
    }
}

/// How many spawns have been admitted since the last fleet report.
///
/// Without this, admission is only as fresh as the last report — and a burst
/// faster than the reporting interval sails straight through. A stress run fired
/// 625 spawns in 0.2 seconds against a limit of 200 and every one was admitted,
/// because the newest lag the platform had was seconds old and said the fleet was
/// nearly caught up.
///
/// Counting what has been let through since that number arrived turns a stale
/// figure into a usable estimate: the fleet is at least this far behind, because
/// this much was added after it last spoke.
fn admitted_since_report() -> u64 {
    records::find_by(FLEET, "row", &json!(FLEET_ROW).to_string())
        .unwrap_or_default()
        .into_iter()
        .next()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok())
        .and_then(|v| v["admitted"].as_u64())
        .unwrap_or(0)
}

/// Record one more admission against the current report.
fn note_admission() {
    let Some(e) = records::find_by(FLEET, "row", &json!(FLEET_ROW).to_string())
        .unwrap_or_default()
        .into_iter()
        .next()
    else {
        return;
    };
    let Ok(mut doc) = serde_json::from_str::<Value>(&e.data) else { return };
    doc["admitted"] = json!(doc["admitted"].as_u64().unwrap_or(0) + 1);
    // Guarded on the revision, so a burst of concurrent admissions cannot lose
    // counts — which is the case this exists for.
    let _ = records::update(FLEET, &e.id, &doc.to_string(), e.revision);
}

/// How far behind the fleet is, how old that number is, and how many nodes it
/// was measured across.
fn fleet_lag() -> Option<(u64, u64, u64)> {
    let row = records::find_by(FLEET, "row", &json!(FLEET_ROW).to_string())
        .ok()?
        .into_iter()
        .next()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok())?;
    let lag = row["lag"].as_u64().unwrap_or(0);
    let at = row["at"].as_u64().unwrap_or(0);
    let age = now().saturating_sub(at);
    // A fleet with no nodes still counts as one, so the limit never collapses to
    // zero and refuses everything the moment inventory blinks.
    let nodes = row["nodes"].as_u64().unwrap_or(1).max(1);
    Some((lag, age, nodes))
}

fn cfg_u64(key: &str, default: u64) -> u64 {
    config::get(key).ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// May the fleet be asked to run one more thing?
///
/// Admission belongs HERE and not in the reconciler: the loop's job is to place
/// what it is told as fast as it can, and a loop that also decided whether work
/// should exist would be judging its own backlog.
///
/// Refusing beats queueing. A queue that grows while nothing drains it is the
/// same bug one level up, and a caller told "not now" can back off — which under
/// ADR-0082 is a person, who can simply try later.
fn admit_one_more() -> Result<(), Outcome> {
    // The limit is PER NODE, multiplied by the nodes actually carrying work.
    //
    // A flat number is wrong everywhere except where it was measured: 200
    // outstanding is a reasonable backlog on three nodes and absurd on one, and a
    // fleet that grows would keep an admission limit sized for the fleet it used
    // to be. `max-placement-lag` still overrides, for an operator who wants a
    // hard cap regardless of size.
    let per_node = cfg_u64("max-placement-lag-per-node", 70);
    let flat = cfg_u64("max-placement-lag", 0);
    if flat == 0 && per_node == 0 {
        return Ok(()); // explicitly disabled
    }
    let stale_after = cfg_u64("status-max-age", 90);

    let Some((lag, age, nodes)) = fleet_lag() else {
        // Nothing has ever reported. Allowed: a platform that refused every
        // spawn until a reconciler had spoken would be unusable on a fresh
        // install, and there is no backlog to protect against yet either.
        return Ok(());
    };

    // FAIL CLOSED on a stale report. If the loop has stopped, accepting more work
    // is pointless — nothing will place it — and failing open here would mean
    // unbounded acceptance at exactly the moment nothing is being done.
    if age > stale_after {
        return Err(Outcome::Err(
            503,
            format!(
                "the reconciler has not reported for {age}s (stale after {stale_after}s) — \
                 nothing is placing work, so nothing new is accepted"
            ),
        ));
    }
    let limit = if flat > 0 { flat } else { per_node.saturating_mul(nodes) };

    // Everything let through since that number arrived counts against it too.
    let pending = admitted_since_report();
    let effective = lag + pending;
    if effective > limit {
        return Err(Outcome::Err(
            429,
            format!(
                "the fleet is {lag} instance(s) behind, {pending} more were accepted since it \
                 last reported, and the limit is {limit} across {nodes} node(s) — this would \
                 be accepted and never placed. Try again once it has caught up."
            ),
        ));
    }
    note_admission();
    Ok(())
}

#[cfg(test)]
mod ref_tests {
    use super::parse_component_ref as parse_ref;

    /// The registry idiom, followed rather than reinvented.
    #[test]
    fn a_bare_name_is_the_moving_pointer() {
        let r = parse_ref("shop");
        assert_eq!(r.name, "shop");
        assert!(r.tag.is_none() && r.digest.is_none());
    }

    #[test]
    fn a_tag_is_a_pointer_an_author_may_move() {
        let r = parse_ref("shop:v2");
        assert_eq!((r.name.as_str(), r.tag.as_deref()), ("shop", Some("v2")));
        assert!(r.digest.is_none());
    }

    /// The one that makes a deployment reproducible.
    #[test]
    fn a_digest_names_bytes_nothing_can_move() {
        let r = parse_ref("shop@sha256:abc123");
        assert_eq!(r.name, "shop");
        assert_eq!(r.digest.as_deref(), Some("abc123"));
    }

    /// A digest wins over a tag in the same reference. Naming exact bytes is a
    /// statement that nothing else gets a say — including the tag beside it,
    /// which may since have moved somewhere else entirely.
    #[test]
    fn a_digest_beats_the_tag_beside_it() {
        let r = parse_ref("shop:v2@sha256:abc123");
        assert_eq!(r.name, "shop");
        assert_eq!(r.tag.as_deref(), Some("v2"), "the tag is kept, for the record");
        assert_eq!(r.digest.as_deref(), Some("abc123"), "but the digest decides");
    }

    /// A name with no tag and no digest must not be mangled by a stray colon in
    /// something that is not a reference at all.
    #[test]
    fn a_name_that_looks_like_a_url_is_still_a_name() {
        let r = parse_ref("shop:8080");
        assert_eq!((r.name.as_str(), r.tag.as_deref()), ("shop", Some("8080")));
    }
}
