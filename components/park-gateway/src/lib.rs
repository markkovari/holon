//! `park-gateway` — the parking lot as HTTP routes, for agents and tests
//! (ADR-0100).
//!
//! It imports `holon:park/lot` and serves each function at `POST
//! /v1/<function>` with the same JSON `comp-park` takes (`holon_park::wire`):
//! parse the body into the contract's records, make ONE import call, write the
//! result back. Composed with `park-store`, the chain is
//!
//!   agent → comp-host (park-gateway ⊕ park-store) → comp-park → NATS JetStream
//!
//! and every request here is a real round trip through the component model —
//! the same reasoning `vcs-gateway`'s doc gives for why an e2e suite drives a
//! store through this, not through the library.
//!
//! Errors are `{error, detail, message}` with the status `holon_park::wire`
//! gives each `park-error` case, exactly as the daemon sends them.
//!
//! No auth of its own: whoever can reach this may park, wake, or cancel any
//! session's ticket. It is a tailnet app and meant for agents on that network,
//! the same reasoning `apps/vcs.toml` gives.

#[allow(warnings)]
mod bindings;
#[path = "../../park-store/src/witconv.rs"]
mod witconv;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::holon::park::lot as store;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

use holon_park::wire;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use witconv::{ToModel, ToWit};

/// The binding module `witconv` is compiled against.
mod wit {
    pub use crate::bindings::holon::park::types as t;
}
use wit::t;

/// Generous, and irrelevant in practice: every body here is small JSON
/// (a ticket id, a call description), never a file tree.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

struct Component;

type Answer = (u16, Value);

fn to_json<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn refusal(e: t::ParkError) -> Answer {
    let body = wire::ErrorBody::from(&e.to_model());
    (wire::status_of(&body.error), to_json(&body))
}

/// The import's result, as the daemon would have written it.
fn answer<W: ToModel>(r: Result<W, t::ParkError>) -> Answer
where
    W::Out: Serialize,
{
    match r {
        Ok(v) => (200, to_json(&v.to_model())),
        Err(e) => refusal(e),
    }
}

fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, Answer> {
    serde_json::from_slice(body)
        .map_err(|e| (400, to_json(&wire::ErrorBody::new("bad-request", e.to_string()))))
}

/// One contract function, by its WIT name, over a JSON body.
fn dispatch(func: &str, body: &[u8]) -> Answer {
    match route(func, body) {
        Ok(a) | Err(a) => a,
    }
}

fn route(func: &str, body: &[u8]) -> Result<Answer, Answer> {
    Ok(match func {
        "park" => {
            let r: wire::ParkRequest = parse(body)?;
            answer(store::park(&r.session, &r.call.to_wit(), &r.by.to_wit()))
        }
        "wake" => {
            let r: wire::WakeRequest = parse(body)?;
            match store::wake(&r.correlation, &r.answer.to_wit()) {
                Ok(ticket) => (200, to_json(&ticket)),
                Err(e) => refusal(e),
            }
        }
        "pending" => {
            let r: wire::Session = parse(body)?;
            answer(store::pending(&r.session))
        }
        "take-ready" => {
            let r: wire::Ticket = parse(body)?;
            answer(store::take_ready(&r.ticket))
        }
        "cancel" => {
            let r: wire::CancelRequest = parse(body)?;
            answer(store::cancel(&r.ticket, &r.by.to_wit()))
        }
        "oplog" => {
            let r: wire::OplogRequest = parse(body)?;
            answer(store::oplog(&r.session, r.after, r.limit))
        }
        other => (404, to_json(&wire::ErrorBody::new("not-found", format!("no route /v1/{other}")))),
    })
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route_path = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route_path.trim_matches('/').split('/').collect();
        let (status, body) = match (&method, seg.as_slice()) {
            (Method::Get, ["health"]) => (200, json!({ "ok": true })),
            (Method::Get, [""]) | (Method::Get, ["v1"]) => (
                200,
                json!({
                    "service": "holon:park gateway (ADR-0100)",
                    "routes": wire::ROUTES.iter().map(|r| format!("POST {}", wire::route(r))).collect::<Vec<_>>(),
                }),
            ),
            (Method::Post, ["v1", func]) => match read_body(&request) {
                Ok(bytes) => dispatch(func, &bytes),
                Err(()) => (
                    413,
                    to_json(&wire::ErrorBody::new("bad-request", format!("the body is over {MAX_BODY_BYTES} bytes, or its read failed"))),
                ),
            },
            _ => (404, to_json(&wire::ErrorBody::new("not-found", format!("no route {route_path}")))),
        };
        emit(response_out, status, &body);
    }
}

fn emit(response_out: ResponseOutparam, status: u16, body: &Value) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    let bytes = body.to_string().into_bytes();
    {
        let stream = out.write().expect("write stream");
        let _ = write_all(&stream, &bytes);
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);

guestio::guest_write_all!();
guestio::guest_read_body!(MAX_BODY_BYTES);

#[cfg(test)]
mod tests {
    use super::*;

    /// A malformed body never reaches the import: it is refused here, in the
    /// daemon's own words.
    #[test]
    fn a_body_that_is_not_the_routes_json_is_a_bad_request() {
        let (s, v) = dispatch("park", b"{\"session\": 1}");
        assert_eq!(s, 400);
        assert_eq!(v["error"], "bad-request");
        let (s, v) = dispatch("wake", b"not json");
        assert_eq!((s, v["error"].as_str()), (400, Some("bad-request")));
        let (s, v) = dispatch("frobnicate", b"{}");
        assert_eq!((s, v["error"].as_str()), (404, Some("not-found")));
    }

    /// A refusal from the import is written with the case, payload and status
    /// the daemon uses — an agent reads one format whichever it talks to.
    #[test]
    fn import_refusals_are_written_as_the_daemon_writes_them() {
        let (s, v) = refusal(t::ParkError::NotFound("t1".into()));
        assert_eq!((s, v["error"].as_str(), v["detail"].as_str()), (404, Some("not-found"), Some("t1")));
        let (s, v) = refusal(t::ParkError::AlreadyClosed("t1".into()));
        assert_eq!((s, v["error"].as_str()), (409, Some("already-closed")));
        let (s, v) = refusal(t::ParkError::StorageError("comp-park at …: http".into()));
        assert_eq!((s, v["error"].as_str()), (503, Some("storage-error")));
    }
}
