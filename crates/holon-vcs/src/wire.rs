//! The JSON `comp-vcs` speaks over HTTP, and that `vcs-store` and `vcs-gateway`
//! speak to it — one definition for all three (ADR-0099, *The service*).
//!
//! Every route is `POST /v1/<the WIT function's name>` with one JSON object as
//! the body, and answers with the function's `ok` value as the JSON body (`200`)
//! or an [`ErrorBody`] (`4xx`/`5xx`, see [`status_of`]). The records are the
//! [`crate::model`] types — which already serialise field for field like the
//! WIT, kebab-case — plus the request envelopes below for functions that take
//! more than one argument, and the `files` interface's records (ingest and
//! materialize), which have no engine-side serde form.
//!
//! Bytes (file content, blobs) travel as standard base64 strings ([`Bytes`]).

use serde::{Deserialize, Serialize};

use crate::error::VcsError;
use crate::extract;
use crate::model::{
    Agent, CasFailure, CommitResult, ConflictState, Hash, OpId, Snapshot, SymbolId, SymbolQuery,
    WorkspaceId,
};

/// Every route, by the WIT function it serves (`holon:vcs/code-store`, then
/// `holon:vcs/files`).
pub const ROUTES: &[&str] = &[
    "apply-patch",
    "resolve-conflict",
    "revert-op",
    "query-symbol",
    "snapshot-export",
    "list-conflicts",
    "oplog",
    "oplog-head",
    "verify",
    "repair",
    "ingest-file",
    "ingest-tree",
    "materialize",
    "read-blob",
];

/// The path a WIT function is served at.
pub fn route(func: &str) -> String {
    format!("/v1/{func}")
}

// ---- request envelopes ---------------------------------------------------------
//
// `apply-patch` takes a `PatchRequest` and `resolve-conflict` a
// `ResolutionRequest` as the body directly; the rest are named after their WIT
// parameters.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RevertOp {
    pub workspace: WorkspaceId,
    pub op: OpId,
    pub by: Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct QuerySymbol {
    pub workspace: WorkspaceId,
    pub query: SymbolQuery,
}

/// `snapshot-export`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Export {
    pub workspace: WorkspaceId,
    pub component: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ListConflicts {
    pub workspace: WorkspaceId,
    #[serde(default)]
    pub state: Option<ConflictState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Oplog {
    pub workspace: WorkspaceId,
    #[serde(default)]
    pub after: Option<OpId>,
    pub limit: u32,
}

/// `oplog-head`, `verify`, `repair`: a workspace and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Workspace {
    pub workspace: WorkspaceId,
}

// ---- files ----------------------------------------------------------------------

/// Bytes as a base64 string on the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

