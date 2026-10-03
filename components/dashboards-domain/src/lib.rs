//! `dashboards-domain` — personal metric dashboards (docs/apps/DASHBOARDS.md) as ONE
//! composed wasm HTTP component. Exports `wasi:http`; imports only WIT contracts:
//! the composed auth-guard (`auth:identity`), `records:store`, and `svg:chart`
//! to render each panel's series to an SVG on the server — the frontend carries
//! no charting library. No bespoke auth, storage, or chart renderer.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/wasi-random-0.2.0",
            "../audit-log/wit",
            "../../wit/deps/ratelimit-guard",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../svg-chart/wit",
            "wit",
        ],
        world: "dashboards:app/dashboards-domain",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use serde_json::{json, Map, Value};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::p3::clocks::system_clock;
use bindings::records::store::store as records;
use bindings::svg::chart::charts as svg;

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};

struct Component;

/// What the handlers need from a request, taken before its body was consumed.
struct Req {
    bearer: Option<String>,
    raw: Result<Vec<u8>, ()>,
}

const TENANT: &str = "dashboards";
const DASHBOARDS: &str = "dashboards";
const PANELS: &str = "panels";

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
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
            (Method::Get, [""]) => usage(),
            (Method::Post, ["api", "register"]) => register(&request),
            (Method::Post, ["api", "login"]) => login(&request),
            (Method::Post, ["api", "logout"]) => logout(&request),
            (Method::Get, ["api", "me"]) => me(&request),

            (Method::Post, ["api", "dashboards"]) => create_dashboard(&request),
            (Method::Get, ["api", "dashboards"]) => list_dashboards(&request),
            (Method::Get, ["api", "dashboards", id]) => get_dashboard(&request, id),
            (Method::Post, ["api", "dashboards", id, "panels"]) => add_panel(&request, id),
            (Method::Delete, ["api", "panels", id]) => delete_panel(&request, id),
            (Method::Get, ["api", "panels", id, "chart.svg"]) => panel_chart(&request, id),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(outcome)
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
    Auth(AuthError),
    File(u16, String, Vec<u8>),
}

fn now() -> u64 {
    system_clock::now().seconds as u64
}

fn usage() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "dashboards",
            "about": "personal metric dashboards — panels rendered to SVG charts on the server via svg:chart (no client-side charting library)",
            "auth": "POST /api/register|login|logout, GET /api/me",
            "dashboards": "POST|GET /api/dashboards {name}, GET /api/dashboards/{id}",
            "panels": "POST /api/dashboards/{id}/panels {title, kind: bar|line|donut|sparkline, data:[{label,value,color?}]}, GET /api/panels/{id}/chart.svg"
        })
        .to_string(),
    )
}

// ---- auth -------------------------------------------------------------------

guestio::guest_p3_bearer!();
guestio::guest_p3_respond!();

fn introspect(request: &Req) -> Result<Principal, Outcome> {
    let token = request
        .bearer
        .clone()
        .ok_or(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())))?;
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

fn register(request: &Req) -> Outcome {
    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = body["email"].as_str().unwrap_or("").trim().to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();
    let p = match accounts::register(&email, &password, TENANT) {
        Ok(p) => p,
        Err(e) => return Outcome::Auth(e),
    };
    seed_demo(&p.subject);
    Outcome::Json(201, json!({ "subject": p.subject }).to_string())
}

fn login(request: &Req) -> Outcome {
    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = body["email"].as_str().unwrap_or("").trim().to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();
    match accounts::login(&email, &password, TENANT) {
        Ok(tp) => Outcome::Json(
            200,
            json!({ "access_token": tp.access_token, "refresh_token": tp.refresh_token, "expires_in": tp.expires_in, "session_id": tp.session_id }).to_string(),
        ),
        Err(e) => Outcome::Auth(e),
    }
}

