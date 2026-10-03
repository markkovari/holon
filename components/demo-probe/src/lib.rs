//! `demo-probe` — the other half: it calls `demo:shape/pager` and nothing else.
//!
//! A stub. The goal is to answer `GET /page?size=&offset=` by calling
//! `paginate` and rendering the answer as JSON.
//!
//! This half has no held-out test of its own, and the reason is structural:
//! `cargo component test` runs a crate AS a component, and this one imports
//! `demo:shape/pager`, which nothing satisfies standalone — "a matching
//! implementation was not found in the linker". So it is judged by compiling, and
//! then by `components/demo/join.sh`, which plugs the two halves together and
//! checks that the import really was satisfied.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../demo/wit",
            "wit",
        ],
        world: "demo:probe/demo-probe",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use bindings::demo::shape::pager;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Request, Response};

guestio::guest_p3_respond!();

struct Component;

fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
        .unwrap_or_default()
}

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let query = match path.split_once('?') {
            Some((_, q)) => q.to_string(),
            None => String::new(),
        };

        let size = param(&query, "size").parse::<u32>().unwrap_or(10);
        let offset = param(&query, "offset").parse::<u32>().unwrap_or(0);

        // The corpus the goal named. The run that wrote this file passed an empty
        // list here and every check still went green — the join gate proves the
        // halves LINK, and nothing yet proves the endpoint answers anything. That
        // is the gap `.comp/goals/07` exists for.
        let ids: Vec<String> = ["a", "b", "c", "d", "e"].iter().map(|s| s.to_string()).collect();
        let page = pager::paginate(&ids, size, offset);

        let body = format!(
            "{{\"hits\":[{}],\"has_more\":{}}}",
            page.hits
                .iter()
                .map(|h| format!("\"{}\"", h.replace('\\', "\\\\").replace('"', "\\\"")))
                .collect::<Vec<_>>()
                .join(","),
            if page.has_more { "true" } else { "false" }
        );

        respond(200, "application/json", body)
    }
}

bindings::export!(Component with_types_in bindings);
