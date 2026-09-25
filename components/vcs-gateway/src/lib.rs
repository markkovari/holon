//! `vcs-gateway` — the code store as HTTP routes, for agents and tests
//! (ADR-0099).
//!
//! It imports `holon:vcs/code-store` and `holon:vcs/files` and serves each
//! function at `POST /v1/<function>` with the same JSON `comp-vcs` takes
//! (`holon_vcs::wire`): parse the body into the contract's records, make ONE
//! import call, write the result back. Composed with `vcs-store`, the chain is
//!
//!   agent → comp-host (vcs-gateway ⊕ vcs-store) → comp-vcs → NATS + SurrealDB
//!
//! and every request here is a real round trip through the component model —
//! which is the point: the e2e suite (`reconciler/tests/e2e_vcs.rs`) drives the
//! store through this, not through the library.
//!
//! Errors are `{error, detail, message}` with the status `holon_vcs::wire`
//! gives each `vcs-error` case, exactly as the daemon sends them.
//!
//! No auth of its own: whoever can reach this may edit every workspace. It is a
//! tailnet app (apps/vcs.toml) and meant for agents on that network; a
//! deployment that needs more puts `auth-guard` in front.

#[allow(warnings)]
mod bindings;
#[path = "../../vcs-store/src/witconv.rs"]
mod witconv;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::holon::vcs::code_store as store;
use bindings::holon::vcs::files as fl;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

use holon_vcs::model as m;
use holon_vcs::wire;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use witconv::{ToModel, ToWit};

/// The binding modules `witconv` is compiled against.
mod wit {
    pub use crate::bindings::holon::vcs::files;
    pub use crate::bindings::holon::vcs::types as t;
}
use wit::t;

/// An `ingest-tree` of a component's sources, base64'd.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

struct Component;

/// A route's answer.
type Answer = (u16, Value);

fn to_json<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn refusal(e: t::VcsError) -> Answer {
    let body = wire::ErrorBody::from(&e.to_model());
    (wire::status_of(&body.error), to_json(&body))
}

/// The import's result, as the daemon would have written it.
fn answer<W: ToModel>(r: Result<W, t::VcsError>) -> Answer
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
        "apply-patch" => {
            let r: m::PatchRequest = parse(body)?;
            answer(store::apply_patch(&r.to_wit()))
        }
        "resolve-conflict" => {
            let r: m::ResolutionRequest = parse(body)?;
            answer(store::resolve_conflict(&r.to_wit()))
        }
        "revert-op" => {
            let r: wire::RevertOp = parse(body)?;
            answer(store::revert_op(&r.workspace, r.op, &r.by.to_wit()))
        }
        "query-symbol" => {
            let r: wire::QuerySymbol = parse(body)?;
            answer(store::query_symbol(&r.workspace, &r.query.to_wit()))
        }
        "snapshot-export" => {
            let r: wire::Export = parse(body)?;
            answer(store::snapshot_export(&r.workspace, &r.component))
        }
        "list-conflicts" => {
            let r: wire::ListConflicts = parse(body)?;
            answer(store::list_conflicts(&r.workspace, r.state.to_wit()))
        }
        "oplog" => {
            let r: wire::Oplog = parse(body)?;
            answer(store::oplog(&r.workspace, r.after, r.limit))
        }
        "oplog-head" => {
            let r: wire::Workspace = parse(body)?;
            answer(store::oplog_head(&r.workspace))
        }
        "verify" => {
            let r: wire::Workspace = parse(body)?;
            answer(store::verify(&r.workspace))
        }
        "repair" => {
            let r: wire::Workspace = parse(body)?;
            answer(store::repair(&r.workspace))
        }
        "ingest-file" => {
            let r: wire::IngestFile = parse(body)?;
            answer(fl::ingest_file(&r.workspace, &r.component, &r.file.to_wit(), &r.by.to_wit(), r.read_at))
        }
        "ingest-tree" => {
            let r: wire::IngestTree = parse(body)?;
            let files = r.files.to_wit();
            answer(fl::ingest_tree(&r.workspace, &r.component, &files, &r.by.to_wit(), r.read_at, r.prune))
        }
        "materialize" => {
            let r: wire::Materialize = parse(body)?;
            answer(fl::materialize(&r.workspace, &r.component, &r.dest))
        }
        "read-blob" => {
            let r: wire::ReadBlob = parse(body)?;
            match fl::read_blob(&r.blob) {
                Ok(b) => (200, to_json(&wire::Bytes(b))),
                Err(e) => refusal(e),
            }
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
                    "service": "holon:vcs gateway (ADR-0099)",
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
        let (s, v) = dispatch("apply-patch", b"{\"workspace\": 1}");
        assert_eq!(s, 400);
        assert_eq!(v["error"], "bad-request");
        let (s, v) = dispatch("materialize", b"not json");
        assert_eq!((s, v["error"].as_str()), (400, Some("bad-request")));
        let (s, v) = dispatch("frobnicate", b"{}");
        assert_eq!((s, v["error"].as_str()), (404, Some("not-found")));
    }

    /// A refusal from the import is written with the case, payload and status
    /// the daemon uses — an agent reads one format whichever it talks to.
    #[test]
    fn import_refusals_are_written_as_the_daemon_writes_them() {
        let id = t::SymbolId { component: "c".into(), path: "a.rs".into(), name: "f".into(), kind: t::SymbolKind::Function };
        let (s, v) = refusal(t::VcsError::NameTaken(id));
        assert_eq!(s, 409);
        assert_eq!(v["error"], "name-taken");
        assert_eq!(v["detail"]["name"], "f");
        assert_eq!(v["detail"]["kind"], "function");
        let (s, v) = refusal(t::VcsError::UnresolvedConflict(vec!["x".into()]));
        assert_eq!((s, v["detail"].clone()), (409, json!(["x"])));
        let (s, v) = refusal(t::VcsError::StorageError("comp-vcs at …: http".into()));
        assert_eq!((s, v["error"].as_str()), (503, Some("storage-error")));
    }
}
