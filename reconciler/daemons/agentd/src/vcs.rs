//! `comp-vcs` as `comp-agentd` uses it (ADR-0121): a session's `write_file` edits recorded in the
//! code store (ADR-0099), one workspace per session, so they can be listed, blamed and rewound.
//!
//! Loopback HTTP with `holon-vcs`'s own wire types, as `park.rs` does with `holon-park`'s.

use std::collections::{BTreeMap, BTreeSet};

use holon_vcs::model::{Agent, OpId, Snapshot};
use holon_vcs::wire::{self, Bytes, ErrorBody, IngestFile, IngestReport, Outcome, SourceFile};
use holon_vcs::VcsError;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Every session's files live under one component of its workspace.
pub const COMPONENT: &str = "workspace";

pub struct Vcs {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl Vcs {
    pub fn new(http: reqwest::Client, base: String, token: Option<String>) -> Vcs {
        Vcs { http, base, token }
    }

    async fn call<B: Serialize, T: DeserializeOwned>(
        &self,
        func: &str,
        body: &B,
    ) -> Result<T, VcsError> {
        let mut req =
            self.http.post(format!("{}{}", self.base.trim_end_matches('/'), wire::route(func)));
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let r = req.json(body).send().await.map_err(|e| VcsError::Storage(format!("{e:#}")))?;
        let status = r.status();
        let bytes = r.bytes().await.map_err(|e| VcsError::Storage(e.to_string()))?;
        if status.is_success() {
            return serde_json::from_slice(&bytes).map_err(|e| VcsError::Storage(e.to_string()));
        }
        match serde_json::from_slice::<ErrorBody>(&bytes) {
            Ok(body) => Err(body.into_error()),
            Err(_) => Err(VcsError::Storage(format!("comp-vcs answered {status}"))),
        }
    }

    async fn ingest(
        &self,
        ws: &str,
        path: &str,
        content: &[u8],
        by: Agent,
    ) -> Result<Vec<OpId>, VcsError> {
        let req = IngestFile {
            workspace: ws.to_string(),
            component: COMPONENT.to_string(),
            file: SourceFile { path: path.to_string(), content: Bytes(content.to_vec()) },
            by,
            read_at: None,
        };
        let rep: IngestReport = self.call("ingest-file", &req).await?;
        rep.patches
            .into_iter()
            .map(|p| match p.outcome {
                Outcome::Ok(c) => Ok(c.op),
                Outcome::Err(e) => Err(e.into_error()),
            })
            .collect()
    }

    /// Records `after` as `path`'s content, and returns the ops that did. `before` is what was on
    /// disk; it is recorded first, as the workspace's own, so a change made outside `write_file`
    /// (by `run`, or a person) is not taken for the agent's, and a rewind brings it back.
    pub async fn record(
        &self,
        ws: &str,
        path: &str,
        before: Option<&[u8]>,
        after: &[u8],
        by: Agent,
    ) -> Result<Vec<OpId>, VcsError> {
        if let Some(b) = before {
            self.ingest(ws, path, b, Agent::named("agentd:workspace")).await?;
        }
        self.ingest(ws, path, after, by).await
    }

    pub async fn revert(&self, ws: &str, op: OpId, by: Agent) -> Result<(), VcsError> {
        let req = wire::RevertOp { workspace: ws.to_string(), op, by };
        self.call::<_, serde_json::Value>("revert-op", &req).await.map(|_| ())
    }

    /// What `paths` hold in the store now: `None` for a path it no longer has (a rewound create).
    pub async fn read(
        &self,
        ws: &str,
        paths: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, Option<Vec<u8>>>, VcsError> {
        let req = wire::Export { workspace: ws.to_string(), component: COMPONENT.to_string() };
        let snap: Snapshot = self.call("snapshot-export", &req).await?;
        let mut out = BTreeMap::new();
        for p in paths {
            let content = match snap.entries.iter().find(|e| &e.path == p) {
                Some(e) => {
                    let req = wire::ReadBlob { blob: e.blob.clone() };
                    Some(self.call::<_, Bytes>("read-blob", &req).await?.0)
                }
                None => None,
            };
            out.insert(p.clone(), content);
        }
        Ok(out)
    }
}
