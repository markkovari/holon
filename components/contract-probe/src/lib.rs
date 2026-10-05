//! `contract-probe` — the door onto `contract:registry` (see wit/probe.wit).
//!
//!   POST /publish                                   body is the contract
//!   GET  /current
//!   GET  /get?version=
//!   GET  /proposed?part=
//!   POST /ask?from=&to=&subject=&at=                body is what is being asked for
//!   GET  /pending?part=
//!   POST /answer?id=&verdict=granted|denied|counter body is the amendment or the reason
//!   POST /ratify?version=&part=&score=
//!   POST /built-against?candidate=&part=&version=
//!   GET  /composable?candidates=a,b
//!
//! Every route answers JSON, and a refusal is `{"error":"refused","detail":"…"}`
//! with a **200** — the same choice `graph-probe` and `memory-probe` made, for the
//! same reason: what is under test is what the registry decided, and a status code
//! would flatten "this part does not own that version" into the same shape as "the
//! host refused the link".

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../host/wit/deps/comp-secrets",
            "../knowledge-graph/wit",
            "../contract-registry/wit",
            "wit",
        ],
        world: "comp:contractprobe/contract-probe",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use bindings::contract::registry::registry as reg;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};

struct Component;

fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent(v))
        .unwrap_or_default()
}

use guestfmt::percent_decode as percent;

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn num(query: &str, key: &str) -> u32 {
    param(query, key).parse().unwrap_or(0)
}

fn signed(query: &str, key: &str) -> i32 {
    param(query, key).parse().unwrap_or(0)
}

fn verdict_of(name: &str) -> reg::Verdict {
    match name {
        "granted" => reg::Verdict::Granted,
        "counter" => reg::Verdict::Counter,
        // Anything unspelled is a refusal, never a grant — the same rule the
        // registry applies when it reads a verdict back.
        _ => reg::Verdict::Denied,
    }
}

fn verdict_name(v: reg::Verdict) -> &'static str {
    match v {
        reg::Verdict::Granted => "granted",
        reg::Verdict::Denied => "denied",
        reg::Verdict::Counter => "counter",
    }
}

fn err(e: reg::RegistryError) -> String {
    let (kind, msg) = match e {
        reg::RegistryError::Rejected(m) => ("rejected", m),
        reg::RegistryError::Unavailable(m) => ("unavailable", m),
        reg::RegistryError::Refused(m) => ("refused", m),
    };
    format!("{{\"error\":\"{kind}\",\"detail\":\"{}\"}}", esc(&msg))
}

fn contract_json(c: &reg::Contract) -> String {
    format!(
        "{{\"version\":{},\"body\":\"{}\",\"canonical\":{},\"owner\":\"{}\",\"from_request\":\"{}\"}}",
        c.version,
        esc(&c.body),
        c.canonical,
        esc(&c.owner),
        esc(&c.from_request)
    )
}

fn request_json(r: &reg::Request) -> String {
    format!(
        "{{\"id\":\"{}\",\"from_part\":\"{}\",\"to_part\":\"{}\",\"subject\":\"{}\",\"body\":\"{}\",\
         \"at_version\":{},\"answered\":{},\"verdict\":\"{}\",\"answer\":\"{}\"}}",
        esc(&r.id),
        esc(&r.from_part),
        esc(&r.to_part),
        esc(&r.subject),
        esc(&r.body),
        r.at_version,
        r.answered,
        verdict_name(r.verdict),
        esc(&r.answer)
    )
}

/// A ceiling on a request body, not a policy: past this the read gives up and
/// the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);
guestio::guest_p3_respond!();

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let method = request.get_method();

        let body = match (&method, route.as_str()) {
            (Method::Post, "/publish") => match reg::publish(&read_body(request).await) {
                Ok(v) => format!("{{\"version\":{v}}}"),
                Err(e) => err(e),
            },

            (Method::Get, "/current") => match reg::current() {
                Ok(c) => contract_json(&c),
                Err(e) => err(e),
            },

            (Method::Get, "/get") => match reg::get(num(&query, "version")) {
                Ok(Some(c)) => contract_json(&c),
                // Absence is an answer.
                Ok(None) => "{\"found\":false}".to_string(),
                Err(e) => err(e),
            },

            (Method::Get, "/proposed") => match reg::proposed(&param(&query, "part")) {
                Ok(Some(c)) => contract_json(&c),
                Ok(None) => "{\"found\":false}".to_string(),
                Err(e) => err(e),
            },

            (Method::Post, "/ask") => {
                let body = read_body(request).await;
                match reg::ask(
                    &param(&query, "from"),
                    &param(&query, "to"),
                    &param(&query, "subject"),
                    &body,
                    num(&query, "at"),
                ) {
                    Ok(id) => format!("{{\"id\":\"{}\"}}", esc(&id)),
                    Err(e) => err(e),
                }
            }

            (Method::Get, "/pending") => match reg::pending(&param(&query, "part")) {
                Ok(rs) => format!(
                    "{{\"requests\":[{}]}}",
                    rs.iter().map(request_json).collect::<Vec<_>>().join(",")
                ),
                Err(e) => err(e),
            },

            (Method::Post, "/answer") => {
                let v = verdict_of(&param(&query, "verdict"));
                let body = read_body(request).await;
                match reg::answer(&param(&query, "id"), v, &body) {
                    // 0 means no new version: a denial and a counter change
                    // nothing about what the parts build against.
                    Ok(version) => format!("{{\"version\":{version}}}"),
                    Err(e) => err(e),
                }
            }

            (Method::Post, "/ratify") => match reg::ratify(
                num(&query, "version"),
                &param(&query, "part"),
                signed(&query, "score"),
            ) {
                Ok(()) => "{\"ok\":true}".to_string(),
                Err(e) => err(e),
            },

            (Method::Post, "/built-against") => match reg::built_against(
                &param(&query, "candidate"),
                &param(&query, "part"),
                num(&query, "version"),
            ) {
                Ok(()) => "{\"ok\":true}".to_string(),
                Err(e) => err(e),
            },

            (Method::Get, "/composable") => {
                let candidates: Vec<String> = param(&query, "candidates")
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                match reg::composable(&candidates) {
                    // An empty list is the yes. Reported as a list rather than a
                    // boolean because "no" without saying which part is on which
                    // version sends the reader to the wrong file.
                    Ok(problems) => format!(
                        "{{\"composable\":{},\"problems\":[{}]}}",
                        problems.is_empty(),
                        problems
                            .iter()
                            .map(|p| format!("\"{}\"", esc(p)))
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                    Err(e) => err(e),
                }
            }

            _ => "{\"service\":\"contract-probe\",\"routes\":[\"/publish\",\"/current\",\"/get\",\"/proposed\",\"/ask\",\"/pending\",\"/answer\",\"/ratify\",\"/built-against\",\"/composable\"]}"
                .to_string(),
        };

        respond(200, "application/json", body)
    }
}

bindings::export!(Component with_types_in bindings);
