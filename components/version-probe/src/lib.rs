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
/// comma-separated `name:version` list (a bare `name` is version 1), so a version
/// that can do MORE — a new capability, or a higher version of one it already had
/// — is genuinely different bytes, read from outside over the lattice rather than
/// trusted from a record.
const CAPS: &str = match option_env!("COMP_CAPS") {
    Some(v) => v,
    None => "",
};

impl Guest for Component {
    fn handle(_request: IncomingRequest, response_out: ResponseOutparam) {
        // The self-eval, run by the version that is actually running: parse the
        // capability→version map and judge health. A build that advertises nothing
        // is not a healthy engine — health here is "I initialised and I can do
        // something", reported by the running code, not asserted by whoever
        // deployed it.
        let items: Vec<(&str, u32)> = CAPS
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|item| match item.split_once(':') {
                Some((name, ver)) => (name, ver.parse::<u32>().unwrap_or(1)),
                None => (item, 1),
            })
            .collect();
        let healthy = !items.is_empty();
        let map = items.iter().map(|(n, v)| format!("\"{n}\":{v}")).collect::<Vec<_>>().join(",");
        let body = format!(
            "{{\"tag\":\"{TAG}\",\"healthy\":{healthy},\"capability_count\":{},\"capabilities\":{{{map}}}}}",
            items.len()
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
