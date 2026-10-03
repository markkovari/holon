//! Front `bytes:codec` over HTTP, so its specification runs against the ARTIFACT.
//!
//! `bytes-codec/tests/codec.rs` calls the Rust crate directly. That is a fine unit
//! test and it cannot judge a component built in another language, or one fetched by
//! digest and never built here at all. Driven through this probe, the same thirteen
//! cases judge whatever satisfies the contract — which is the precondition for both
//! polyglot components and prebuilt artifacts.
//!
//! `GET /encode?bytes=<hex>&alphabet=standard|url-safe`
//! `GET /decode?text=<pct>&alphabet=…`   `GET /to-hex?bytes=<hex>`   `GET /from-hex?text=…`
//!
//! Bytes cross as HEX in both directions, because a base64 test whose transport is
//! base64 cannot tell a bug from a round trip.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../bytes-codec/wit",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "wit",
        ],
        world: "comp:codecprobe/codec-probe",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use bindings::bytes::codec::codec;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Request, Response};
use serde_json::json;

struct Component;

guestio::guest_p3_respond!();

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

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).filter_map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn alphabet(q: &str) -> codec::Alphabet {
    match param(q, "alphabet").as_str() {
        "url-safe" => codec::Alphabet::UrlSafe,
        _ => codec::Alphabet::Standard,
    }
}

/// The error, as JSON a gate can assert on rather than a rendered sentence.
fn err(e: codec::DecodeError) -> serde_json::Value {
    match e {
        codec::DecodeError::NotInAlphabet((at, found)) => {
            json!({ "error": "not-in-alphabet", "at": at, "found": found })
        }
        codec::DecodeError::TruncatedGroup(n) => json!({ "error": "truncated-group", "length": n }),
        codec::DecodeError::MisplacedPadding(at) => {
            json!({ "error": "misplaced-padding", "at": at })
        }
    }
}

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let full = request.get_path_with_query().unwrap_or_else(|| "/".into());
        let (path, q) = full.split_once('?').unwrap_or((full.as_str(), ""));

        let body = match path {
            "/health" => json!({ "ok": true }),
            "/encode" => json!({ "text": codec::encode(&unhex(&param(q, "bytes")), alphabet(q)) }),
            "/decode" => match codec::decode(&param(q, "text"), alphabet(q)) {
                Ok(b) => json!({ "bytes": hex(&b) }),
                Err(e) => err(e),
            },
            "/to-hex" => json!({ "text": codec::to_hex(&unhex(&param(q, "bytes"))) }),
            "/from-hex" => match codec::from_hex(&param(q, "text")) {
                Ok(b) => json!({ "bytes": hex(&b) }),
                Err(e) => err(e),
            },
            _ => json!({ "error": "no such route" }),
        };

        respond(200, "application/json", body.to_string())
    }
}

bindings::export!(Component with_types_in bindings);
