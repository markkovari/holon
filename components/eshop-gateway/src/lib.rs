//! eshop:gateway — the Envoy + Blazor-host stand-in: embedded storefront SPA
//! plus prefix-routed forwarding via the proxy:route capability (which owns
//! the route table and the outgoing-HTTP round trip; this component is glue).
//!
//! POST /internal/pump fans out to every consumer service's pump (the
//! /pump/* routes in the table), so one driver advances the whole
//! choreography.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../proxy-route/wit",
            "wit",
        ],
        world: "eshop:gateway/gateway-service",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use bindings::proxy::route::router;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};

// ponytail: single-file SPA (the jco-helpdesk pattern) include_str!'d here;
// switch to the static-assets component if the UI ever needs a build step.
const INDEX_HTML: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/eshop/ui/index.html"));

/// Ordering first (creates/advances), then the reactors.
const PUMPS: [&str; 4] = ["/pump/ordering", "/pump/catalog", "/pump/payment", "/pump/basket"];

guestio::guest_p3_respond!();

struct Component;

impl bindings::p3::handler::Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let method = method_str(&request.get_method());
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        match seg.as_slice() {
            ["api", ..] => {
                let mut headers = Vec::new();
                if let Some(auth) = header(&request, "authorization") {
                    headers.push(("authorization".to_string(), auth));
                }
                let body = match method.as_str() {
                    "GET" | "HEAD" => Vec::new(),
                    _ => read_body(request).await.unwrap_or_default(),
                };
                match router::forward(&method, &path, &headers, &body) {
                    Ok(up) => respond(up.status, &up.content_type, up.body),
                    Err(router::ProxyError::NoRoute) => {
                        respond(404, "application/json", b"{\"error\":\"not_found\"}")
                    }
                    Err(router::ProxyError::UpstreamUnreachable(m)) => respond(
                        502,
                        "application/json",
                        format!("{{\"error\":\"upstream unreachable: {m}\"}}").as_bytes(),
                    ),
                }
            }
            ["internal", "pump"] => {
                let ok =
                    PUMPS.iter().filter(|p| router::forward("POST", p, &[], &[]).is_ok()).count();
                let body = format!("{{\"pumped\":{ok}}}");
                respond(200, "application/json", body)
            }
            // everything else is the storefront (SPA fallback included).
            _ => respond(200, "text/html; charset=utf-8", INDEX_HTML.as_bytes()),
        }
    }
}

fn method_str(m: &Method) -> String {
    match m {
        Method::Get => "GET".into(),
        Method::Post => "POST".into(),
        Method::Put => "PUT".into(),
        Method::Delete => "DELETE".into(),
        Method::Patch => "PATCH".into(),
        Method::Head => "HEAD".into(),
        Method::Options => "OPTIONS".into(),
        Method::Trace => "TRACE".into(),
        Method::Connect => "CONNECT".into(),
        Method::Other(s) => s.clone(),
    }
}

fn header(request: &Request, name: &str) -> Option<String> {
    request.get_headers().get(name).into_iter().next().and_then(|v| String::from_utf8(v).ok())
}

/// A ceiling on a request body, not a policy: past this the read stops and the
/// caller is told, rather than growing until the store's memory cap traps the
/// component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

// No error channel on this one, so an over-long or failed body reads as
// EMPTY rather than as a plausible prefix of itself (`unwrap_or_default`).
guestio::guest_p3_read_body!(MAX_BODY_BYTES);

bindings::export!(Component with_types_in bindings);
