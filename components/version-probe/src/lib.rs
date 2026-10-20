//! `version-probe` — answers with the tag it was BUILT with.
//! node runs is answerable from outside, not a version field someone could set wrong.

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

/// The TAG is the artifact's identity; what the version CAN DO is loaded from config at startup.
const TAG: &str = match option_env!("COMP_VERSION_TAG") {
    Some(v) => v,
    None => "unset",
};

impl Guest for Component {
    async fn handle(_request: Request) -> Result<Response, ErrorCode> {
        // A version advertising nothing is not healthy: health is "I was given a registry and can do
        // something", reported by the running code.
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
