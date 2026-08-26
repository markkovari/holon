//! Front the `slug` capability over HTTP: `GET /slugify?text=<pct-encoded>` calls
//! the real slug component's `slugify` and returns the result. Deployed composed
//! with slug, this makes the capability answer over the lattice.
#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::slug::generate::generator as slug;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

guestio::guest_write_all!();

struct Component;

/// Percent-decode a query value (handles `+` and multi-byte `%XX`), so a request
/// can carry "Café Déjà" and the slug component sees the real UTF-8.
fn pct(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 3 <= b.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(v) => {
                    out.push(v);
                    i += 3;
                }
                Err(_) => {
                    out.push(b[i]);
                    i += 1;
                }
            },
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn param(q: &str, k: &str) -> String {
    q.split('&')
        .find_map(|kv| kv.split_once('=').filter(|(kk, _)| *kk == k).map(|(_, v)| pct(v)))
        .unwrap_or_default()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let body = match (request.method(), route.as_str()) {
            (Method::Get, "/slugify") => {
                // The real capability, invoked over the lattice.
                let slug = slug::slugify(&param(&query, "text"));
                format!("{{\"slug\":\"{}\"}}", esc(&slug))
            }
            // A VERSIONED feature: bounded slugs via the real slug's slugify-with.
            // Present only when built with COMP_SLUGWITH, so a baseline build
            // genuinely lacks it and a candidate genuinely gains it — a behavioral
            // difference the gate can see by CALLING it over the lattice.
            (Method::Get, "/slugify-with") if option_env!("COMP_SLUGWITH").is_some() => {
                let max = param(&query, "max").parse().unwrap_or(0);
                let opts = slug::Options { separator: "-".to_string(), max_length: max };
                let slug = slug::slugify_with(&param(&query, "text"), &opts);
                format!("{{\"slug\":\"{}\"}}", esc(&slug))
            }
            _ => "{\"service\":\"slug-probe\",\"routes\":[\"/slugify?text=\"]}".to_string(),
        };
        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(200);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            let _ = write_all(&stream, body.as_bytes());
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
