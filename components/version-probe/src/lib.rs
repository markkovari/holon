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

/// Write a whole body, however long it is.
///
/// `blocking-write-and-flush` accepts at most 4096 bytes and TRAPS above that
/// rather than returning an error: the component dies mid-response and the caller
/// sees `connection closed before message completed`, three layers from the cause.
/// This bit a real run — a 4573-byte contract — and cost four failed starts to
/// find, so it is written the same way everywhere now.
///
/// Not a flat 4096-byte loop: `check-write` is the stream saying how much it will
/// take right now, usually far more, so this writes in whatever bites it offers,
/// waits on the pollable when it offers none, and flushes ONCE at the end.
///
/// Returns false when the stream is gone. For an SSE loop that means the client
/// hung up, which is ordinary and not an error.
fn write_all(stream: &bindings::wasi::io::streams::OutputStream, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        let ready = match stream.check_write() {
            Ok(0) => {
                stream.subscribe().block();
                continue;
            }
            Ok(n) => n as usize,
            Err(_) => return false,
        };
        let take = ready.min(bytes.len());
        if stream.write(&bytes[..take]).is_err() {
            return false;
        }
        bytes = &bytes[take..];
    }
    stream.blocking_flush().is_ok()
}

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
        let map =
            items.iter().map(|(n, v)| format!("\"{n}\":\"{v}\"")).collect::<Vec<_>>().join(",");
        let body = format!(
            "{{\"tag\":\"{TAG}\",\"healthy\":{healthy},\"capability_count\":{},\"capabilities\":{{{map}}}}}",
            items.len()
        );
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
