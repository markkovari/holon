//! `version-probe` — answers with the tag it was BUILT with.
//!
//! `option_env!` is resolved at compile time, so `COMP_VERSION_TAG=alpha` and
//! `=beta` produce different bytes and therefore different digests. That is the
//! whole point: it makes "which build is this node actually running" a question
//! answerable from outside, rather than a version field somebody could set by
//! hand and be wrong about.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "wit",
        ],
        world: "comp:versionprobe/version-probe",
        generate_all,
    });
    /// Stable names for the p3 `wasi:http` (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http::handler;
        pub use super::wasi::http;
    }
}

use bindings::p3::handler::Guest;
use bindings::wasi::config::store as config;
use bindings::p3::http::types::{ErrorCode, Request, Response};

guestio::guest_p3_respond!();

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
    async fn handle(_request: Request) -> Result<Response, ErrorCode> {
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
        respond(200, "application/json", body)
    }
}

bindings::export!(Component with_types_in bindings);