impl Serialize for Bytes {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use base64::Engine as _;
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(s.as_bytes())
            .map(Bytes)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SourceFile {
    pub path: String,
    pub content: Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct IngestFile {
    pub workspace: WorkspaceId,
    pub component: String,
    pub file: SourceFile,
    pub by: Agent,
    #[serde(default)]
    pub read_at: Option<OpId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct IngestTree {
    pub workspace: WorkspaceId,
    pub component: String,
    pub files: Vec<SourceFile>,
    pub by: Agent,
    #[serde(default)]
    pub read_at: Option<OpId>,
    #[serde(default)]
    pub prune: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EditKind {
    Create,
    Replace,
    Delete,
    Rename,
    Move,
}

impl From<extract::EditKind> for EditKind {
    fn from(k: extract::EditKind) -> Self {
        match k {
            extract::EditKind::Create => EditKind::Create,
            extract::EditKind::Replace => EditKind::Replace,
            extract::EditKind::Delete => EditKind::Delete,
            extract::EditKind::Rename => EditKind::Rename,
            extract::EditKind::Move => EditKind::Move,
        }
    }
}

/// `result<commit-result, vcs-error>` inside an ingest report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Ok(CommitResult),
    Err(ErrorBody),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct IngestPatch {
    pub symbol: SymbolId,
    pub edit: EditKind,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct IngestReport {
    pub path: String,
    pub read_at: OpId,
    pub unchanged: Vec<SymbolId>,
    pub patches: Vec<IngestPatch>,
}

impl From<extract::IngestReport> for IngestReport {
    fn from(r: extract::IngestReport) -> Self {
        IngestReport {
            path: r.path,
            read_at: r.read_at,
            unchanged: r.unchanged,
            patches: r
                .patches
                .into_iter()
                .map(|p| IngestPatch {
                    symbol: p.symbol,
                    edit: p.edit.into(),
                    outcome: match p.result {
                        Ok(c) => Outcome::Ok(c),
                        Err(e) => Outcome::Err(ErrorBody::from(&e)),
                    },
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Materialize {
    pub workspace: WorkspaceId,
    pub component: String,
    pub dest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Materialized {
    pub dir: String,
    pub snapshot: Snapshot,
    pub files: u32,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ReadBlob {
    pub blob: Hash,
}

// ---- errors ---------------------------------------------------------------------

/// A refusal on the wire: `error` is the `vcs-error` case, kebab-case, and
/// `detail` its payload as JSON (a string for `storage-error`, `not-found` and
/// `invalid`; a `cas-failure` object; a list of conflict ids; a `symbol-id`).
/// `message` is for people and never parsed.
///
/// Two codes are not `vcs-error` cases: `not-permitted` (a `materialize`
/// outside the daemon's `--allow-path`, which a component receives as
/// `invalid`) and `bad-request` (a body that is not the route's JSON, also
/// `invalid`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ErrorBody {
    pub error: String,
    #[serde(default)]
    pub detail: serde_json::Value,
    #[serde(default)]
    pub message: String,
}

impl ErrorBody {
    pub fn new(error: &str, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        ErrorBody {
            error: error.to_string(),
            message: format!("{error}: {detail}"),
            detail: serde_json::Value::String(detail),
        }
    }

    /// Back to the contract's error. Anything this side does not recognise is
    /// a `storage-error` naming it: the caller learns the store refused, and
    /// retrying is the only thing it could do about an answer it cannot read.
    pub fn into_error(self) -> VcsError {
        let text = |v: &serde_json::Value| match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        };
        let typed = match self.error.as_str() {
            "storage-error" => Some(VcsError::Storage(text(&self.detail))),
            "not-found" => Some(VcsError::NotFound(text(&self.detail))),
            "invalid" => Some(VcsError::Invalid(text(&self.detail))),
            "not-permitted" => {
                Some(VcsError::Invalid(format!("not-permitted: {}", text(&self.detail))))
            }
            "bad-request" => {
                Some(VcsError::Invalid(format!("bad-request: {}", text(&self.detail))))
            }
            "concurrent-modification" => serde_json::from_value::<CasFailure>(self.detail.clone())
                .ok()
                .map(VcsError::ConcurrentModification),
            "unresolved-conflict" => serde_json::from_value::<Vec<String>>(self.detail.clone())
                .ok()
                .map(VcsError::UnresolvedConflict),
            "symbol-not-found" => serde_json::from_value::<SymbolId>(self.detail.clone())
                .ok()
                .map(VcsError::SymbolNotFound),
            "name-taken" => serde_json::from_value::<SymbolId>(self.detail.clone())
                .ok()
                .map(VcsError::NameTaken),
            _ => None,
        };
        typed.unwrap_or_else(|| {
            VcsError::Storage(format!("comp-vcs answered {}: {}", self.error, self.message))
        })
    }
}

impl From<&VcsError> for ErrorBody {
    fn from(e: &VcsError) -> Self {
        let (error, detail) = match e {
            VcsError::Storage(s) => ("storage-error", serde_json::Value::String(s.clone())),
            VcsError::ConcurrentModification(c) => ("concurrent-modification", json(c)),
            VcsError::UnresolvedConflict(ids) => ("unresolved-conflict", json(ids)),
            VcsError::SymbolNotFound(id) => ("symbol-not-found", json(id)),
            VcsError::NameTaken(id) => ("name-taken", json(id)),
            VcsError::NotFound(s) => ("not-found", serde_json::Value::String(s.clone())),
            VcsError::Invalid(s) => ("invalid", serde_json::Value::String(s.clone())),
        };
        ErrorBody { error: error.to_string(), detail, message: e.to_string() }
    }
}

fn json<T: Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

/// The HTTP status a refusal is sent with. Informational: a client decides by
/// `error`, never by the status alone (a proxy can produce a 502 with no body).
pub fn status_of(error: &str) -> u16 {
    match error {
        "invalid" | "bad-request" => 400,
        "not-permitted" => 403,
        "not-found" | "symbol-not-found" => 404,
        "concurrent-modification" | "unresolved-conflict" | "name-taken" => 409,
        // storage-error and anything unknown: the store, not the request.
        _ => 503,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SymbolKind;

    fn sym() -> SymbolId {
        SymbolId::new("c", "src/lib.rs", "f", SymbolKind::Function)
    }

    #[test]
    fn every_error_round_trips_through_json() {
        let cases = vec![
            VcsError::Storage("down".into()),
            VcsError::ConcurrentModification(CasFailure {
                pointer: "ws/w/sym/k".into(),
                expected: Some("a".repeat(64)),
                actual: None,
            }),
            VcsError::UnresolvedConflict(vec!["x".into(), "y".into()]),
            VcsError::SymbolNotFound(sym()),
            VcsError::NameTaken(sym()),
            VcsError::NotFound("op 9".into()),
            VcsError::Invalid("bad hash".into()),
        ];
        for e in cases {
            let body = ErrorBody::from(&e);
            let text = serde_json::to_string(&body).unwrap();
            let back: ErrorBody = serde_json::from_str(&text).unwrap();
            assert_eq!(back.into_error(), e, "{text}");
        }
    }

    #[test]
    fn errors_have_the_status_of_what_went_wrong() {
        assert_eq!(status_of(&ErrorBody::from(&VcsError::Invalid("x".into())).error), 400);
        assert_eq!(status_of(&ErrorBody::from(&VcsError::NameTaken(sym())).error), 409);
        assert_eq!(status_of(&ErrorBody::from(&VcsError::NotFound("x".into())).error), 404);
        assert_eq!(status_of(&ErrorBody::from(&VcsError::Storage("x".into())).error), 503);
        assert_eq!(status_of("not-permitted"), 403);
    }

    #[test]
    fn unknown_and_malformed_errors_are_storage_errors() {
        let e = ErrorBody {
            error: "teapot".into(),
            detail: serde_json::Value::Null,
            message: "m".into(),
        }
        .into_error();
        assert!(matches!(e, VcsError::Storage(s) if s.contains("teapot")));
        // A name-taken whose detail is not a symbol-id cannot be typed.
        let e = ErrorBody::new("name-taken", "not a symbol").into_error();
        assert!(matches!(e, VcsError::Storage(_)));
        assert!(
            matches!(ErrorBody::new("not-permitted", "/etc").into_error(), VcsError::Invalid(s) if s.contains("/etc"))
        );
    }

    #[test]
    fn bytes_are_base64_and_requests_read_like_the_wit() {
        let f = SourceFile { path: "a.rs".into(), content: Bytes(vec![0, 255, b'x']) };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v, serde_json::json!({"path": "a.rs", "content": "AP94"}));
        let back: SourceFile = serde_json::from_value(v).unwrap();
        assert_eq!(back, f);
        let t: IngestTree = serde_json::from_value(serde_json::json!({
            "workspace": "w", "component": "c", "files": [], "by": {"id": "a"}
        }))
        .unwrap();
        assert!(!t.prune && t.read_at.is_none());
        let o: Oplog = serde_json::from_str(r#"{"workspace":"w","limit":5}"#).unwrap();
        assert_eq!(o.after, None);
        assert_eq!(ROUTES.len(), 14);
        assert_eq!(route("apply-patch"), "/v1/apply-patch");
    }
}
