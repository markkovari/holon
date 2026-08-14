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
use bindings::wasi::config::store as config;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

/// Baked in at build time. `unset` when nobody passed one, which is a legitimate
/// build rather than an error — it just is not the one a version test wants.
/// The TAG is the only thing compiled in: it is the artifact's identity, so two
/// versions are different bytes and the fleet can tell them apart. What the
/// version CAN DO is not compiled in — it is loaded from config at startup.
const TAG: &str = match option_env!("COMP_VERSION_TAG") {
    Some(v) => v,
    None => "unset",
};

impl Guest for Component {
    fn handle(_request: IncomingRequest, response_out: ResponseOutparam) {
        // Capabilities are LOADED at startup from the registry the platform hands
        // this instance — `wasi:config/store`, key `capabilities`, a `name:semver`
        // list. Nothing is baked in. A version that advertises nothing (no config)
        // is not a healthy engine; health is "I was given a registry and can do
        // something", reported by the running code, not asserted by a record.
        let registry = config::get("capabilities").ok().flatten().unwrap_or_default();
        let items: Vec<(&str, &str)> = registry
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
