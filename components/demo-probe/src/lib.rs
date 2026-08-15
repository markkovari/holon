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
use bindings::wasi::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

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
            let _ = stream.blocking_write_and_flush(b"{\"hits\":[],\"has_more\":false}");
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
