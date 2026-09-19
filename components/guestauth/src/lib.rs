//! `guestauth` — the `auth:identity` register/login/RBAC+ABAC boilerplate
//! every "real SaaS" domain component needs, expanded into the caller rather
//! than linked. Same reason as `guestio` (ADR-0095): `auth:identity`'s
//! `Principal` and `AuthError` are generated PER CRATE by `cargo-component`,
//! so a shared function that takes one is impossible — a macro that expands
//! the function INTO the caller is not.
//!
//! `crm-domain`, `billing-domain`, `ats-domain`, `timesheet-domain` and
//! `feedback-domain` each hand-wrote an identical `Reply` type, `introspect`,
//! an audit helper, and a register/login/logout/me router — five copies
//! differing only in a tenant string, a role list and a default role. Four
//! of the five also hand-wrote an identical "owner or admin may act on this
//! row" policy check, differing only in the resource's attribute name and
//! which action it gates. This crate is that, once each. Every SaaS app
//! built since — `expense-domain`, `survey-domain`, `parking-domain` — has
//! also hand-wrote an identical `entries_json` (a page of `records:store`
//! entries with each one's id merged in); `guest_entries_json!()` is that,
//! once.
//!
//! What is DELIBERATELY not here: `timesheet-domain`'s policy check has no
//! admin bypass at all (a routed manager decides, nobody else, including the
//! member who logged the entry) — a real business-rule difference, not
//! incidental duplication, so it stays hand-written rather than being forced
//! through `guest_owner_or_admin_policy!` with an unused knob.
//!
//! ```ignore
//! use guestauth::Route;
//! guestauth::guest_auth_reply!();
//! guestauth::guest_introspect!();
//! guestauth::guest_role_check!(is_admin, "admin");
//! guestauth::guest_audit!("crm");
//! guestauth::guest_accounts_endpoints!("crm", &["admin", "rep"], "rep");
//! guestauth::guest_saas_router!();
//! ```

#![allow(clippy::crate_in_macro_def)]

/// A request awaiting dispatch. A plain shared type, unlike everything else
/// here: neither field names a `bindings::…` type, so nothing about
/// ADR-0095 stops this one from being ordinary and non-macro.
pub struct Route {
    pub segments: Vec<String>,
    pub bearer: String,
}

/// Define the `Reply` type every handler answers with, including the ONE
/// mapping from `auth:identity`'s error variants to an HTTP status — measured
/// identical across all five call sites this replaces.
///
/// Expanded rather than shared for the same reason `introspect` is: `err`'s
/// parameter is `crate::bindings::auth::identity::types::AuthError`, a type
/// that exists once per crate, not once for the whole dependency graph.
#[macro_export]
macro_rules! guest_auth_reply {
    () => {
        /// What a handler answers with: a status and a JSON body.
        ///
        /// Expanded by `guestauth::guest_auth_reply!()`.
        pub struct Reply {
            pub status: u16,
            pub json: serde_json::Value,
        }

        impl Reply {
            pub fn json(status: u16, body: serde_json::Value) -> Self {
                Reply { status, json: body }
            }
            pub fn err(status: u16, code: &str) -> Self {
                Reply::json(status, serde_json::json!({ "error": code }))
            }
            pub fn auth_err(e: crate::bindings::auth::identity::types::AuthError) -> Self {
                use crate::bindings::auth::identity::types::AuthError;
                match e {
                    AuthError::InsufficientScope(_) => Reply::err(403, "forbidden"),
                    AuthError::InvalidCredentials => Reply::err(401, "invalid_credentials"),
                    AuthError::AlreadyExists => Reply::err(409, "already_exists"),
                    AuthError::Malformed(_) => Reply::err(400, "malformed"),
                    AuthError::RateLimited(_) => Reply::err(429, "rate_limited"),
                    _ => Reply::err(401, "unauthorized"),
                }
            }
        }
    };
}