fn me(request: &Req) -> Outcome {
    match introspect(request) {
        Ok(p) => Outcome::Json(200, json!({ "subject": p.subject, "roles": p.roles }).to_string()),
        Err(o) => o,
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

// ---- dashboards + panels ----------------------------------------------------

fn create_dashboard(request: &Req) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let name = b["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return Outcome::Err(422, "name required".into());
    }
    let d = json!({ "id": Value::Null, "name": name, "owner": p.subject, "created": now() });
    match records::create(DASHBOARDS, &d.to_string(), &["owner".to_string()]) {
        Ok(rec) => Outcome::Json(201, hydrate(&rec.id, &rec.data)),
        Err(e) => store_err(e),
    }
}

fn dashboard(id: &str) -> Option<Value> {
    records::get(DASHBOARDS, id).ok().and_then(|e| serde_json::from_str::<Value>(&e.data).ok()).map(
        |mut v| {
            v["id"] = json!(id);
            v
        },
    )
}

fn owns_dashboard(p: &Principal, id: &str) -> Option<Value> {
    dashboard(id).filter(|d| d["owner"].as_str() == Some(&p.subject))
}

fn list_dashboards(request: &Req) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let mut items: Vec<Value> =
        records::find_by(DASHBOARDS, "owner", &json!(p.subject).to_string())
            .unwrap_or_default()
            .iter()
            .filter_map(|e| serde_json::from_str::<Value>(&hydrate(&e.id, &e.data)).ok())
            .collect();
    items.sort_by_key(|d| d["created"].as_u64().unwrap_or(0));
    Outcome::Json(200, json!({ "items": items }).to_string())
}

fn panels_of(dashboard_id: &str) -> Vec<Value> {
    let mut v: Vec<Value> = records::find_by(PANELS, "dashboard", &json!(dashboard_id).to_string())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&hydrate(&e.id, &e.data)).ok())
        .collect();
    v.sort_by_key(|pn| pn["created"].as_u64().unwrap_or(0));
    v
}

fn get_dashboard(request: &Req, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let d = match owns_dashboard(&p, id) {
        Some(d) => d,
        None => return Outcome::Err(404, "not_found".into()),
    };
    Outcome::Json(200, json!({ "dashboard": d, "panels": panels_of(id) }).to_string())
}

