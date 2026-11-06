//! `vcs-store` — `holon:vcs/code-store` and `holon:vcs/files` with the engine in-process (ADR-0134):
//! the code store is this app's keyvalue, moved by compare-and-set. Each call goes through the same
//! JSON wire the daemon served, so a refusal reaches the caller as the same `vcs-error` case.

#[allow(warnings)]
mod bindings;
mod kvstore;
mod witconv;

use std::path::Path;

use bindings::exports::holon::vcs::code_store::Guest as CodeStore;
use bindings::exports::holon::vcs::files::Guest as Files;

use holon_vcs::error::VcsError;
use holon_vcs::model as m;
use holon_vcs::serve::{self, Answer};
use holon_vcs::{git, wire};
use serde::de::DeserializeOwned;
use serde::Serialize;
use witconv::{ToModel, ToWit};

mod wit {
    pub use crate::bindings::exports::holon::vcs::files;
    pub use crate::bindings::holon::vcs::types as t;
}
use wit::files as f;
use wit::t;

struct Component;

fn storage(msg: impl Into<String>) -> t::VcsError {
    t::VcsError::StorageError(msg.into())
}

fn post(func: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let (status, v) = match kvstore::block_on(serve::dispatch(&kvstore::engine(), func, body)) {
        Some(a) => a,
        None => match serve::parse::<wire::Materialize>(body) {
            Ok(r) => materialize(r),
            Err(a) => a,
        },
    };
    (status, serde_json::to_vec(&v).unwrap_or_default())
}

fn ok<T: Serialize>(v: &T) -> Answer {
    (200, serde_json::to_value(v).unwrap_or_default())
}

/// The host's `--dir` is the allow-list: outside it the guest sees no parent, so it is refused.
/// shortcut: WASI has no chmod, so an executable entry is written 0644; the snapshot (and its
/// git tree) still says executable. Set the bit from the host if a checkout must run its scripts.
fn materialize(r: wire::Materialize) -> Answer {
    let dest = Path::new(&r.dest);
    let parent_ok = dest.is_absolute()
        && dest.file_name().is_some()
        && dest.parent().is_some_and(|p| p.is_dir());
    if !parent_ok {
        let body = wire::ErrorBody::new("not-permitted", r.dest.clone());
        return (403, serde_json::to_value(&body).unwrap_or_default());
    }
    if dest.exists() {
        let empty = std::fs::read_dir(dest).map(|mut d| d.next().is_none()).unwrap_or(false);
        if !dest.is_dir() || !empty {
            let msg = format!("{} exists and is not an empty directory", r.dest);
            return serve::refusal(&VcsError::Invalid(msg));
        }
    }
    let e = kvstore::engine();
    let snap = match kvstore::block_on(e.snapshot_export(&r.workspace, &r.component)) {
        Ok(s) => s,
        Err(e) => return serve::refusal(&e),
    };
    let io =
        |e: std::io::Error| serve::refusal(&VcsError::Storage(format!("writing {}: {e}", r.dest)));
    if let Err(e) = std::fs::create_dir_all(dest) {
        return io(e);
    }
    let (mut files, mut bytes) = (0u32, 0u64);
    for entry in &snap.entries {
        if let Err(e) = git::validate_path(&entry.path) {
            return serve::refusal(&e);
        }
        let content =
            match kvstore::block_on(holon_vcs::store::BlobStore::get(e.blobs(), &entry.blob)) {
                Ok(Some(b)) => b,
                Ok(None) => {
                    return serve::refusal(&VcsError::Storage(format!(
                        "blob {} is missing",
                        entry.blob
                    )))
                }
                Err(e) => return serve::refusal(&e),
            };
        let path = dest.join(&entry.path);
        if let Some(p) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(p) {
                return io(e);
            }
        }
        if let Err(e) = std::fs::write(&path, &content) {
            return io(e);
        }
        files += 1;
        bytes += content.len() as u64;
    }
    ok(&wire::Materialized { dir: r.dest.clone(), snapshot: snap, files, bytes })
}

fn decode<T: DeserializeOwned>(status: u16, body: &[u8]) -> Result<T, t::VcsError> {
    if status == 200 {
        return serde_json::from_slice(body).map_err(|e| {
            storage(format!("the engine answered 200 with a body this cannot read: {e}"))
        });
    }
    match serde_json::from_slice::<wire::ErrorBody>(body) {
        Ok(e) => Err(e.into_error().to_wit()),
        Err(_) => {
            let text = String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned();
            Err(storage(format!("the engine answered {status}: {text}")))
        }
    }
}

fn call<Q: Serialize, R: DeserializeOwned + ToWit>(
    func: &str,
    req: &Q,
) -> Result<R::Out, t::VcsError> {
    let body = serde_json::to_vec(req).map_err(|e| t::VcsError::Invalid(e.to_string()))?;
    let (status, raw) = post(func, &body);
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
        let (status, raw) = post("read-blob", &body);
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

    #[test]
    fn engine_refusals_map_back_to_their_vcs_error_case() {
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
    fn what_is_not_the_engine_s_json_is_a_storage_error() {
        assert!(
            matches!(decode::<u64>(502, b"<html>bad gateway</html>"), Err(t::VcsError::StorageError(s)) if s.contains("502"))
        );
        assert!(matches!(decode::<u64>(200, b"not json"), Err(t::VcsError::StorageError(_))));
        let not_permitted =
            serde_json::to_vec(&wire::ErrorBody::new("not-permitted", "/etc")).unwrap();
        assert!(
            matches!(decode::<wire::Materialized>(403, &not_permitted), Err(t::VcsError::Invalid(s)) if s.contains("not-permitted"))
        );
        assert_eq!(decode::<u64>(200, b"7").ok(), Some(7));
    }

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
    fn materialize_outside_the_host_s_dirs_is_not_permitted() {
        for dest in ["/no/such/parent/x", "relative/x", "/"] {
            let r = wire::Materialize {
                workspace: "w".into(),
                component: "c".into(),
                dest: dest.into(),
            };
            assert_eq!(materialize(r).0, 403, "{dest}");
        }
    }
}
