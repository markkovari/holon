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
/// comma-separated `name:semver` list (a bare `name` is `1.0.0`), so a version
/// that can do MORE — a new capability, or a higher semver of one it already had
/// — is genuinely different bytes, read from outside over the lattice rather than
/// trusted from a record.
const CAPS_ENV: &str = match option_env!("COMP_CAPS") {
    Some(v) => v,
    None => "",
};

/// The real manifest, baked in from source at compile time. This is what closes
/// the loop honestly: the bytes that deploy report the capabilities the loop
/// actually wrote, not a list handed in at build time. `COMP_CAPS` still wins
/// when set, for reproducible gate runs.
const MANIFEST: &str = include_str!("../../capman/capabilities.txt");

impl Guest for Component {
    fn handle(_request: IncomingRequest, response_out: ResponseOutparam) {
        // The self-eval, run by the version that is actually running: parse the
        // capability→version map and judge health. A build that advertises nothing
        // is not a healthy engine — health here is "I initialised and I can do
        // something", reported by the running code, not asserted by whoever
        // deployed it.
        // The manifest from source, unless COMP_CAPS overrides for a gate run.
        // Strip comment LINES first, THEN split on commas — so a comment that
        // contains a comma cannot leak its tail as a bogus capability. The
        // manifest is one entry per line; COMP_CAPS is one comma-separated line.
        let src = if CAPS_ENV.is_empty() { MANIFEST } else { CAPS_ENV };
        let items: Vec<(&str, &str)> = src
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .flat_map(|l| l.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|item| match item.split_once(':') {
                Some((name, ver)) => (name.trim(), ver.trim()),
                None => (item, "1.0.0"),
            })
            .collect();
        let healthy = !items.is_empty();
        let map = items.iter().map(|(n, v)| format!("\"{n}\":\"{v}\"")).collect::<Vec<_>>().join(",");
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
