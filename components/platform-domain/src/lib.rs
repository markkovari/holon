//! `platform-domain` — the platform control plane as ONE composed wasm HTTP component.
//! native reconciler makes the lattice match (ADR-0022); it holds no lattice credential (ADR-0003).

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../audit-log/wit",
            "../../wit/deps/ratelimit-guard",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../policy-guard/wit",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../event-bus/wit",
            "../blob-store/wit",
            "../quota/wit",
            "../wit-reflect/wit",
            "../secrets-vault/wit",
            "wit",
        ],
        world: "platform:app/platform-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}
mod goals;
mod manifest;
mod orgs;
mod req;
mod secrets;

use serde_json::{json, Map, Value};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::types as auth_types;
use bindings::blob::store::blobstore as blob;
use bindings::comp::store::cas;
use bindings::event::bus::bus;
use bindings::p3::clocks::system_clock;
use bindings::policy::guard::guard as policy;
use bindings::quota::meter::meter as quota;
use bindings::records::store::store as records;
use bindings::wasi::config::store as config;
use bindings::wasi::keyvalue::store as kv;
use bindings::wit::reflect::composer;
use bindings::wit::reflect::inspector;

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;

use manifest::{HostIface, ManifestInput, Part, Plan, Strategy};

guestio::guest_p3_bearer!();
guestio::guest_p3_respond!();

struct Component;

/// Reading a p3 body consumes the request, so headers, bearer and body are taken before routing.
pub(crate) struct Incoming {
    headers: Fields,
    bearer: Option<String>,
    body: Result<Vec<u8>, ()>,
}

impl Incoming {
    pub(crate) fn headers(&self) -> &Fields {
        &self.headers
    }
}

const ACCOUNTS: &str = "tenants";
const CATALOG: &str = "catalog";
/// One row per key, indexed by org (ADR-0073).
const ORGKEYS: &str = "orgkeys";
const DEPLOYMENTS: &str = "deployments";
const REVISIONS: &str = "revisions";
const BIN: &str = "wasm";
/// Long enough to hand over, short enough that a leaked one is usually already dead.
const INVITE_TTL: u64 = 7 * 24 * 3600;
/// `quota:meter` takes the limit as a parameter: the platform is the entitlement store.
const DEPLOYMENT_BUDGET: u64 = 5;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let request = Incoming {
            // A deep copy: the request resource is gone once its body is consumed.
            headers: request.get_headers().clone(),
            bearer: bearer(&request),
            body: read_whole_body(request).await,
        };
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

            (Method::Post, ["api", "internal", "fetch-token"]) => {
                secrets::fetch_token_mint(&request)
            }
            (Method::Get, ["api", "internal", "secret"]) => secrets::secret_fetch(&request, &query),

            (Method::Post, ["api", "secrets"]) => secrets::secret_put(&request, &query),
            (Method::Get, ["api", "secrets"]) => secrets::secrets_list(&request, &query),
            (Method::Delete, ["api", "secrets", name]) => {
                secrets::secret_delete(&request, name, &query)
            }

            (Method::Post, ["api", "projects"]) => goals::project_create(&request, &query),
            (Method::Get, ["api", "projects"]) => goals::projects_list(&request, &query),
            (Method::Post, ["api", "projects", project, "goals"]) => {
                goals::goal_create(&request, project, &query)
            }
            (Method::Get, ["api", "projects", project, "goals"]) => {
                goals::goals_list(&request, project, &query)
            }
            // Lossy-hint fast path over the durable log (ADR-0082); `goals_list` is always the fallback.
            (Method::Get, ["api", "projects", project, "events"]) => {
                goals::events_list(&request, project, &query)
            }
            (Method::Post, ["api", "projects", project, "events", "ack"]) => {
                goals::events_ack(&request, project, &query)
            }
            // A human starts every goal; there is no loop that does (ADR-0082).
            (Method::Post, ["api", "goals", id, "start"]) => {
                goals::goal_transition(&request, id, "running", &query)
            }
            (Method::Post, ["api", "goals", id, "fail"]) => {
                goals::goal_transition(&request, id, "failed", &query)
            }
            (Method::Post, ["api", "goals", id, "done"]) => {
                goals::goal_transition(&request, id, "done", &query)
            }
            (Method::Post, ["api", "goals", id, "review"]) => {
                goals::goal_transition(&request, id, "awaiting-human", &query)
            }
            (Method::Delete, ["api", "goals", id]) => {
                goals::goal_transition(&request, id, "abandoned", &query)
            }

            (Method::Post, ["api", "deployments"]) => deployment_create(&request, &query),
            (Method::Get, ["api", "deployments"]) => deployments_list(&request),
            (Method::Get, ["api", "deployments", id]) => deployment_get(&request, id),
            (Method::Post, ["api", "deployments", id, "save"]) => {
                deployment_save(&request, id, &query)
            }
            (Method::Get, ["api", "deployments", id, "manifests"]) => manifests(&request, id),
            (Method::Delete, ["api", "deployments", id]) => deployment_delete(&request, id, &query),

            (Method::Get, ["api", "internal", "revisions"]) => internal_revisions(&request),
            (Method::Post, ["api", "internal", "status"]) => internal_status_put(&request),
            // The push path (ADR-0017): the applier asks, fetches, pushes, then reports back.
            (Method::Get, ["api", "internal", "pending-pushes"]) => internal_pending(&request),
            (Method::Get, ["api", "internal", "artifact"]) => internal_artifact(&request, &query),
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
        emit(outcome)
    }
}

