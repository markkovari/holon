//! `vcs-store` — `holon:vcs/code-store` and `holon:vcs/files` for components,
//! each call one HTTP request to `comp-vcs` (ADR-0099, ADR-0095).
//!
//! The engine needs NATS JetStream and SurrealDB, which a `wasm32-wasip2` guest
//! cannot dial, so it runs natively (`reconciler/src/bin/vcs.rs`) and this is
//! its component face: it holds the contract, turns each call into
//! `POST <vcs-url>/v1/<function>` with the arguments as JSON
//! (`holon_vcs::wire`), and turns the answer back into the WIT result. It keeps
//! no state and makes no decision.
//!
//! Config (wasi:config/store):
//!   vcs-url     where `comp-vcs` listens, e.g. http://127.0.0.1:8014
//!   vcs-token   the daemon's `--token`, sent as `Authorization: Bearer`, if set
//!
//! Errors: the daemon's refusals arrive as `{error, detail}` and come back as
//! the `vcs-error` case they name, payload and all. Anything else — no
//! `vcs-url`, nothing listening, a 401, a body that is not the daemon's JSON —
//! is `storage-error`, which is what the contract says a caller may retry.

#[allow(warnings)]
mod bindings;
mod witconv;

use bindings::exports::holon::vcs::code_store::Guest as CodeStore;
use bindings::exports::holon::vcs::files::Guest as Files;
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
use bindings::wasi::io::streams::StreamError;

use holon_vcs::model as m;
use holon_vcs::wire;
use serde::de::DeserializeOwned;
use serde::Serialize;
use witconv::{ToModel, ToWit};

/// The binding modules `witconv` is compiled against.
mod wit {
    pub use crate::bindings::exports::holon::vcs::files;
    pub use crate::bindings::holon::vcs::types as t;
}
use wit::files as f;
use wit::t;

struct Component;

/// Five minutes. An `ingest-tree` of a whole component, or an export waiting
/// out a crashed writer's lease, is slow but should not hang a caller forever.
const TIMEOUT_NS: u64 = 300_000_000_000;

fn storage(msg: impl Into<String>) -> t::VcsError {
    t::VcsError::StorageError(msg.into())
}

fn daemon_url() -> Result<String, t::VcsError> {
    match config::get("vcs-url") {
        Ok(Some(u)) if !u.is_empty() => Ok(u),
        _ => Err(storage("vcs-url is not set — this component has no comp-vcs to ask")),
    }
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), t::VcsError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(storage(format!("vcs-url must be http(s), got {url:?}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].trim_end_matches('/').to_string()),
        None => (rest.to_string(), String::new()),
    };
    Ok((scheme, authority, path))
}

/// POST `body` to `/v1/<func>`; the status and the whole response body.
fn post(func: &str, body: &[u8]) -> Result<(u16, Vec<u8>), t::VcsError> {
    let url = daemon_url()?;
    let (scheme, authority, base) = parse_url(&url)?;
    let net = |m: &str| storage(format!("comp-vcs at {url}: {m}"));

    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    if let Ok(Some(token)) = config::get("vcs-token") {
        if !token.is_empty() {
            let _ = headers.set("authorization", &[format!("Bearer {token}").into_bytes()]);
        }
    }
    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&format!("{base}{}", wire::route(func))))
        .map_err(|_| net("set path"))?;

    let out = req.body().map_err(|_| net("body"))?;
    {
        let stream = out.write().map_err(|_| net("write"))?;
        // Chunked: `blocking-write-and-flush` traps above 4096 bytes.
        for chunk in body.chunks(4096) {
            stream
                .blocking_write_and_flush(chunk)
                .map_err(|e| net(&format!("body write: {e:?}")))?;
        }
    }
    OutgoingBody::finish(out, None).map_err(|_| net("finish"))?;

    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(TIMEOUT_NS));
    let _ = opts.set_first_byte_timeout(Some(TIMEOUT_NS));
    let _ = opts.set_between_bytes_timeout(Some(TIMEOUT_NS));

    let fut =
        outgoing_handler::handle(req, Some(opts)).map_err(|e| net(&format!("handle: {e:?}")))?;
    fut.subscribe().block();
    let resp = fut
        .get()
        .ok_or_else(|| net("no response"))?
        .map_err(|_| net("response taken"))?
        .map_err(|e| net(&format!("http: {e:?}")))?;
    let status = resp.status();

    let body = resp.consume().map_err(|_| net("consume"))?;
    let stream = body.stream().map_err(|_| net("stream"))?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(c) if c.is_empty() => break,
            Ok(c) => buf.extend_from_slice(&c),
            // `Closed` is end-of-body; anything else is a read that went wrong,
            // and returning what arrived would be a truncated answer.
            Err(StreamError::Closed) => break,
            Err(e) => return Err(net(&format!("read: {e:?}"))),
        }
    }
    Ok((status, buf))
}

