//! `secret-probe` — an instrument for `comp:secrets/reader` (see wit/probe.wit).
//! granted it (ADR-0051).

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../host/wit/deps/comp-secrets",
            "wit",
        ],
        world: "comp:secretprobe/secret-probe",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use bindings::comp::secrets::reader;

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};

guestio::guest_p3_respond!();

struct Component;

fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.replace('+', " "))
        .unwrap_or_default()
}

/// Only the two characters a secret value could plausibly carry; not a general encoder.
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let k = param(&query, "k");

        let body = match (request.get_method(), route.as_str()) {
            (Method::Get, "/has") => match reader::get(&k) {
                // Read back off the handle, not echoed: proves it carries the manifest's name, the only thing
                // about a secret that is safe to log.
                Ok(Some(s)) => {
                    format!(
                        "{{\"key\":\"{}\",\"granted\":true,\"name\":\"{}\"}}",
                        esc(&k),
                        esc(&s.key())
                    )
                }
                // Not an error: absent is also what a guest gets for a key it was not granted.
                Ok(None) => format!("{{\"key\":\"{}\",\"granted\":false}}", esc(&k)),
                Err(e) => format!("{{\"key\":\"{}\",\"error\":\"{e:?}\"}}", esc(&k)),
            },
            (Method::Get, "/reveal") => match reader::get(&k) {
                Ok(Some(s)) => match reader::reveal(&s) {
                    Ok(v) => format!("{{\"key\":\"{}\",\"value\":\"{}\"}}", esc(&k), esc(&v)),
                    Err(e) => format!("{{\"key\":\"{}\",\"error\":\"{e:?}\"}}", esc(&k)),
                },
                Ok(None) => format!("{{\"key\":\"{}\",\"granted\":false}}", esc(&k)),
                Err(e) => format!("{{\"key\":\"{}\",\"error\":\"{e:?}\"}}", esc(&k)),
            },
            _ => {
                "{\"service\":\"secret-probe\",\"routes\":[\"/has?k=\",\"/reveal?k=\"]}".to_string()
            }
        };

        respond(200, "application/json", body)
    }
}

bindings::export!(Component with_types_in bindings);