enum Outcome {
    Json(u16, String),
    Bytes(u16, String, Vec<u8>),
    Err(u16, String),
    /// A client that must act on the failure (highlight a port, offer a component) needs the parts named.
    Structured(u16, Value),
}

pub(crate) fn now() -> u64 {
    system_clock::now().seconds as u64
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

/// Keeps sign-up usable without inventing an org UI.
fn tenant_of_email(email: &str) -> String {
    let local = email.split('@').next().unwrap_or("tenant");
    let s: String = local
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    s.trim_matches('-').to_string()
}

fn register(request: &Incoming) -> Outcome {
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
            // First account in a tenant owns it.
            if find_one(ACCOUNTS, "tenant", &tenant).is_none() {
                let doc = json!({
                    "tenant": tenant, "owner": p.subject, "created": now(),
                    "plan": { "replicas": 1, "pool_size": 8, "max_invocations": 200,
                              "max_deployments": DEPLOYMENT_BUDGET, "egress": [],
                              "constraints": {} },
                });
                let _ = records::create(ACCOUNTS, &doc.to_string(), &["tenant".to_string()]);
            }
            // So every account always has an org to deploy into.
            if orgs::role_of(&p.subject, &tenant).is_none() {
                let role = match find_one(orgs::ORGS, "id", &tenant) {
                    None => {
                        let _ = orgs::create(&tenant, &p.subject, &email);
                        orgs::Role::Owner
                    }
                    // Not joined by guessing its tenant name: registration would bypass access control.
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

fn login(request: &Incoming) -> Outcome {
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

/// Roles come from the RBAC store, never from the token.
fn caller(request: &Incoming) -> Option<auth_types::Principal> {
    authorizer::introspect(request.bearer.as_deref()?).ok()
}

fn me(request: &Incoming) -> Outcome {
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

fn visibility_of(v: &Value) -> &str {
    v["visibility"].as_str().unwrap_or("private")
}

/// A `policy:guard` decision (ADR-0007/0009).
fn may_use(p: &auth_types::Principal, row: &Value) -> bool {
    let attrs = |pairs: &[(&str, &str)]| -> Vec<policy::Attr> {
        pairs
            .iter()
            .map(|(k, v)| policy::Attr { key: (*k).to_string(), value: (*v).to_string() })
            .collect()
    };
    // A pre-org row belongs to its uploader's personal org; guessing anything else would widen who sees it.
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

    if decide("") {
        return true;
    }
    if visibility_of(row) != "org" {
        return false;
    }
    // `policy:guard` compares single values, so it is asked once per membership (ADR-0031).
    orgs::memberships(&p.subject)
        .iter()
        .filter_map(|m| m["id"].as_str().map(String::from))
        .any(|org| org == row_org && decide(&org))
}

/// ADR-0007's visibility table, registered idempotently.
fn seed_policy() {
    let cond = |left: &str, op: policy::Op, right: &str| policy::Condition {
        left: left.to_string(),
        op,
        right: right.to_string(),
    };
    let rules = vec![
        policy::Rule {
            id: "catalog-own".into(),
            action: "use".into(),
            effect: policy::Effect::Allow,
            priority: 10,
            conditions: vec![cond("principal.tenant", policy::Op::Eq, "resource.tenant")],
        },
        // ADR-0007's middle row: ranks between own (10) and public (5).
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

/// A component that declared nothing accepts nothing: deny by omission.
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

/// wac's subtype check on the real bytes, not a name comparison (version, fields, methods can differ).
/// Lists every interface the plug would satisfy: `wac plug` matches EVERY common one.
fn components_satisfies(request: &Incoming) -> Outcome {
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

    // Same visibility rule as a deploy, so asking about a fit cannot reveal a private component.
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
                // An empty list is the surprising answer, so it is said out loud.
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

fn component_add(request: &Incoming, query: &Map<String, Value>) -> Outcome {
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

    // Staged by content, not by name: `tenant/id` is a mutable pointer, and racing workers lost bytes under it.
    let content = surface.sha256.clone();
    let blob_key = format!("sha256/{content}");
    if blob::put(BIN, &blob_key, &bytes, "application/wasm").is_err() {
        return Outcome::Err(500, "could not stage the component bytes".into());
    }
    // Only the uploader can know; a trailing `!` marks a key required.
    let config_keys = declared_config(query);
    // Membership is checked before the row exists.
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Member) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    let existing = find_one(CATALOG, "key", &key);

    // Tags accumulate: moving one must not drop the others, or their still-staged bytes are stranded.
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
        "content": content,
        "blob_key": blob_key,
        // name -> content, merged: tagging a new build must not forget where old tags point.
        "tags": tags,
        // Empty until the push step records a digest; a deployment cannot render without one (ADR-0006).
        "oci_ref": "",
        "surface": surface_json(&surface),
    });
    // Same bytes is a no-op: a retried build would otherwise clear the digest and force a redistribution.
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
        // Revision-guarded: two workers moving one name is what content addressing does NOT solve.
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

fn components_list(request: &Incoming) -> Outcome {
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
                // Null on rows uploaded before declarations existed: "declared nothing".
                "config_keys": row["config_keys"],
            })
        })
        .collect();
    Outcome::Json(200, json!({ "components": rows }).to_string())
}

/// `?iface=` matches an EXPORT ("who can fill this gap"). Linear scan, no index.
fn market_search(request: &Incoming, query: &Map<String, Value>) -> Outcome {
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
        // Visibility first: filtering afterwards would leak a private component through the result count.
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
                // ADR-0007 rule 5: public means the provenance is public too.
                "signed_by": r["signed_by"], "signed_at": r["signed_at"],
                "deployable": r["digest"].as_str().unwrap_or("").starts_with("sha256:"),
                "config_keys": r["config_keys"],
                "surface": r["surface"],
            })
        })
        .collect();
    Outcome::Json(200, json!({ "components": rows, "count": rows.len() }).to_string())
}