/// The daemon's answer, as the WIT result: `200` is the `ok` value; anything
/// else is its `{error, detail}` or, failing that, a storage error.
fn decode<T: DeserializeOwned>(status: u16, body: &[u8]) -> Result<T, t::VcsError> {
    if status == 200 {
        return serde_json::from_slice(body).map_err(|e| {
            storage(format!("comp-vcs answered 200 with a body this cannot read: {e}"))
        });
    }
    if status == 401 {
        return Err(storage(
            "comp-vcs refused the credentials (401): vcs-token must equal its --token",
        ));
    }
    match serde_json::from_slice::<wire::ErrorBody>(body) {
        Ok(e) => Err(e.into_error().to_wit()),
        Err(_) => {
            let text = String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned();
            Err(storage(format!("comp-vcs answered HTTP {status}: {text}")))
        }
    }
}

/// One call: the request as JSON, the answer as `R` (the model type), then WIT.
fn call<Q: Serialize, R: DeserializeOwned + ToWit>(
    func: &str,
    req: &Q,
) -> Result<R::Out, t::VcsError> {
    let body = serde_json::to_vec(req).map_err(|e| t::VcsError::Invalid(e.to_string()))?;
    let (status, raw) = post(func, &body)?;
    decode::<R>(status, &raw).map(ToWit::to_wit)
}

impl CodeStore for Component {
    fn apply_patch(req: t::PatchRequest) -> Result<t::CommitResult, t::VcsError> {
        call::<_, m::CommitResult>("apply-patch", &req.to_model())
    }

    fn resolve_conflict(req: t::ResolutionRequest) -> Result<t::CommitResult, t::VcsError> {
        call::<_, m::CommitResult>("resolve-conflict", &req.to_model())
    }

    fn revert_op(workspace: String, op: u64, by: t::Agent) -> Result<t::OpEntry, t::VcsError> {
        call::<_, m::OpEntry>("revert-op", &wire::RevertOp { workspace, op, by: by.to_model() })
    }

    fn query_symbol(
        workspace: String,
        query: t::SymbolQuery,
    ) -> Result<Vec<t::SymbolView>, t::VcsError> {
        call::<_, Vec<m::SymbolView>>(
            "query-symbol",
            &wire::QuerySymbol { workspace, query: query.to_model() },
        )
    }

    fn snapshot_export(workspace: String, component: String) -> Result<t::Snapshot, t::VcsError> {
        call::<_, m::Snapshot>("snapshot-export", &wire::Export { workspace, component })
    }

    fn list_conflicts(
        workspace: String,
        state: Option<t::ConflictState>,
    ) -> Result<Vec<t::Conflict>, t::VcsError> {
        call::<_, Vec<m::Conflict>>(
            "list-conflicts",
            &wire::ListConflicts { workspace, state: state.to_model() },
        )
    }

    fn oplog(
        workspace: String,
        after: Option<u64>,
        limit: u32,
    ) -> Result<Vec<t::OpEntry>, t::VcsError> {
        call::<_, Vec<m::OpEntry>>("oplog", &wire::Oplog { workspace, after, limit })
    }

    fn oplog_head(workspace: String) -> Result<u64, t::VcsError> {
        call::<_, u64>("oplog-head", &wire::Workspace { workspace })
    }

    fn verify(workspace: String) -> Result<t::ConsistencyReport, t::VcsError> {
        call::<_, m::ConsistencyReport>("verify", &wire::Workspace { workspace })
    }

    fn repair(workspace: String) -> Result<t::RepairReport, t::VcsError> {
        call::<_, m::RepairReport>("repair", &wire::Workspace { workspace })
    }
}

impl Files for Component {
    fn ingest_file(
        workspace: String,
        component: String,
        file: f::SourceFile,
        by: t::Agent,
        read_at: Option<u64>,
    ) -> Result<f::IngestReport, t::VcsError> {
        let req = wire::IngestFile {
            workspace,
            component,
            file: file.to_model(),
            by: by.to_model(),
            read_at,
        };
        call::<_, wire::IngestReport>("ingest-file", &req)
    }

    fn ingest_tree(
        workspace: String,
        component: String,
        files: Vec<f::SourceFile>,
        by: t::Agent,
        read_at: Option<u64>,
        prune: bool,
    ) -> Result<Vec<f::IngestReport>, t::VcsError> {
        let req = wire::IngestTree {
            workspace,
            component,
            files: files.to_model(),
            by: by.to_model(),
            read_at,
            prune,
        };
        call::<_, Vec<wire::IngestReport>>("ingest-tree", &req)
    }

    fn materialize(
        workspace: String,
        component: String,
        dest: String,
    ) -> Result<f::Materialized, t::VcsError> {
        call::<_, wire::Materialized>(
            "materialize",
            &wire::Materialize { workspace, component, dest },
        )
    }

