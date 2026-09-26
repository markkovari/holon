//! Support desk dashboard for managing user requests

#[allow(warnings)]
mod bindings;

mod api;
mod reply;
mod tickets;

use bindings::exports::wasi::http::incoming_handler::{Guest, IncomingRequest, ResponseOutparam};

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        api::handle(request, response_out)
    }
}

bindings::export!(Component with_types_in bindings);
