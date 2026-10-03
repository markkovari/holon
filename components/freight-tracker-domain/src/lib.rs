//! `freight-tracker-domain` — track shipments behind a login, with per-account access to their history

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../audit-log/wit",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/ratelimit-guard",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "wit",
        ],
        world: "freight-tracker:freight-tracker/freight-tracker-domain",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::p3::clocks::system_clock;
use bindings::records::store::store as records;
use bindings::wasi::keyvalue::store;
use serde_json::{json, Map, Value};

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};

struct Component;

const TENANT: &str = "freight-tracker";

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        // Measure usage
        if let Ok(bucket) = store::open("default") {
            let count_bytes = bucket.get("usage_count").unwrap_or(None).unwrap_or(b"0".to_vec());
            let count = String::from_utf8_lossy(&count_bytes).parse::<u64>().unwrap_or(0);
            let _ = bucket.set("usage_count", (count + 1).to_string().as_bytes());
        }

        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();
        // Reading a p3 body consumes the request: take the bearer first, then read
        // the body once, only for the routes that have one.
        let request = Req {
            bearer: bearer(&request),
            raw: if matches!(method, Method::Post) {
                read_body(request).await
            } else {
                Ok(Vec::new())
            },
        };

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => serve_html(),
            (Method::Post, ["api", "register"]) => register(&request),
            (Method::Post, ["api", "login"]) => login(&request),
            (Method::Post, ["api", "logout"]) => logout(&request),
            (Method::Get, ["api", "me"]) => me(&request),
            (Method::Get, ["api", "items"]) => list_items(&request),
            (Method::Post, ["api", "items"]) => create_item(&request),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(outcome)
    }
}

/// What the handlers need from a request, taken before its body was consumed.
struct Req {
    bearer: Option<String>,
    raw: Result<Vec<u8>, ()>,
}

enum Outcome {
    Html(u16, String),
    Json(u16, String),
    Err(u16, String),
    Auth(AuthError),
}

fn now() -> u64 {
    system_clock::now().seconds as u64
}

fn serve_html() -> Outcome {
    let html = r#"<!DOCTYPE html>
<html>
<head>
    <title>freight-tracker</title>
    <style>body { font-family: sans-serif; margin: 2rem; }</style>
</head>
<body>
    <h1>freight-tracker (Logistics and Freight Tracking)</h1>
    <div id="app">Please interact via API for now.</div>
    <script>
        console.log("App loaded.");
    </script>
</body>
</html>"#;
    Outcome::Html(200, html.to_string())
}

guestio::guest_p3_bearer!();

fn introspect(request: &Req) -> Result<Principal, Outcome> {
    let token = request
        .bearer
        .clone()
        .ok_or(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())))?;
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

fn body(request: &Req) -> Result<Value, Outcome> {
    // simplified body reader
    let raw = request.raw.clone().map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

/// Ceiling on a request body, matching the rest of the tree.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body!(MAX_BODY_BYTES);

fn register(request: &Req) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = b["email"].as_str().unwrap_or("").trim().to_string();
    let password = b["password"].as_str().unwrap_or("").to_string();
    match accounts::register(&email, &password, TENANT) {
        Ok(p) => Outcome::Json(201, json!({ "subject": p.subject }).to_string()),
        Err(e) => Outcome::Auth(e),
    }
}

fn login(request: &Req) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = b["email"].as_str().unwrap_or("").trim().to_string();
    let password = b["password"].as_str().unwrap_or("").to_string();
    match accounts::login(&email, &password, TENANT) {
        Ok(tp) => Outcome::Json(200, json!({ "access_token": tp.access_token }).to_string()),
        Err(e) => Outcome::Auth(e),
    }
}

fn logout(request: &Req) -> Outcome {
    let token = match request.bearer.clone() {
        Some(t) => t,
        None => return Outcome::Auth(AuthError::InvalidToken("missing bearer".into())),
    };
    match session::revoke(&token) {
        Ok(()) => Outcome::Json(200, json!({ "ok": true }).to_string()),
        Err(e) => Outcome::Auth(e),
    }
}

fn me(request: &Req) -> Outcome {
    match introspect(request) {
        Ok(p) => Outcome::Json(200, json!({ "subject": p.subject, "roles": p.roles }).to_string()),
        Err(o) => o,
    }
}

fn create_item(request: &Req) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };

    // RBAC check: Only admins can create items
    if !p.roles.contains(&"dispatcher".to_string()) {
        return Outcome::Err(403, "forbidden".into());
    }

    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let name = b["name"].as_str().unwrap_or("").trim().to_string();
    let d = json!({ "name": name, "owner": p.subject, "created": now() });
    match records::create("shipments", &d.to_string(), &["owner".to_string()]) {
        Ok(rec) => {
            let mut v: Value = serde_json::from_str(&rec.data).unwrap_or(d);
            v["id"] = json!(rec.id);
            Outcome::Json(201, v.to_string())
        }
        Err(_) => Outcome::Err(500, "store error".into()),
    }
}

fn list_items(request: &Req) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let items: Vec<Value> = records::find_by("shipments", "owner", &json!(p.subject).to_string())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| {
            serde_json::from_str::<Value>(&e.data).ok().map(|mut v| {
                v["id"] = json!(e.id);
                v
            })
        })
        .collect();
    Outcome::Json(200, json!({ "items": items }).to_string())
}

fn emit(result: Outcome) -> Result<Response, ErrorCode> {
    let (code, body, content_type) = match result {
        Outcome::Html(c, b) => (c, b, b"text/html".to_vec()),
        Outcome::Json(c, b) => (c, b, b"application/json".to_vec()),
        Outcome::Err(c, m) => (c, json!({ "error": m }).to_string(), b"application/json".to_vec()),
        Outcome::Auth(e) => {
            let msg = match &e {
                AuthError::InvalidToken(m) => m.clone(),
                AuthError::InvalidCredentials => "invalid credentials".into(),
                other => format!("{other:?}"),
            };
            (401, json!({ "error": msg }).to_string(), b"application/json".to_vec())
        }
    };
    let headers = Fields::new();
    let _ = headers.set("content-type", &[content_type]);
    respond_with(code, headers, body.into_bytes())
}

bindings::export!(Component with_types_in bindings);

guestio::guest_p3_respond!();