    fn read_blob(blob: String) -> Result<Vec<u8>, t::VcsError> {
        let body = serde_json::to_vec(&wire::ReadBlob { blob })
            .map_err(|e| t::VcsError::Invalid(e.to_string()))?;
        let (status, raw) = post("read-blob", &body)?;
        decode::<wire::Bytes>(status, &raw).map(|b| b.0)
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    fn body(e: &holon_vcs::VcsError) -> Vec<u8> {
        serde_json::to_vec(&wire::ErrorBody::from(e)).unwrap()
    }

    fn sym() -> m::SymbolId {
        m::SymbolId::new("c", "src/lib.rs", "f", m::SymbolKind::Function)
    }

    /// Every refusal the daemon can send comes back as the case it names, with
    /// its payload — a caller must be able to tell "retry" from "resolve" from
    /// "rename to something else".
    #[test]
    fn daemon_refusals_map_back_to_their_vcs_error_case() {
        let cas = m::CasFailure {
            pointer: "ws/w/sym/k".into(),
            expected: None,
            actual: Some("b".repeat(64)),
        };
        let got = decode::<m::CommitResult>(
            409,
            &body(&holon_vcs::VcsError::ConcurrentModification(cas.clone())),
        );
        assert!(
            matches!(got, Err(t::VcsError::ConcurrentModification(c)) if c.pointer == cas.pointer && c.actual == cas.actual)
        );
        let got = decode::<m::CommitResult>(409, &body(&holon_vcs::VcsError::NameTaken(sym())));
        assert!(
            matches!(got, Err(t::VcsError::NameTaken(id)) if id.name == "f" && matches!(id.kind, t::SymbolKind::Function))
        );
        let got = decode::<m::Snapshot>(
            409,
            &body(&holon_vcs::VcsError::UnresolvedConflict(vec!["x".into()])),
        );
        assert!(
            matches!(got, Err(t::VcsError::UnresolvedConflict(ids)) if ids == vec!["x".to_string()])
        );
        let got = decode::<m::Snapshot>(404, &body(&holon_vcs::VcsError::SymbolNotFound(sym())));
        assert!(matches!(got, Err(t::VcsError::SymbolNotFound(_))));
        let got = decode::<m::Snapshot>(404, &body(&holon_vcs::VcsError::NotFound("op 3".into())));
        assert!(matches!(got, Err(t::VcsError::NotFound(s)) if s == "op 3"));
        let got = decode::<m::Snapshot>(400, &body(&holon_vcs::VcsError::Invalid("bad".into())));
        assert!(matches!(got, Err(t::VcsError::Invalid(s)) if s == "bad"));
        let got = decode::<m::Snapshot>(503, &body(&holon_vcs::VcsError::Storage("down".into())));
        assert!(matches!(got, Err(t::VcsError::StorageError(s)) if s == "down"));
    }

    #[test]
    fn what_is_not_the_daemons_json_is_a_storage_error() {
        assert!(
            matches!(decode::<u64>(502, b"<html>bad gateway</html>"), Err(t::VcsError::StorageError(s)) if s.contains("502"))
        );
        assert!(
            matches!(decode::<u64>(401, b""), Err(t::VcsError::StorageError(s)) if s.contains("vcs-token"))
        );
        assert!(matches!(decode::<u64>(200, b"not json"), Err(t::VcsError::StorageError(_))));
        let not_permitted =
            serde_json::to_vec(&wire::ErrorBody::new("not-permitted", "/etc")).unwrap();
        assert!(
            matches!(decode::<wire::Materialized>(403, &not_permitted), Err(t::VcsError::Invalid(s)) if s.contains("not-permitted"))
        );
        assert_eq!(decode::<u64>(200, b"7").ok(), Some(7));
    }

    /// The conversion is lossless both ways for the records with the most
    /// shape: a request with a placement, and an ingest report with a refusal.
    #[test]
    fn records_survive_model_to_wit_and_back() {
        let req = m::PatchRequest {
            workspace: "w".into(),
            symbol: sym(),
            parent: Some("a".repeat(64)),
            change: m::Transformation::Move(m::Placement::After(sym())),
            agent: m::Agent { id: "a".into(), goal: Some("g".into()), model: None },
            message: Some("msg".into()),
            depends_on: vec![sym()],
            implements: vec![],
            wit_binding: Some("x:y/z".into()),
            read_at: Some(4),
            position: Some(m::Placement::First),
        };
        assert_eq!(req.clone().to_wit().to_model(), req);
        let rep = wire::IngestReport {
            path: "src/lib.rs".into(),
            read_at: 3,
            unchanged: vec![sym()],
            patches: vec![wire::IngestPatch {
                symbol: sym(),
                edit: wire::EditKind::Rename,
                outcome: wire::Outcome::Err(wire::ErrorBody::from(
                    &holon_vcs::VcsError::NameTaken(sym()),
                )),
            }],
        };
        let back = rep.clone().to_wit().to_model();
        assert_eq!(back.patches[0].outcome, rep.patches[0].outcome);
        assert_eq!(back, rep);
    }

    #[test]
    fn a_url_with_a_base_path_keeps_it() {
        let (_, auth, base) = parse_url("http://127.0.0.1:8014/").unwrap();
        assert_eq!((auth.as_str(), base.as_str()), ("127.0.0.1:8014", ""));
        let (_, _, base) = parse_url("https://vcs.internal/api").unwrap();
        assert_eq!(base, "/api");
        assert!(parse_url("nats://x").is_err());
    }
}