fn add_panel(request: &Req, dashboard_id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if owns_dashboard(&p, dashboard_id).is_none() {
        return Outcome::Err(404, "no such dashboard".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let title = b["title"].as_str().unwrap_or("").trim().to_string();
    let kind = b["kind"].as_str().unwrap_or("bar").to_string();
    if !["bar", "line", "donut", "sparkline"].contains(&kind.as_str()) {
        return Outcome::Err(422, "kind must be bar|line|donut|sparkline".into());
    }
    let data = b["data"].as_array().cloned().unwrap_or_default();
    if data.is_empty() {
        return Outcome::Err(422, "data must be a non-empty [{label,value}]".into());
    }
    let d = json!({ "id": Value::Null, "dashboard": dashboard_id, "title": title, "kind": kind, "data": data, "created": now() });
    match records::create(PANELS, &d.to_string(), &["dashboard".to_string()]) {
        Ok(rec) => Outcome::Json(201, hydrate(&rec.id, &rec.data)),
        Err(e) => store_err(e),
    }
}

fn panel(id: &str) -> Option<Value> {
    records::get(PANELS, id).ok().and_then(|e| serde_json::from_str::<Value>(&e.data).ok()).map(
        |mut v| {
            v["id"] = json!(id);
            v
        },
    )
}

/// The panel, if the caller owns its dashboard.
fn owned_panel(p: &Principal, id: &str) -> Option<Value> {
    let pn = panel(id)?;
    let dash = pn["dashboard"].as_str()?;
    owns_dashboard(p, dash).map(|_| pn)
}

fn delete_panel(request: &Req, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if owned_panel(&p, id).is_none() {
        return Outcome::Err(404, "not_found".into());
    }
    let _ = records::delete(PANELS, id);
    Outcome::Json(200, json!({ "ok": true }).to_string())
}

fn panel_chart(request: &Req, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let pn = match owned_panel(&p, id) {
        Some(pn) => pn,
        None => return Outcome::Err(404, "not_found".into()),
    };
    let svg_doc = svg::render(&chart_of(&pn));
    Outcome::File(200, "image/svg+xml".into(), svg_doc.into_bytes())
}

// ---- svg:chart bridge -------------------------------------------------------

fn kind_of(s: &str) -> svg::Kind {
    match s {
        "line" => svg::Kind::Line,
        "donut" => svg::Kind::Donut,
        "sparkline" => svg::Kind::Sparkline,
        _ => svg::Kind::Bar,
    }
}

/// Build the svg:chart request from a stored panel.
fn chart_of(pn: &Value) -> svg::Chart {
    let data = pn["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|d| svg::Slice {
                    label: d["label"].as_str().unwrap_or("").to_string(),
                    value: d["value"].as_f64().unwrap_or(0.0),
                    color: d["color"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    svg::Chart {
        kind: kind_of(pn["kind"].as_str().unwrap_or("bar")),
        title: pn["title"].as_str().unwrap_or("").to_string(),
        data,
        width: 0,
        height: 0,
    }
}

/// Seed a fresh account with a demo dashboard so the app is never empty.
fn seed_demo(subject: &str) {
    let d =
        json!({ "id": Value::Null, "name": "Demo dashboard", "owner": subject, "created": now() });
    let dash = match records::create(DASHBOARDS, &d.to_string(), &["owner".to_string()]) {
        Ok(r) => r.id,
        Err(_) => return,
    };
    let panels = json!([
        { "title": "Hours by project", "kind": "bar",
          "data": [{"label":"Web","value":42},{"label":"Ops","value":28},{"label":"Sales","value":15},{"label":"Design","value":9}] },
        { "title": "Effort split", "kind": "donut",
          "data": [{"label":"Web","value":42},{"label":"Ops","value":28},{"label":"Sales","value":15},{"label":"Design","value":9}] },
        { "title": "This week", "kind": "line",
          "data": [{"label":"Mon","value":3},{"label":"Tue","value":7},{"label":"Wed","value":5},{"label":"Thu","value":9},{"label":"Fri","value":6},{"label":"Sat","value":2},{"label":"Sun","value":4}] },
        { "title": "Signups (30d)", "kind": "sparkline",
          "data": [{"label":"1","value":4},{"label":"2","value":6},{"label":"3","value":5},{"label":"4","value":9},{"label":"5","value":8},{"label":"6","value":12},{"label":"7","value":11}] }
    ]);
    for pn in panels.as_array().unwrap() {
        let mut d = pn.clone();
        d["id"] = Value::Null;
        d["dashboard"] = json!(dash);
        d["created"] = json!(now());
        let _ = records::create(PANELS, &d.to_string(), &["dashboard".to_string()]);
    }
}

// ---- http plumbing ----------------------------------------------------------

/// Attach the record id to its stored JSON.
fn hydrate(id: &str, data: &str) -> String {
    let mut v: Value = serde_json::from_str(data).unwrap_or_else(|_| json!({}));
    v["id"] = json!(id);
    v.to_string()
}

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::Err(404, "not_found".into()),
        records::StoreError::InvalidJson(m) => Outcome::Err(422, m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn body(request: &Req) -> Result<Value, Outcome> {
    let raw = request.raw.clone().map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
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

guestio::guest_p3_read_body!(MAX_BODY_BYTES);

fn emit(result: Outcome) -> Result<Response, ErrorCode> {
    if let Outcome::File(code, ctype, bytes) = result {
        return respond_cors(code, &ctype, bytes);
    }
    let (code, body) = match result {
        Outcome::Json(c, b) => (c, b),
        Outcome::Err(c, m) => (c, json!({ "error": m }).to_string()),
        Outcome::Auth(e) => {
            let msg = match &e {
                AuthError::InvalidToken(m) => m.clone(),
                AuthError::InvalidCredentials => "invalid credentials".into(),
                other => format!("{other:?}"),
            };
            (401, json!({ "error": msg }).to_string())
        }
        Outcome::File(..) => unreachable!(),
    };
    respond_cors(code, "application/json", body.into_bytes())
}

fn respond_cors(status: u16, ctype: &str, body: Vec<u8>) -> Result<Response, ErrorCode> {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[ctype.as_bytes().to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    respond_with(status, headers, body)
}

bindings::export!(Component with_types_in bindings);
