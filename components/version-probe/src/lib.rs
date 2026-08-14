//! `version-probe` — answers with the tag it was BUILT with.
//!
//! `option_env!` is resolved at compile time, so `COMP_VERSION_TAG=alpha` and
//! `=beta` produce different bytes and therefore different digests. That is the
//! whole point: it makes "which build is this node actually running" a question
//! answerable from outside, rather than a version field somebody could set by
//! hand and be wrong about.

#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

/// Baked in at build time. `unset` when nobody passed one, which is a legitimate
/// build rather than an error — it just is not the one a version test wants.
const TAG: &str = match option_env!("COMP_VERSION_TAG") {
    Some(v) => v,
    None => "unset",
};

/// The capabilities this build advertises, baked in at compile time as a
/// comma-separated list, so a version that CAN do more is genuinely different
/// bytes — and what a running version can do is read from outside, over the
/// lattice, not trusted from a record.
const CAPS: &str = match option_env!("COMP_CAPS") {
    Some(v) => v,
    None => "",
};

impl Guest for Component {
    fn handle(_request: IncomingRequest, response_out: ResponseOutparam) {
        // The self-eval, run by the version that is actually running: collect the
        // capabilities and judge health. A build that advertises nothing is not a
        // healthy engine — health here is "I initialised and I can do something",
        // reported by the running code rather than asserted by whoever deployed it.
        let caps: Vec<&str> = CAPS.split(',').filter(|s| !s.is_empty()).collect();
        let healthy = !caps.is_empty();
        let list = caps.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(",");
        let body = format!(
            "{{\"tag\":\"{TAG}\",\"healthy\":{healthy},\"capability_count\":{},\"capabilities\":[{list}]}}",
            caps.len()
        );
        let headers = Fields::new();
        let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(200);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            let _ = stream.blocking_write_and_flush(body.as_bytes());
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