fn component_publish(request: &Incoming) -> Outcome {
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
                        // Bound to the digest: a later push demotes the row, so new bytes never inherit a signature (ADR-0007 rule 1).
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
            // Only overwritten when given: a visibility change must not erase a description.
            if let Some(d) = b["description"].as_str() {
                row["description"] = json!(d);
            }
            // ADR-0007: deprecation, never deletion; a deployed component must keep resolving.
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

/// Derived, not a queue: "needs pushing" IS "has no oci_ref", so lost messages desynchronise nothing.
fn internal_pending(request: &Incoming) -> Outcome {
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
            // The content key, so the distributor fetches the bytes this row was created from; composed artifacts fall back to the name.
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

/// A pull, not a push: avoids the outgoing-body handshake and matches the applier's polling.
fn internal_artifact(request: &Incoming, query: &Map<String, Value>) -> Outcome {
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

fn internal_pushed(request: &Incoming) -> Outcome {
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
    // Staged location first: matching the name index alone left content-staged parts 404ing forever.
    let found = find_one(CATALOG, "key", &key).or_else(|| {
        all_records(CATALOG, 100_000).into_iter().find_map(|e| {
            let row = serde_json::from_str::<Value>(&e.data).ok()?;
            (staged_key(&row) == key).then(|| (e.id.clone(), e.revision, row))
        })
    });
    match found {
        Some((rec, rev, mut row)) => {
            // Bare content address (ADR-0024); new bytes demote a public row rather than inherit its signature (ADR-0073).
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

/// Internal: it scans the whole bucket. Converges, so a second run reports zero (ADR-0068).
fn internal_repair(request: &Incoming, query: &Map<String, Value>) -> Outcome {
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

/// Member, not viewer: adding a key decides whose bytes the platform later trusts as public (ADR-0073).
fn key_add(request: &Incoming, query: &Map<String, Value>) -> Outcome {
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
    // Parsed now: a key that cannot verify should be refused here, not by a publish weeks later.
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

/// Public by construction: a verifying key is what lets anyone check a signature.
fn key_list(request: &Incoming, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Viewer) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    // Revoked keys are listed too, marked, so an auditor of an old signature can see what happened to its key.
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

/// A revoked key is skipped, so it stops verifying new publishes immediately.
fn org_keys(org: &str) -> Vec<(String, String)> {
    records::find_by(ORGKEYS, "org", &json!(org).to_string())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .filter(|v| !v["revoked"].as_bool().unwrap_or(false))
        .map(|v| (str_of(&v, "name"), str_of(&v, "public_key")))
        .collect()
}

/// Demoted to private, not deleted (ADR-0076): a referenced digest must stay resolvable (ADR-0007 rule 4).
fn key_revoke(request: &Incoming, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    // Owner, not member: revoking retracts published bytes, the louder act.
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

    // Walked, not indexed: revocation is rare, and a scan is cheap next to missing a row the key touched.
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

/// The message signed is the digest STRING as stored (`sha256:…`); returns the key name for provenance.
fn verify_publish(org: &str, digest: &str, signature_b64: &str) -> Option<String> {
    use p256::ecdsa::signature::Verifier;
    let sig_raw = B64.decode(signature_b64.trim()).ok()?;
    // Both encodings: the p256 crate emits fixed-width, OpenSSL emits DER.
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

/// Narrow on purpose: it bounds how many nonces must be remembered.
const FETCH_SKEW_SECS: u64 = 60;

/// `cas::set` at revision 0 means "must not exist", so a replay conflicts (ADR-0066); no header is refused.
pub(crate) fn claim_fetch_nonce(request: &Incoming) -> Result<(), Outcome> {
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
        // Outside the window the nonce set proves nothing, so it is refused either way.
        return Err(Outcome::Err(409, "stale request".into()));
    }
    let Ok(bucket) = kv::open("default") else {
        return Err(Outcome::Err(503, "store unavailable".into()));
    };
    // Timestamp in the key: a sweeper can drop a window by prefix, and two windows cannot collide.
    let key = format!("fn_{}_{}", ts / FETCH_SKEW_SECS, sanitize_key(&nonce));
    match cas::set(&bucket, &key, b"1", 0) {
        Ok(cas::Outcome::Committed(_)) => Ok(()),
        Ok(cas::Outcome::Conflict(_)) => {
            Err(Outcome::Err(409, "this request has already been used".into()))
        }
        Err(_) => Err(Outcome::Err(503, "store unavailable".into())),
    }
}

/// Nothing that reaches a store is trusted to be well-formed.
fn sanitize_key(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .take(80)
        .collect()
}

/// An environment IS a derived app (ADR-0078), so it gets its own store `b-app-{tenant}-{app}` (ADR-0023).
fn env_app(app: &str, env: &str) -> String {
    format!("{app}-env-{env}")
}

/// The derived name collapses to a DNS label; two names collapsing together would share a store.
fn valid_env_name(env: &str) -> bool {
    !env.is_empty()
        && env.len() <= 32
        && env.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !env.starts_with('-')
        && !env.ends_with('-')
}

/// The token (minted by the reconciler, never seen by the guest) names the calling instance (ADR-0079).
fn internal_env_spawn(request: &Incoming, query: &Map<String, Value>) -> Outcome {
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
    let Ok(entry) = records::get(secrets::FETCH_TOKENS, &token) else {
        return Outcome::Err(401, "unknown instance token".into());
    };
    let Ok(doc) = serde_json::from_str::<Value>(&entry.data) else {
        return Outcome::Err(401, "unreadable instance token".into());
    };
    if doc["expires"].as_u64().unwrap_or(0) < now() {
        return Outcome::Err(401, "expired".into());
    }
    // The app (middle segment) is the only thing this call may fork.
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

/// Recorded as desired state: `plan()` reaps anything started behind the reconciler's back (ADR-0078).
fn env_spawn(request: &Incoming, _query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    // Before the lookup: admission must not depend on the request being otherwise valid.
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

    // Revisions are keyed by the deployment's RECORD ID; a manifest's `app` (the store name) is its NAME.
    let parent = records::get(DEPLOYMENTS, &app)
        .ok()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok().map(|d| (e.id, d)))
        .or_else(|| {
            all_records(DEPLOYMENTS, 100_000)
                .into_iter()
                .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok().map(|d| (e.id, d)))
                .find(|(_, d)| str_of(d, "name") == app)
        });
    // An environment may be a parent too (branching from a branch); its owner comes from its revision.
    let (app, owner) = match parent {
        Some((_, doc)) => {
            // Everything downstream keys on the deployment's NAME (the manifest's `app`, the store name).
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
    // This route only establishes that the caller may act for the owning org.
    spawn_environment(&app, &env)
}

/// Revisions are keyed two ways (deployment RECORD ID, environment NAME); both lookups let environments nest.
fn parent_of(app: &str) -> Option<(Value, String)> {
    let deployment = all_records(DEPLOYMENTS, 100_000)
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok().map(|d| (e.id, d)))
        .find(|(_, d)| str_of(d, "name") == app);
    if let Some((id, doc)) = deployment {
        return newest_revision(&id).map(|rev| (rev, str_of(&doc, "org")));
    }
    // Not a deployment: it may be an environment, a legitimate parent.
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
    // Refused, not resolved: `shop` + env `x` and an app `shop-env-x` are indistinguishable as a DNS label.
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
    // A DERIVED front door (ADR-0083): the parent's hostname on two apps would route to whichever came last.
    if let Some(host) = manifest["ingress"]["host"].as_str() {
        let derived = format!("{}.{host}", manifest::dns_label(env));
        manifest["ingress"]["host"] = json!(derived);
    }

    let revision_doc = json!({
        "deployment": derived, "tenant": str_of(&latest, "tenant"), "revision": 1,
        "strategy": latest["strategy"], "manifest": manifest,
        "org": owner, "saved": now(),
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

fn env_list(request: &Incoming, query: &Map<String, Value>) -> Outcome {
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

/// The reconciler stops its instances on the next pass.
fn env_despawn(request: &Incoming, query: &Map<String, Value>) -> Outcome {
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

    // Environments nest; descendants of `x` are named `x-env-…` (ADR-0078) and must close with it.
    let prefix = format!("{derived}-env-");
    let mut doomed: Vec<(String, String)> = Vec::new();
    // Paged, not capped at 1000 (see `all_records`).
    for e in all_records(REVISIONS, 100_000) {
        let Ok(row) = serde_json::from_str::<Value>(&e.data) else { continue };
        let name = str_of(&row, "deployment");
        if name != derived && !name.starts_with(&prefix) {
            continue;
        }
        if row["environment"].is_null() {
            // A real deployment with this name: refusing beats deleting an app because the names rhyme.
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
            "closed": closed,
        })
        .to_string(),
    )
}

fn newest_revision(id: &str) -> Option<Value> {
    records::find_by(REVISIONS, "deployment", &json!(id).to_string())
        .ok()?
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .max_by_key(|v| v["revision"].as_u64().unwrap_or(0))
}

/// What `repair` WOULD do, changing nothing (ADR-0075).
fn internal_verify(request: &Incoming, query: &Map<String, Value>) -> Outcome {
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

pub(crate) fn internal_ok(request: &Incoming) -> bool {
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

/// Created at registration, so nobody is ever without an org.
fn personal_org(p: &auth_types::Principal) -> String {
    p.tenant.clone()
}

fn org_create(request: &Incoming) -> Outcome {
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

fn org_list(request: &Incoming) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    Outcome::Structured(200, json!({ "orgs": orgs::memberships(&p.subject) }))
}

fn org_invite(request: &Incoming, org: &str) -> Outcome {
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

fn org_join(request: &Incoming) -> Outcome {
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

fn org_members(request: &Incoming, org: &str) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let q: Map<String, Value> = [("org".to_string(), json!(org))].into_iter().collect();
    if let Err((code, msg)) = orgs::acting(&p.subject, &personal_org(&p), &q, orgs::Role::Viewer) {
        return Outcome::Err(code, msg);
    }
    Outcome::Structured(200, json!({ "org": org, "members": orgs::members(org) }))
}

fn org_remove(request: &Incoming, org: &str, subject: &str) -> Outcome {
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

fn deployment_create(request: &Incoming, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Member) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    // Typed: a misspelled field is refused, not silently ignored.
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
    // Per-ORG budget: one company shares one allowance (`quota:meter`'s limit is a parameter, ADR-0008).
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

/// The operator's `default-egress` grant is this component's own config, which no tenant can write (ADR-0008).
fn with_operator_egress(mut tenant: Vec<String>, operator: &str) -> Vec<String> {
    for e in operator.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        if !tenant.iter().any(|t| t == e) {
            tenant.push(e.to_string());
        }
    }
    tenant
}

fn plan_of(tenant: &str) -> TenantPlan {
    let row = find_one(ACCOUNTS, "tenant", tenant).map(|(_, _, v)| v).unwrap_or_else(|| json!({}));
    let p = &row["plan"];
    TenantPlan {
        plan: Plan {
            replicas: p["replicas"].as_u64().unwrap_or(1) as u32,
            pool_size: p["pool_size"].as_u64().unwrap_or(8) as u32,
            max_invocations: p["max_invocations"].as_u64().unwrap_or(200) as u32,
            egress: with_operator_egress(
                p["egress"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
                &cfg("default-egress", ""),
            ),
            // A plan prices WHERE the app may run: node labels the reconciler matches (the multicloud/multiregion knob).
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

/// By ORG membership, not tenant equality; pre-org rows fall back to the record's `tenant`.
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

fn deployments_list(request: &Incoming) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
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

fn deployment_get(request: &Incoming, id: &str) -> Outcome {
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

fn resolve_parts(
    p: &auth_types::Principal,
    doc: &Value,
    strategy: Strategy,
) -> Result<Vec<Part>, Outcome> {
    // The owning org, which a secret reference is checked against. Stored at creation so a save body cannot change it.
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
        // The NAME finds the row; a tag or digest then decides which bytes it is read as.
        let r = parse_component_ref(raw);
        let id = &r.name;

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

        // An unrecorded tag is an error, not a fall-through to `latest`.
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

        // A digest overrides everything and is checked against what is staged: finding a typo here beats finding it on a node.
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
            // The row's digest describes the CURRENT pointer, not these bytes, so distribution re-derives it.
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

    // A graph that cannot support the strategy is refused with the reason, not silently switched (ADR-0005).
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
        // Every gap, and for each the components that would fill it, so a UI can say "add this" rather than "wire it".
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
        // Refused before anything is built: a config error is the author's to fix, not a failure in front of a user.
        let id = row["id"].as_str().unwrap_or_default().to_string();
        let given = given_config(&id);
        if let Err(why) = check_config(&id, row, &given) {
            return Err(Outcome::Err(422, why));
        }
        let asked = given_secrets(&id);
        let secrets = match secrets::check_secrets(&id, &deploy_org, &asked) {
            Ok(v) => v,
            Err(why) => return Err(Outcome::Err(422, why)),
        };

        // Checked LAST: a missing digest is transient, a bad config key or secret is a permanent authoring error.
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
                    // Values are strings on the wire: `wasi:config/store` hands the guest a string.
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
            // One component: the root with everything composed into it; host imports are the union.
            let root = plan.roots.first().cloned().ok_or_else(|| {
                Outcome::Err(422, "no root: something is plugged into every component".into())
            })?;
            let root_row = rows
                .iter()
                .find(|r| r["id"] == json!(root))
                .ok_or_else(|| Outcome::Err(500, "root vanished".into()))?;
            // COMPOSE, for real. The root's own digest alone would ship it with its imports unsatisfied.
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

            // The composed artifact is a new component staged like any other; the existing pending-push queue distributes it.
            let fused_id = format!("{root}-fused");
            let key = format!("{}/{}", p.tenant, fused_id);
            if blob::put(BIN, &key, &fused, "application/wasm").is_err() {
                return Err(Outcome::Err(500, "could not stage the composed artifact".into()));
            }

            // ONE `wasi:config/store` per fused artifact: a key with two different values is refused, not resolved by order.
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

            // Secrets merge like config: one artifact asks with one identity, so one KEY from different refs is ambiguous.
            let mut fused_secrets: std::collections::BTreeMap<String, (String, String)> =
                std::collections::BTreeMap::new();
            for row in &rows {
                let id = row["id"].as_str().unwrap_or_default().to_string();
                let checked = match secrets::check_secrets(&id, &deploy_org, &given_secrets(&id)) {
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

/// `shop` (moving), `shop:v2` (named pointer), `shop@sha256:<hex>` (exact bytes, reproducible).
pub struct ComponentRef {
    pub name: String,
    pub tag: Option<String>,
    pub digest: Option<String>,
}

fn parse_component_ref(raw: &str) -> ComponentRef {
    // `@` binds tighter than `:`: naming a digest means nothing else gets a say.
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

/// Ask the row, do not rebuild `tenant/id`: the name may no longer hold these bytes.
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

/// Recorded on every revision so the NEXT save can compare: the catalogue row is overwritten by upload.
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

/// Surfaces never decide whether an artifact CHANGED (a changed constant keeps them equal),
/// only whether a change BREAKS something: a vanished export.
fn lost_exports(before: &Value, after: &Value) -> Vec<String> {
    let mut gone = Vec::new();
    let Some(old) = before.as_object() else { return gone };
    for (component, exports) in old {
        // A component removed from the graph is deliberate, not a broken upgrade.
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

/// Not hashed here: the distributor content-addresses the bytes it fetched, a stronger check.
fn stage_fused(
    tenant: &str,
    fused_id: &str,
    key: &str,
    root_row: &Value,
    plan: &composer::CompositionPlan,
) -> String {
    // Root's exports (preserved) over the graph's host needs (unioned).
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
    // Inputs by content: a changed constant leaves surfaces IDENTICAL, so comparing them never re-composed.
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

fn deployment_save(request: &Incoming, id: &str, query: &Map<String, Value>) -> Outcome {
    // Removing an export is sometimes the point. An author who means it says so.
    let force = query.get("force").and_then(|v| v.as_str()).is_some_and(|v| v == "true");
    // A save asks the fleet for work too, so it is admitted like environment spawns.
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
    // Unknown fields are refused: `{"noodes": [...]}` once saved the OLD graph and reported success.
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
    // THE isolation change: plan, hostname and bucket are keyed by the owning ORG, from a record
    // the guest cannot write (ADR-0012).
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

    // Compatibility gate: refuse a removed export here, where the author is; `?force=true` when that IS the change.
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
        "surfaces": surfaces,
    });
    let _ = records::create(
        REVISIONS,
        &revision_doc.to_string(),
        &["deployment".to_string(), "tenant".to_string()],
    );

    doc["revision"] = json!(next);
    // Nothing to apply: the reconciler polls the current revision, so a save is complete when stored and cannot half-succeed (ADR-0022).
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

fn manifests(request: &Incoming, id: &str) -> Outcome {
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
        Some(v) => Outcome::Structured(
            200,
            json!({ "revision": v["revision"], "saved": v["saved"], "manifest": v["manifest"] }),
        ),
        None => Outcome::Err(404, "no revision yet — deploy first".into()),
    }
}

/// Follows the cursor: one big-limit call SILENTLY truncates (a flat 1000 once capped a fleet at 500 apps).
fn all_records(collection: &str, cap: usize) -> Vec<records::Entry> {
    const PAGE: u32 = 500;
    let mut out = Vec::new();
    let mut after = String::new();
    while let Ok(page) = records::list_records(collection, PAGE, &after) {
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

fn internal_revisions(request: &Incoming) -> Outcome {
    if !internal_ok(request) {
        return Outcome::Err(401, "internal endpoint".into());
    }
    let mut current: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    // Paged, not capped at 1000 (see `all_records`).
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

/// The reconciler derives desired state from records, so a removed app stops on its own.
fn deployment_delete(request: &Incoming, id: &str, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "unauthorized".into());
    };
    // Destroys the app's storage permanently (ADR-0016): an owner action.
    let Some((rec, _rev, doc)) = owned_deployment(&p, id, orgs::Role::Owner) else {
        return Outcome::Err(404, "not_found".into());
    };
    let name = doc["name"].as_str().unwrap_or("app").to_string();

    // 428 unless `?confirm=<name>`: an accidental or replayed DELETE cannot satisfy it.
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

pub(crate) fn body(request: &Incoming) -> Result<Value, Outcome> {
    let raw = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

/// Backstop under wasmtime's 64 MiB per-store cap, which traps instead of erroring.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body_named!(read_whole_body, MAX_BODY_BYTES);

pub(crate) fn read_body(request: &Incoming) -> Result<Vec<u8>, ()> {
    request.body.clone()
}

fn emit(result: Outcome) -> Result<Response, ErrorCode> {
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
    respond_with(code, headers, body)
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn operator_egress_is_added_once_and_never_removes_a_tenants_own() {
        let own = vec!["api.stripe.com".to_string()];
        let got = with_operator_egress(own.clone(), " 127.0.0.1:9 , api.stripe.com ,,");
        assert_eq!(got, vec!["api.stripe.com", "127.0.0.1:9"]);
        assert_eq!(with_operator_egress(own.clone(), ""), own);
    }

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
        assert!(declared_config(&Map::new()).is_empty());
    }

    #[test]
    fn a_typo_is_refused_and_the_message_lists_the_legal_keys() {
        // "you wrote grace-period-sec, it takes grace-period-secs" is the whole value of ADR-0010.
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
        check_config("gate", &row(&[("token", false)]), &given(&[])).unwrap();
    }

    #[test]
    fn a_component_that_declares_nothing_accepts_nothing() {
        // The message says so, since an empty list reads as a bug.
        let err = check_config("gate", &json!({ "id": "gate" }), &given(&[("anything", "1")]))
            .unwrap_err();
        assert!(err.contains("declares no config keys"), "{err}");
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
        // Regression: an escaped ref compared unequal to the ref it named.
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

const FLEET: &str = "fleet";

/// One row, overwritten.
const FLEET_ROW: &str = "status";

fn internal_status_put(request: &Incoming) -> Outcome {
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
    // A new report accounts for everything admitted before it.
    body["admitted"] = json!(0);

    // History is a metrics system's job; here it would grow in the collection admission reads every spawn.
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

/// Without it a burst faster than the reporting interval sails through; it makes a stale figure a lower bound.
fn admitted_since_report() -> u64 {
    records::find_by(FLEET, "row", &json!(FLEET_ROW).to_string())
        .unwrap_or_default()
        .into_iter()
        .next()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok())
        .and_then(|v| v["admitted"].as_u64())
        .unwrap_or(0)
}

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

/// Admission lives here, not in the reconciler judging its own backlog. Refusing beats queueing.
fn admit_one_more() -> Result<(), Outcome> {
    // Per node, times the nodes carrying work; `max-placement-lag` still overrides as a hard cap.
    let per_node = cfg_u64("max-placement-lag-per-node", 70);
    let flat = cfg_u64("max-placement-lag", 0);
    if flat == 0 && per_node == 0 {
        return Ok(()); // explicitly disabled
    }
    let stale_after = cfg_u64("status-max-age", 90);

    let Some((lag, age, nodes)) = fleet_lag() else {
        // Nothing has ever reported: allowed, or a fresh install would refuse every spawn until a reconciler spoke.
        return Ok(());
    };

    // FAIL CLOSED on a stale report: if the loop stopped, nothing will place more work.
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

    #[test]
    fn a_digest_names_bytes_nothing_can_move() {
        let r = parse_ref("shop@sha256:abc123");
        assert_eq!(r.name, "shop");
        assert_eq!(r.digest.as_deref(), Some("abc123"));
    }

    #[test]
    fn a_digest_beats_the_tag_beside_it() {
        let r = parse_ref("shop:v2@sha256:abc123");
        assert_eq!(r.name, "shop");
        assert_eq!(r.tag.as_deref(), Some("v2"), "the tag is kept, for the record");
        assert_eq!(r.digest.as_deref(), Some("abc123"), "but the digest decides");
    }

    /// A stray colon in something that is not a reference must not mangle it.
    #[test]
    fn a_name_that_looks_like_a_url_is_still_a_name() {
        let r = parse_ref("shop:8080");
        assert_eq!((r.name.as_str(), r.tag.as_deref()), ("shop", Some("8080")));
    }
}
