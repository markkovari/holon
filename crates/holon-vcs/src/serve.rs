//! `POST /v1/<wit function>` over any engine: JSON in and out as [`crate::wire`] defines, errors as
//! `{error, detail, message}` with a status by kind. `materialize` writes a directory, so it is the
//! caller's; this answers `None` for it and for no other name.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::error::VcsError;
use crate::graph::Graph;
use crate::model::{PatchRequest, ResolutionRequest};
use crate::oplog::OpLog;
use crate::store::{BlobStore, PointerStore};
use crate::{extract, git, wire, Engine};

pub type Answer = (u16, Value);

fn json<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

pub fn refusal(e: &VcsError) -> Answer {
    let body = wire::ErrorBody::from(e);
    (wire::status_of(&body.error), json(&body))
}

fn answer<T: Serialize>(r: Result<T, VcsError>) -> Answer {
    match r {
        Ok(v) => (200, json(&v)),
        Err(e) => refusal(&e),
    }
}

pub fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, Answer> {
    serde_json::from_slice(body).map_err(|e| {
        let body = wire::ErrorBody::new("bad-request", e.to_string());
        (400, json(&body))
    })
}

/// The workspace a call writes to, for a caller that keeps a list of them to repair.
pub fn writes_to(func: &str, body: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Ws {
        workspace: String,
    }
    matches!(
        func,
        "apply-patch" | "resolve-conflict" | "revert-op" | "repair" | "ingest-file" | "ingest-tree"
    )
    .then(|| serde_json::from_slice::<Ws>(body).ok().map(|w| w.workspace))
    .flatten()
}

pub async fn dispatch<B, P, G, L>(e: &Engine<B, P, G, L>, func: &str, body: &[u8]) -> Option<Answer>
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    Some(match inner(e, func, body).await? {
        Ok(a) | Err(a) => a,
    })
}

async fn inner<B, P, G, L>(
    e: &Engine<B, P, G, L>,
    func: &str,
    body: &[u8],
) -> Option<Result<Answer, Answer>>
where
    B: BlobStore,
    P: PointerStore,
    G: Graph,
    L: OpLog,
{
    let run = async {
        Ok(match func {
            "apply-patch" => {
                let req: PatchRequest = parse(body)?;
                answer(e.apply_patch(req).await)
            }
            "resolve-conflict" => {
                let req: ResolutionRequest = parse(body)?;
                answer(e.resolve_conflict(req).await)
            }
            "revert-op" => {
                let r: wire::RevertOp = parse(body)?;
                answer(e.revert_op(&r.workspace, r.op, r.by.clone()).await)
            }
            "query-symbol" => {
                let r: wire::QuerySymbol = parse(body)?;
                answer(e.query_symbol(&r.workspace, r.query.clone()).await)
            }
            "snapshot-export" => {
                let r: wire::Export = parse(body)?;
                answer(e.snapshot_export(&r.workspace, &r.component).await)
            }
            "list-conflicts" => {
                let r: wire::ListConflicts = parse(body)?;
                answer(e.list_conflicts(&r.workspace, r.state).await)
            }
            "oplog" => {
                let r: wire::Oplog = parse(body)?;
                answer(e.oplog(&r.workspace, r.after, r.limit).await)
            }
            "oplog-head" => {
                let r: wire::Workspace = parse(body)?;
                answer(e.oplog_head(&r.workspace).await)
            }
            "verify" => {
                let r: wire::Workspace = parse(body)?;
                answer(e.verify(&r.workspace).await)
            }
            "repair" => {
                let r: wire::Workspace = parse(body)?;
                answer(e.repair(&r.workspace).await)
            }
            "ingest-file" => {
                let r: wire::IngestFile = parse(body)?;
                git::validate_path(&r.file.path).map_err(|e| refusal(&e))?;
                let rep = extract::ingest_file(
                    e,
                    &r.workspace,
                    &r.component,
                    &r.file.path,
                    &r.file.content.0,
                    &r.by,
                    r.read_at,
                )
                .await;
                answer(rep.map(wire::IngestReport::from))
            }
            "ingest-tree" => {
                let r: wire::IngestTree = parse(body)?;
                for f in &r.files {
                    git::validate_path(&f.path).map_err(|e| refusal(&e))?;
                }
                let files: Vec<(String, Vec<u8>)> =
                    r.files.into_iter().map(|f| (f.path, f.content.0)).collect();
                let rep = extract::ingest_tree(
                    e,
                    &r.workspace,
                    &r.component,
                    &files,
                    &r.by,
                    r.read_at,
                    r.prune,
                )
                .await;
                answer(rep.map(|v| v.into_iter().map(wire::IngestReport::from).collect::<Vec<_>>()))
            }
            "read-blob" => {
                let r: wire::ReadBlob = parse(body)?;
                if !crate::store::is_hash(&r.blob) {
                    let msg = format!("{:?} is not a sha-256 hash", r.blob);
                    return Err(refusal(&VcsError::Invalid(msg)));
                }
                match e.blobs().get(&r.blob).await {
                    Ok(Some(b)) => (200, json(&wire::Bytes(b))),
                    Ok(None) => refusal(&VcsError::NotFound(format!("blob {}", r.blob))),
                    Err(e) => refusal(&e),
                }
            }
            other => {
                let body = wire::ErrorBody::new("not-found", format!("no route /v1/{other}"));
                (404, json(&body))
            }
        })
    };
    if func == "materialize" {
        return None;
    }
    Some(run.await)
}
