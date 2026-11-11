//! Compare-and-Swap manifest and Git-lineage commit engine

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::RwLock;

/// Manifest stored at `refs/{repo}/{branch}`
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RefManifest {
    pub repo: String,
    pub branch: String,
    pub head_commit_oid: String,
    pub generation: u64,
    pub updated_at_unix_ms: u64,
    pub writer_agent_id: String,
}

/// Immutable commit object stored at `commits/{commit_oid}`
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AgentCommit {
    pub oid: String,
    pub parent_oids: Vec<String>,
    pub tree_oid: String,
    pub capability_log_oid: Option<String>,
    pub agent_id: String,
    pub generation: u64,
    pub message: String,
    pub eval_score: Option<String>,
    pub timestamp_ms: u64,
}

impl AgentCommit {
    pub fn compute_oid(
        parent_oids: &[String],
        tree_oid: &str,
        capability_log_oid: Option<&str>,
        agent_id: &str,
        generation: u64,
        message: &str,
        timestamp_ms: u64,
    ) -> String {
        let mut hasher = Sha256::new();
        for p in parent_oids {
            hasher.update(p.as_bytes());
            hasher.update(b":");
        }
        hasher.update(tree_oid.as_bytes());
        hasher.update(b":");
        if let Some(log) = capability_log_oid {
            hasher.update(log.as_bytes());
        }
        hasher.update(b":");
        hasher.update(agent_id.as_bytes());
        hasher.update(b":");
        hasher.update(generation.to_string().as_bytes());
        hasher.update(b":");
        hasher.update(message.as_bytes());
        hasher.update(b":");
        hasher.update(timestamp_ms.to_string().as_bytes());
        format!("sha256:{}", hex::encode(hasher.finalize()))
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CheckpointRequest {
    pub branch: String,
    pub agent_id: String,
    pub state_root: String,
    pub capability_log_head: String,
    pub message: String,
    pub expected_etag: Option<String>,
    pub eval_score: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CasSuccess {
    pub manifest: RefManifest,
    pub etag: String,
    pub commit: AgentCommit,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CasConflict {
    pub current_head: Option<String>,
    pub current_etag: Option<String>,
    pub message: String,
}

pub fn hash_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

pub fn etag_of(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("\"{}\"", hex::encode(hasher.finalize()))
}

/// In-memory CAS storage backing the edge component
#[derive(Default)]
pub struct CasStore {
    objects: RwLock<HashMap<String, Vec<u8>>>,
    commits: RwLock<HashMap<String, AgentCommit>>,
    manifests: RwLock<HashMap<String, (RefManifest, String)>>,
}

impl CasStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put_object(&self, data: &[u8]) -> String {
        let oid = hash_bytes(data);
        let mut objs = self.objects.write().unwrap();
        objs.insert(oid.clone(), data.to_vec());
        oid
    }

    pub fn get_object(&self, oid: &str) -> Option<Vec<u8>> {
        let objs = self.objects.read().unwrap();
        objs.get(oid).cloned()
    }

    pub fn put_commit(&self, commit: AgentCommit) {
        let mut commits = self.commits.write().unwrap();
        commits.insert(commit.oid.clone(), commit);
    }

    pub fn get_commit(&self, oid: &str) -> Option<AgentCommit> {
        let commits = self.commits.read().unwrap();
        commits.get(oid).cloned()
    }

    pub fn get_manifest(&self, repo: &str, branch: &str) -> Option<(RefManifest, String)> {
        let key = format!("refs/{repo}/{branch}");
        let manifests = self.manifests.read().unwrap();
        manifests.get(&key).cloned()
    }

    pub fn put_manifest_cas(
        &self,
        repo: &str,
        branch: &str,
        new_head_oid: &str,
        agent_id: &str,
        expected_etag: Option<&str>,
        timestamp_ms: u64,
    ) -> Result<(RefManifest, String), CasConflict> {
        let key = format!("refs/{repo}/{branch}");
        let mut manifests = self.manifests.write().unwrap();

        let current = manifests.get(&key);

        match (current, expected_etag) {
            // Expected a specific ETag, but ref does not exist
            (None, Some(_)) => {
                return Err(CasConflict {
                    current_head: None,
                    current_etag: None,
                    message: format!("Ref {key} does not exist"),
                });
            }
            // Expected creation (If-None-Match: *), but ref already exists
            (Some((existing, etag)), None) => {
                return Err(CasConflict {
                    current_head: Some(existing.head_commit_oid.clone()),
                    current_etag: Some(etag.clone()),
                    message: format!("Ref {key} already exists with etag {etag}"),
                });
            }
            // Mismatched ETag
            (Some((existing, etag)), Some(expected)) => {
                let norm_expected = expected.trim_matches('"');
                let norm_etag = etag.trim_matches('"');
                if norm_expected != "*" && norm_expected != norm_etag {
                    return Err(CasConflict {
                        current_head: Some(existing.head_commit_oid.clone()),
                        current_etag: Some(etag.clone()),
                        message: format!("ETag mismatch: expected {expected}, current is {etag}"),
                    });
                }
            }
            // Creating new ref
            (None, None) => {}
        }

        let generation = current.map_or(1, |(m, _)| m.generation + 1);
        let new_manifest = RefManifest {
            repo: repo.to_string(),
            branch: branch.to_string(),
            head_commit_oid: new_head_oid.to_string(),
            generation,
            updated_at_unix_ms: timestamp_ms,
            writer_agent_id: agent_id.to_string(),
        };

        let raw = serde_json::to_vec(&new_manifest).unwrap();
        let new_etag = etag_of(&raw);
        manifests.insert(key, (new_manifest.clone(), new_etag.clone()));

        Ok((new_manifest, new_etag))
    }

    pub fn commit_checkpoint(
        &self,
        repo: &str,
        req: &CheckpointRequest,
        timestamp_ms: u64,
    ) -> Result<CasSuccess, CasConflict> {
        let current = self.get_manifest(repo, &req.branch);
        let (parent_oids, generation) = match &current {
            Some((m, _)) => (vec![m.head_commit_oid.clone()], m.generation + 1),
            None => (Vec::new(), 1),
        };

        let commit_oid = AgentCommit::compute_oid(
            &parent_oids,
            &req.state_root,
            Some(&req.capability_log_head),
            &req.agent_id,
            generation,
            &req.message,
            timestamp_ms,
        );

        let commit = AgentCommit {
            oid: commit_oid.clone(),
            parent_oids,
            tree_oid: req.state_root.clone(),
            capability_log_oid: Some(req.capability_log_head.clone()),
            agent_id: req.agent_id.clone(),
            generation,
            message: req.message.clone(),
            eval_score: req.eval_score.clone(),
            timestamp_ms,
        };

        let (manifest, etag) = self.put_manifest_cas(
            repo,
            &req.branch,
            &commit_oid,
            &req.agent_id,
            req.expected_etag.as_deref(),
            timestamp_ms,
        )?;

        self.put_commit(commit.clone());

        Ok(CasSuccess { manifest, etag, commit })
    }
}