/// Define `introspect`: the 401-on-no-bearer check every domain route needs
/// before it can even ask who is calling. Requires `guest_auth_reply!()` to
/// already be in scope (for `Reply`).
#[macro_export]
macro_rules! guest_introspect {
    () => {
        /// A verified caller for a domain route.
        ///
        /// Expanded by `guestauth::guest_introspect!()`.
        pub fn introspect(
            route: &$crate::Route,
        ) -> Result<crate::bindings::auth::identity::types::Principal, Reply> {
            if route.bearer.is_empty() {
                return Err(Reply::err(401, "unauthorized"));
            }
            crate::bindings::auth::identity::authorizer::introspect(&route.bearer)
                .map_err(Reply::auth_err)
        }
    };
}

/// Define a role-membership check, e.g. `guest_role_check!(is_admin, "admin")`.
///
/// Reads `roles` directly rather than going through `auth:identity/rbac`'s
/// permission-mapping layer — the domains this serves have few enough roles
/// that a role check says as plainly as a permission check would, with no
/// `rbac::set-role-permissions` seeding step to keep in sync (the same
/// tradeoff `track-domain` makes).
#[macro_export]
macro_rules! guest_role_check {
    ($name:ident, $role:literal) => {
        pub fn $name(p: &crate::bindings::auth::identity::types::Principal) -> bool {
            p.roles.iter().any(|r| r == $role)
        }
    };
}

/// Define `now_secs` and `audit`: a timestamp and an `audit:log` event
/// carrying it, for `$tenant`.
#[macro_export]
macro_rules! guest_audit {
    ($tenant:expr) => {
        pub fn now_secs() -> u64 {
            crate::bindings::wasi::clocks::wall_clock::now().seconds
        }

        pub fn audit(event: &str, outcome: &str, subject: &str, detail: &str) {
            use crate::bindings::audit::log::recorder as audit_rec;
            use crate::bindings::audit::log::types::Event;
            let _ = audit_rec::record_event(&Event {
                id: String::new(),
                trace_id: String::new(),
                span_id: String::new(),
                timestamp: now_secs(),
                event: event.to_string(),
                outcome: outcome.to_string(),
                tenant: $tenant.to_string(),
                subject: subject.to_string(),
                detail: detail.to_string(),
            });
        }
    };
}

/// Define `register`/`login`/`logout`/`me` — a real (non-test) `auth:identity`
/// accounts flow, for `$tenant`, restricted to `$roles` (falling back to
/// `$default_role` for anything else, including an absent choice). Requires
/// `guest_auth_reply!()`, `guest_introspect!()` and `guest_audit!()` already
/// in scope.
#[macro_export]
macro_rules! guest_accounts_endpoints {
    ($tenant:expr, $roles:expr, $default_role:expr) => {
        #[derive(serde::Deserialize)]
        struct RegisterReq {
            email: String,
            password: String,
            #[serde(default)]
            role: Option<String>,
        }

        /// Real account creation — `accounts::register` then `rbac::assign-role`
        /// — never a test-only shortcut. A role outside `$roles` falls back to
        /// `$default_role` rather than erroring: a role is a privilege, not a
        /// free-text field, so an unrecognised request degrades quietly to the
        /// least-privileged one instead of refusing to register at all.
        pub fn register(body: &str) -> Reply {
            use crate::bindings::auth::identity::accounts;
            use crate::bindings::auth::identity::rbac;
            let req: RegisterReq = match serde_json::from_str(body) {
                Ok(v) => v,
                Err(_) => return Reply::err(400, "bad_json"),
            };
            let principal = match accounts::register(&req.email, &req.password, $tenant) {
                Ok(p) => p,
                Err(e) => return Reply::auth_err(e),
            };
            let wanted = req.role.unwrap_or_else(|| $default_role.to_string());
            let role =
                if $roles.contains(&wanted.as_str()) { wanted } else { $default_role.to_string() };
            let _ = rbac::assign_role(&principal.tenant, &principal.subject, &role);
            audit("account.register", "allow", &principal.subject, &role);
            Reply::json(201, serde_json::json!({"subject": principal.subject, "role": role}))
        }

        #[derive(serde::Deserialize)]
        struct LoginReq {
            email: String,
            password: String,
        }

        pub fn login(body: &str) -> Reply {
            use crate::bindings::auth::identity::accounts;
            let req: LoginReq = match serde_json::from_str(body) {
                Ok(v) => v,
                Err(_) => return Reply::err(400, "bad_json"),
            };
            match accounts::login(&req.email, &req.password, $tenant) {
                Ok(tp) => {
                    audit("account.login", "allow", &req.email, "");
                    Reply::json(
                        200,
                        serde_json::json!({
                            "access_token": tp.access_token,
                            "refresh_token": tp.refresh_token,
                            "expires_in": tp.expires_in,
                        }),
                    )
                }
                Err(e) => {
                    audit("account.login", "deny", &req.email, "");
                    Reply::auth_err(e)
                }
            }
        }

        pub fn logout(route: &$crate::Route) -> Reply {
            use crate::bindings::auth::identity::session;
            if route.bearer.is_empty() {
                return Reply::err(401, "unauthorized");
            }
            match session::revoke(&route.bearer) {
                Ok(()) => Reply::json(204, serde_json::Value::Null),
                Err(e) => Reply::auth_err(e),
            }
        }

        pub fn me(route: &$crate::Route) -> Reply {
            match introspect(route) {
                Ok(p) => Reply::json(
                    200,
                    serde_json::json!({"subject": p.subject, "tenant": p.tenant, "roles": p.roles}),
                ),
                Err(r) => r,
            }
        }
    };
}

