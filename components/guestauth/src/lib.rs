//! `guestauth` — the `auth:identity` register/login/RBAC+ABAC boilerplate every "real SaaS" domain
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

/// Plain shared type: no field names a `bindings::…` type.
pub struct Route {
    pub segments: Vec<String>,
    pub bearer: String,
}

#[macro_export]
macro_rules! guest_auth_reply {
    () => {
        pub struct Reply {
            pub status: u16,
            pub json: serde_json::Value,
            pub file: Option<(String, Vec<u8>)>,
        }

        impl Reply {
            pub fn json(status: u16, body: serde_json::Value) -> Self {
                Reply { status, json: body, file: None }
            }
            pub fn err(status: u16, code: &str) -> Self {
                Reply::json(status, serde_json::json!({ "error": code }))
            }
            pub fn file(status: u16, content_type: &str, bytes: Vec<u8>) -> Self {
                Reply {
                    status,
                    json: serde_json::Value::Null,
                    file: Some((content_type.to_string(), bytes)),
                }
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

/// Requires `guest_auth_reply!()` in scope.
#[macro_export]
macro_rules! guest_introspect {
    () => {
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

/// Reads `roles` directly rather than rbac's permission mapping: few roles, no seeding to sync.
#[macro_export]
macro_rules! guest_role_check {
    ($name:ident, $role:literal) => {
        pub fn $name(p: &crate::bindings::auth::identity::types::Principal) -> bool {
            p.roles.iter().any(|r| r == $role)
        }
    };
}

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

/// Requires `guest_auth_reply!()`, `guest_introspect!()` and `guest_audit!()` in scope.
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

        /// A role outside `$roles` degrades to `$default_role`: an unknown privilege request gets the least.
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

/// Requires `guest_accounts_endpoints!()` and `handlers::handle(&Method, &Route, &str) -> Reply`.
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

#[macro_export]
macro_rules! guest_emit {
    () => {
        fn emit(response_out: crate::bindings::wasi::http::types::ResponseOutparam, reply: Reply) {
            use crate::bindings::wasi::http::types::{Fields, OutgoingBody, OutgoingResponse};
            let headers = Fields::new();
            // A `Value::Null` body writes nothing.
            let write_bytes = match &reply.file {
                Some((content_type, bytes)) => {
                    let _ = headers.set("content-type", &[content_type.as_bytes().to_vec()]);
                    Some(bytes.clone())
                }
                None => {
                    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
                    (!reply.json.is_null()).then(|| reply.json.to_string().into_bytes())
                }
            };
            let resp = OutgoingResponse::new(headers);
            let _ = resp.set_status_code(reply.status);
            let out = resp.body().expect("body");
            crate::bindings::wasi::http::types::ResponseOutparam::set(response_out, Ok(resp));
            if let Some(bytes) = write_bytes {
                if let Ok(stream) = out.write() {
                    let _ = write_all(&stream, &bytes);
                }
            }
            let _ = OutgoingBody::finish(out, None);
        }
    };
}

/// The owner-or-admin row rule rbac cannot express; its rules are registered once per deployment.
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

        /// `action` is carried though every rule matches "*", so an action-scoped rule needs no call-site change.
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

#[macro_export]
macro_rules! guest_authenticated {
    ($route:expr) => {
        match introspect($route) {
            Ok(p) => p,
            Err(r) => return r,
        }
    };
}

/// The denial is audited, so a 403 never lacks an audit trail.
#[macro_export]
macro_rules! guest_deny_unless {
    ($cond:expr, $principal:expr, $event:expr, $detail:expr) => {
        if !($cond) {
            audit($event, "deny", &$principal.subject, $detail);
            return Reply::err(403, "forbidden");
        }
    };
}

#[macro_export]
macro_rules! guest_parse_body {
    ($body:expr, $ty:ty) => {
        match serde_json::from_str::<$ty>($body) {
            Ok(v) => v,
            Err(_) => return Reply::err(400, "bad_json"),
        }
    };
}

#[macro_export]
macro_rules! guest_get_or_404 {
    ($collection:expr, $id:expr) => {
        match records::get($collection, $id) {
            Ok(e) => e,
            Err(_) => return Reply::err(404, "not_found"),
        }
    };
}

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

// p3 variants of the macros touching `wasi:http`; they name the `crate::bindings::p3::http` alias.

/// Requires `guestio::guest_p3_bearer!()`, `guestio::guest_p3_read_body_text!()`, `guest_p3_emit!()`.
#[macro_export]
macro_rules! guest_p3_saas_router {
    () => {
        struct Component;

        impl crate::bindings::p3::handler::Guest for Component {
            async fn handle(
                request: crate::bindings::p3::http::types::Request,
            ) -> Result<
                crate::bindings::p3::http::types::Response,
                crate::bindings::p3::http::types::ErrorCode,
            > {
                use crate::bindings::p3::http::types::Method;
                let path = request.get_path_with_query().unwrap_or_else(|| "/".into());
                let raw_path = path.split('?').next().unwrap_or("/").to_string();
                let bearer = bearer(&request).unwrap_or_default();
                let method = request.get_method();
                let body = match method {
                    Method::Post | Method::Put | Method::Patch | Method::Delete => {
                        read_body(request).await
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
                emit(reply)
            }
        }

        bindings::export!(Component with_types_in bindings);
    };
}

/// Requires `guestio::guest_p3_respond!()` in scope.
#[macro_export]
macro_rules! guest_p3_emit {
    () => {
        fn emit(
            reply: Reply,
        ) -> Result<
            crate::bindings::p3::http::types::Response,
            crate::bindings::p3::http::types::ErrorCode,
        > {
            let headers = crate::bindings::p3::http::types::Fields::new();
            let bytes = match reply.file {
                Some((content_type, bytes)) => {
                    let _ = headers.set("content-type", &[content_type.into_bytes()]);
                    bytes
                }
                None => {
                    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
                    if reply.json.is_null() {
                        Vec::new()
                    } else {
                        reply.json.to_string().into_bytes()
                    }
                }
            };
            respond_with(reply.status, headers, bytes)
        }
    };
}

/// The world imports p3 `system-clock` in place of `wall-clock@0.2.0` (the `p3::clocks` alias).
#[macro_export]
macro_rules! guest_p3_audit {
    ($tenant:expr) => {
        pub fn now_secs() -> u64 {
            crate::bindings::p3::clocks::system_clock::now().seconds as u64
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
