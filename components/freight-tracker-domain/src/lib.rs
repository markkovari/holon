#[allow(warnings)]
mod bindings;

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::records::store::store as records;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::store;
use serde_json::{json, Map, Value};

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const TENANT: &str = "freight-tracker";

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        // Measure usage
        if let Ok(bucket) = store::open("default") {
            let count_bytes = bucket.get("usage_count").unwrap_or(None).unwrap_or(b"0".to_vec());
            let count = String::from_utf8_lossy(&count_bytes).parse::<u64>().unwrap_or(0);
            let _ = bucket.set("usage_count", (count + 1).to_string().as_bytes());
        }

        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

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
        emit(response_out, outcome);
    }
}

enum Outcome {
    Html(u16, String),
    Json(u16, String),
    Err(u16, String),
    Auth(AuthError),
}

fn now() -> u64 {
    wall_clock::now().seconds
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

fn bearer(request: &IncomingRequest) -> Option<String> {
    let headers = request.headers();
    let vals = headers.get("authorization");
    let raw = vals.first()?;
    let s = String::from_utf8(raw.clone()).ok()?;
    s.strip_prefix("Bearer ").map(|t| t.to_string())
}

fn introspect(request: &IncomingRequest) -> Result<Principal, Outcome> {
    let token =
        bearer(request).ok_or(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())))?;
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

fn body(request: &IncomingRequest) -> Result<Value, Outcome> {
    // simplified body reader
    let raw = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

/// Ceiling on a request body, matching the rest of the tree.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let b = request.consume().map_err(|_| ())?;
    let stream = b.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                // A ceiling, not a policy: past this the read stops and the
                // caller is told, rather than growing until the store's
                // memory cap traps the component and the connection just
                // closes with nothing said.
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

fn register(request: &IncomingRequest) -> Outcome {
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

fn login(request: &IncomingRequest) -> Outcome {
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

fn logout(request: &IncomingRequest) -> Outcome {
    let token = match bearer(request) {
        Some(t) => t,
        None => return Outcome::Auth(AuthError::InvalidToken("missing bearer".into())),
    };
    match session::revoke(&token) {
        Ok(()) => Outcome::Json(200, json!({ "ok": true }).to_string()),
        Err(e) => Outcome::Auth(e),
    }
}

fn me(request: &IncomingRequest) -> Outcome {
    match introspect(request) {
        Ok(p) => Outcome::Json(200, json!({ "subject": p.subject, "roles": p.roles }).to_string()),
        Err(o) => o,
    }
}

fn create_item(request: &IncomingRequest) -> Outcome {
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

fn list_items(request: &IncomingRequest) -> Outcome {
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

fn emit(response_out: ResponseOutparam, result: Outcome) {
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
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(code);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    let bytes = body.as_bytes();
    if !bytes.is_empty() {
        let stream = out.write().expect("write stream");
        let _ = stream.blocking_write_and_flush(bytes);
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
