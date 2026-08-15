//! `demo-probe` — the other half: it calls `demo:shape/pager` and nothing else.
//!
//! A stub. The goal is to answer `GET /page?size=&offset=` by calling
//! `paginate`, and to render the answer with `page_json` — which
//! `tests/page_json.rs` judges.

#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

/// Render a page as JSON. A plain function so the held-out test can reach it.
pub fn page_json(_hits: &[String], _has_more: bool) -> String {
    String::new()
}

struct Component;

impl Guest for Component {
    fn handle(_request: IncomingRequest, response_out: ResponseOutparam) {
        let headers = Fields::new();
        let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(200);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            let _ = stream.blocking_write_and_flush(page_json(&[], false).as_bytes());
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
