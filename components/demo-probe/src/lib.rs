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
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::demo::shape::pager;
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

fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
        .unwrap_or_default()
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
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
        let ids: Vec<String> =
            ["a", "b", "c", "d", "e"].iter().map(|s| s.to_string()).collect();
        let page = pager::paginate(&ids, size, offset);

        let body = format!(
            "{{\"hits\":[{}],\"has_more\":{}}}",
            page
                .hits
                .iter()
                .map(|h| format!("\"{}\"", h.replace('\\', "\\\\").replace('"', "\\\"")))
                .collect::<Vec<_>>()
                .join(","),
            if page.has_more { "true" } else { "false" }
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