/// Define the whole HTTP surface: `Component`, its `Guest` impl (dispatching
/// `/register`, `/login`, `/logout`, `/me`, `/health`, and everything under
/// `/api` to `crate::handlers::handle`), the response-writing glue, and the
/// export. Requires `guest_accounts_endpoints!()` already in scope, and a
/// `mod handlers` exposing `pub fn handle(&Method, &Route, &str) -> Reply`.
#[macro_export]
macro_rules! guest_saas_router {
    () => {
        struct Component;

        impl crate::bindings::exports::wasi::http::incoming_handler::Guest for Component {
            fn handle(
                request: crate::bindings::wasi::http::types::IncomingRequest,
                response_out: crate::bindings::wasi::http::types::ResponseOutparam,
            ) {
                use crate::bindings::wasi::http::types::Method;
                let path = request.path_with_query().unwrap_or_else(|| "/".into());
                let raw_path = path.split('?').next().unwrap_or("/").to_string();
                let bearer = bearer(&request).unwrap_or_default();
                let method = request.method();
                let body = match method {
                    Method::Post | Method::Put | Method::Patch | Method::Delete => {
                        read_body(&request)
                    }
                    _ => String::new(),
                };
                let segments: Vec<String> =
                    raw_path.split('/').filter(|s| !s.is_empty()).map(str::to_string).collect();
                let route = $crate::Route { segments, bearer };
                let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();

                let reply = if seg.as_slice() == ["health"] {
                    Reply::json(200, serde_json::json!({"ok": true}))
                } else {
                    match (&method, seg.as_slice()) {
                        (Method::Post, ["register"]) => register(&body),
                        (Method::Post, ["login"]) => login(&body),
                        (Method::Post, ["logout"]) => logout(&route),
                        (Method::Get, ["me"]) => me(&route),
                        (_, ["api", ..]) => crate::handlers::handle(&method, &route, &body),
                        _ => Reply::err(404, "not_found"),
                    }
                };
                emit(response_out, reply);
            }
        }

        bindings::export!(Component with_types_in bindings);
    };
}

/// Write one `Reply` to the outgoing response — the part of `guest_saas_router!`
/// that genuinely needs the caller's own `bindings` types, split out so the
/// macro above stays readable.
#[macro_export]
macro_rules! guest_emit {
    () => {
        fn emit(response_out: crate::bindings::wasi::http::types::ResponseOutparam, reply: Reply) {
            use crate::bindings::wasi::http::types::{Fields, OutgoingBody, OutgoingResponse};
            let headers = Fields::new();
            let _ = headers.set("content-type", &[b"application/json".to_vec()]);
            let resp = OutgoingResponse::new(headers);
            let _ = resp.set_status_code(reply.status);
            let out = resp.body().expect("body");
            crate::bindings::wasi::http::types::ResponseOutparam::set(response_out, Ok(resp));
            if !reply.json.is_null() {
                if let Ok(stream) = out.write() {
                    let _ = write_all(&stream, reply.json.to_string().as_bytes());
                }
            }
            let _ = OutgoingBody::finish(out, None);
        }
    };
}

