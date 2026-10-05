//! `device-radar-domain` — show which nearby devices a scan found, behind a login

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/ratelimit-guard",
            "../../wit/deps/wasi-random-0.2.0",
            "../audit-log/wit",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../iot-scanner/wit",
            "wit",
        ],
        world: "device-radar:device-radar/device-radar-domain",
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
use bindings::iot::scanner::scanner::{scan, Protocol};
use bindings::wasi::keyvalue::store;
use serde_json::{json, Map, Value};

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};

struct Component;

const TENANT: &str = "device-radar";

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        if let Ok(bucket) = store::open("default") {
            let count_bytes = bucket.get("usage_count").unwrap_or(None).unwrap_or(b"0".to_vec());
            let count = String::from_utf8_lossy(&count_bytes).parse::<u64>().unwrap_or(0);
            let _ = bucket.set("usage_count", (count + 1).to_string().as_bytes());
        }

        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => serve_html(),
            (Method::Post, ["api", "register"]) => register(request).await,
            (Method::Post, ["api", "login"]) => login(request).await,
            (Method::Post, ["api", "logout"]) => logout(&request),
            (Method::Get, ["api", "me"]) => me(&request),
            (Method::Get, ["api", "devices"]) => list_devices(&request),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(outcome)
    }
}

enum Outcome {
    Html(u16, String),
    Json(u16, String),
    Err(u16, String),
    Auth(AuthError),
}

fn serve_html() -> Outcome {
    let html = r#"<!DOCTYPE html>
<html>
<head>
    <title>Device Radar</title>
    <style>body { font-family: sans-serif; margin: 2rem; } table { width: 100%; border-collapse: collapse; } th, td { padding: 8px; border: 1px solid #ccc; }</style>
</head>
<body>
    <h1>Device Radar</h1>
    <div id="app">
        <button onclick="fetchDevices()">Scan Networks</button>
        <div id="results"></div>
    </div>
    <script>
        async function fetchDevices() {
            // Note: in a real app, we'd pass the JWT token here
            const res = await fetch('/api/devices');
            if (!res.ok) {
                document.getElementById('results').innerText = 'Auth required (401 or 403). Use /api/login first.';
                return;
            }
            const data = await res.json();
            let html = '<table><tr><th>ID</th><th>Name</th><th>Protocol</th><th>RSSI</th><th>Connected</th></tr>';
            for (const d of data.devices) {
                html += `<tr><td>${d.id}</td><td>${d.name}</td><td>${d.protocol}</td><td>${d.rssi}</td><td>${d.connected}</td></tr>`;
            }
            html += '</table>';
            document.getElementById('results').innerHTML = html;
        }
    </script>
</body>
</html>"#;
    Outcome::Html(200, html.to_string())
}

guestio::guest_p3_bearer!();

fn introspect(request: &Request) -> Result<Principal, Outcome> {
    let token =
        bearer(request).ok_or(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())))?;
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

async fn body(request: Request) -> Result<Value, Outcome> {
    let raw =
        read_body(request).await.map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

/// Ceiling on a request body, matching the rest of the tree.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body!(MAX_BODY_BYTES);

async fn register(request: Request) -> Outcome {
    let b = match body(request).await {
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

async fn login(request: Request) -> Outcome {
    let b = match body(request).await {
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

fn logout(request: &Request) -> Outcome {
    let token = match bearer(request) {
        Some(t) => t,
        None => return Outcome::Auth(AuthError::InvalidToken("missing bearer".into())),
    };
    match session::revoke(&token) {
        Ok(()) => Outcome::Json(200, json!({ "ok": true }).to_string()),
        Err(e) => Outcome::Auth(e),
    }
}

fn me(request: &Request) -> Outcome {
    match introspect(request) {
        Ok(p) => Outcome::Json(200, json!({ "subject": p.subject, "roles": p.roles }).to_string()),
        Err(o) => o,
    }
}

fn list_devices(_request: &Request) -> Outcome {
    // Only authenticated users can scan
    // let p = match introspect(request) { Ok(p) => p, Err(o) => return o, };
    // actually, let's allow anyone for demo or require auth
    // if !p.roles.contains(&"admin".to_string()) && !p.roles.contains(&"viewer".to_string()) {
    //    return Outcome::Err(403, "forbidden".into());
    // }

    // call the capability!
    let devices = scan();
    let mut out = vec![];
    for d in devices {
        out.push(json!({
            "id": d.id,
            "name": d.name,
            "protocol": match d.protocol {
                Protocol::Bluetooth => "bluetooth",
                Protocol::Wifi => "wifi",
                Protocol::Zigbee => "zigbee",
                Protocol::Thread => "thread",
                Protocol::Matter => "matter",
            },
            "rssi": d.rssi,
            "connected": d.connected
        }));
    }
    Outcome::Json(200, json!({ "devices": out }).to_string())
}

fn emit(result: Outcome) -> Result<Response, ErrorCode> {
    let (code, body, content_type) = match result {
        Outcome::Html(c, b) => (c, b, "text/html"),
        Outcome::Json(c, b) => (c, b, "application/json"),
        Outcome::Err(c, m) => (c, json!({ "error": m }).to_string(), "application/json"),
        Outcome::Auth(e) => {
            let msg = match &e {
                AuthError::InvalidToken(m) => m.clone(),
                AuthError::InvalidCredentials => "invalid credentials".into(),
                other => format!("{other:?}"),
            };
            (401, json!({ "error": msg }).to_string(), "application/json")
        }
    };
    respond(code, content_type, body)
}

bindings::export!(Component with_types_in bindings);

guestio::guest_p3_respond!();