/// Define `owns_or_admin`: the "a resource's own `$resource_attr` matches the
/// caller, or the caller is an admin" row-level rule `auth:identity/rbac`
/// cannot express — the same shape `crm-domain` enforces for "a rep only acts
/// on their own deals", `billing-domain` for invoices, `ats-domain` for
/// postings and `feedback-domain` for post deletion, differing only in the
/// attribute name and the policy domain.
///
/// Idempotent rule registration lives here too: the two rules are only
/// written to `policy:guard` once per deployment, guarded by a marker record
/// in the `meta` collection — identical across all four call sites this
/// replaces.
#[macro_export]
macro_rules! guest_owner_or_admin_policy {
    ($policy_domain:expr, $resource_attr:literal) => {
        fn ensure_policy_rules() {
            use crate::bindings::policy::guard::guard as policy;
            use crate::bindings::policy::guard::guard::{Condition, Effect, Op, Rule as PolicyRule};
            use crate::bindings::records::store::store as records;
            match records::find_by("meta", "kind", "\"policy_rules\"") {
                Ok(entries) if !entries.is_empty() => {}
                _ => {
                    let rules = vec![
                        PolicyRule {
                            id: "owner-may-act".to_string(),
                            action: "*".to_string(),
                            effect: Effect::Allow,
                            conditions: vec![Condition {
                                left: format!("resource.{}", $resource_attr),
                                op: Op::Eq,
                                right: "principal.subject".to_string(),
                            }],
                            priority: 10,
                        },
                        PolicyRule {
                            id: "admin-may-act".to_string(),
                            action: "*".to_string(),
                            effect: Effect::Allow,
                            conditions: vec![Condition {
                                left: "principal.roles".to_string(),
                                op: Op::Has,
                                right: "admin".to_string(),
                            }],
                            priority: 5,
                        },
                    ];
                    if policy::set_rules($policy_domain, &rules).is_ok() {
                        let marker = serde_json::json!({"kind": "policy_rules"}).to_string();
                        let _ = records::create("meta", &marker, &["kind".to_string()]);
                    }
                }
            }
        }

        /// `action` is carried through even though every rule above matches
        /// `"*"` — `policy:guard/guard::enforce` still takes one, and a future
        /// rule scoped to a specific action (the way `feedback-domain` used to
        /// scope its own to `"delete"`) can be added without touching the call
        /// sites below.
        fn owns_or_admin(
            action: &str,
            p: &crate::bindings::auth::identity::types::Principal,
            resource_value: &str,
        ) -> bool {
            use crate::bindings::policy::guard::guard as policy;
            use crate::bindings::policy::guard::guard::Attr;
            ensure_policy_rules();
            let principal_attrs = vec![
                Attr { key: "subject".to_string(), value: p.subject.clone() },
                Attr { key: "roles".to_string(), value: p.roles.join(",") },
            ];
            let resource_attrs =
                vec![Attr { key: $resource_attr.to_string(), value: resource_value.to_string() }];
            policy::enforce($policy_domain, action, &principal_attrs, &resource_attrs)
        }
    };
}

/// Define `entries_json`: a page of `records:store` entries as a JSON array,
/// each entry's stored id merged into its own document — the same helper
/// `billing-domain`, `crm-domain`, `ats-domain`, `timesheet-domain`,
/// `feedback-domain`, `expense-domain` and `parking-domain` each hand-wrote,
/// byte-identical every time.
#[macro_export]
macro_rules! guest_entries_json {
    () => {
        fn entries_json(
            entries: &[crate::bindings::records::store::store::Entry],
        ) -> Vec<serde_json::Value> {
            entries
                .iter()
                .map(|e| {
                    let mut v: serde_json::Value =
                        serde_json::from_str(&e.data).unwrap_or(serde_json::json!({}));
                    if let serde_json::Value::Object(ref mut m) = v {
                        m.insert("id".to_string(), serde_json::json!(e.id));
                    }
                    v
                })
                .collect()
        }
    };
}
