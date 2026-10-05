//! Everything an agent leaves on disk, under one state directory:
//!
//! ```text
//! agents/<name>.json      the spec (edit it and the next run uses it)
//! memory/<name>.jsonl     things the agent chose to remember
//! runs/<name>.jsonl       one finished run per line, with every step
//! workspaces/<name>/      the only place its file tools can touch
//! ```

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::spec::{validate_name, AgentSpec};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step {
    /// The model's raw reply for one turn.
    Model { text: String },
    /// A tool the agent called. `approved` is `None` when none was needed.
    Tool {
        name: String,
        args: serde_json::Value,
        result: String,
        error: bool,
        approved: Option<bool>,
    },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Failed,
    /// Stopped for spending more than `max_tokens`, or the daily budget.
    OverBudget,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RunRecord {
    pub id: String,
    pub agent: String,
    /// `http`, `schedule: <cron>`, `event: <topic>`, `agent: <caller>`, `ui`.
    pub trigger: String,
    pub input: String,
    pub started: u64,
    pub finished: u64,
    pub status: Status,
    pub answer: String,
    pub steps: Vec<Step>,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MemoryItem {
    pub at: u64,
    pub text: String,
}

#[derive(Clone)]
pub struct Store {
    dir: PathBuf,
}

fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(|w| w.to_lowercase())
        .collect()
}

impl Store {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, String> {
        let dir = dir.into();
        for sub in ["agents", "memory", "runs", "workspaces"] {
            fs::create_dir_all(dir.join(sub)).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok(Self { dir })
    }

    fn path(&self, sub: &str, name: &str, ext: &str) -> Result<PathBuf, String> {
        validate_name(name)?;
        Ok(self.dir.join(sub).join(format!("{name}.{ext}")))
    }

    pub fn list(&self) -> Vec<AgentSpec> {
        let mut v: Vec<AgentSpec> = fs::read_dir(self.dir.join("agents"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| fs::read_to_string(e.path()).ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn get(&self, name: &str) -> Option<AgentSpec> {
        let p = self.path("agents", name, "json").ok()?;
        serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
    }

    /// Atomic (write then rename) so a crash mid-save can't leave half a spec.
    pub fn put(&self, spec: &AgentSpec) -> Result<(), String> {
        let p = self.path("agents", &spec.name, "json")?;
        let tmp = p.with_extension("json.tmp");
        let body = serde_json::to_string_pretty(spec).map_err(|e| e.to_string())?;
        fs::write(&tmp, body).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &p).map_err(|e| e.to_string())?;
        self.workspace(&spec.name).map(|_| ())
    }

    /// Removes the spec and everything the agent accumulated.
    pub fn delete(&self, name: &str) -> Result<(), String> {
        let spec = self.path("agents", name, "json")?;
        if !spec.exists() {
            return Err(format!("no agent named {name}"));
        }
        fs::remove_file(spec).map_err(|e| e.to_string())?;
        let _ = fs::remove_file(self.path("memory", name, "jsonl")?);
        let _ = fs::remove_file(self.path("runs", name, "jsonl")?);
        let _ = fs::remove_dir_all(self.dir.join("workspaces").join(name));
        Ok(())
    }

    pub fn workspace(&self, name: &str) -> Result<PathBuf, String> {
        validate_name(name)?;
        let dir = self.dir.join("workspaces").join(name);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(dir)
    }

    // ---- memory -----------------------------------------------------------

    pub fn remember(&self, agent: &str, text: &str, now: u64) -> Result<(), String> {
        append(
            &self.path("memory", agent, "jsonl")?,
            &MemoryItem { at: now, text: text.to_string() },
        )
    }

    pub fn memories(&self, agent: &str) -> Vec<MemoryItem> {
        read_lines(&self.path("memory", agent, "jsonl").unwrap_or_default())
    }

    /// Keyword-overlap recall, best first. An empty query returns the most
    /// recent. Lexical on purpose: it is cheap and explainable, and a model
    /// asked to "recall" can rephrase and ask again.
    pub fn recall(&self, agent: &str, query: &str, k: usize) -> Vec<MemoryItem> {
        let all = self.memories(agent);
        let q = tokens(query);
        if q.is_empty() {
            return all.into_iter().rev().take(k).collect();
        }
        let mut scored: Vec<(usize, MemoryItem)> = all
            .into_iter()
            .map(|m| {
                let t = tokens(&m.text);
                (q.iter().filter(|w| t.contains(w)).count(), m)
            })
            .filter(|(s, _)| *s > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.at.cmp(&a.1.at)));
        scored.into_iter().take(k).map(|(_, m)| m).collect()
    }

    // ---- runs -------------------------------------------------------------

    pub fn record_run(&self, r: &RunRecord) -> Result<(), String> {
        append(&self.path("runs", &r.agent, "jsonl")?, r)
    }

    /// Newest first.
    pub fn runs(&self, agent: &str, n: usize) -> Vec<RunRecord> {
        let mut v: Vec<RunRecord> =
            read_lines(&self.path("runs", agent, "jsonl").unwrap_or_default());
        v.reverse();
        v.truncate(n);
        v
    }

    pub fn tokens_since(&self, agent: &str, since: u64) -> u64 {
        self.runs(agent, usize::MAX)
            .iter()
            .filter(|r| r.finished >= since)
            .map(|r| r.tokens_in + r.tokens_out)
            .sum()
    }
}

fn append<T: Serialize>(path: &Path, item: &T) -> Result<(), String> {
    let mut line = serde_json::to_string(item).map_err(|e| e.to_string())?;
    line.push('\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .map_err(|e| e.to_string())
}

fn read_lines<T: for<'a> Deserialize<'a>>(path: &Path) -> Vec<T> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// A path inside `root`, or why not. Relative only, no `..`, and — after
/// resolving symlinks in whatever part already exists — still under `root`.
pub fn confine(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let p = Path::new(rel);
    if rel.is_empty()
        || p.is_absolute()
        || p.components().any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(format!("`{rel}`: use a relative path inside the workspace, with no `..`"));
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let full = root.join(p);
    let mut probe = full.as_path();
    while !probe.exists() {
        probe = probe.parent().ok_or("path has no existing parent")?;
    }
    if !probe.canonicalize().map_err(|e| e.to_string())?.starts_with(&root) {
        return Err(format!("`{rel}` resolves outside the workspace"));
    }
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = crate::testutil::dir("agent-runtime");
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn specs_persist_and_delete_removes_everything() {
        let d = tmp("spec");
        let s = Store::open(&d).unwrap();
        s.put(&AgentSpec::new("alpha", "does a thing")).unwrap();
        s.remember("alpha", "the sky is blue", 1).unwrap();
        assert_eq!(s.list().len(), 1);
        assert_eq!(Store::open(&d).unwrap().get("alpha").unwrap().description, "does a thing");
        s.delete("alpha").unwrap();
        assert!(s.get("alpha").is_none() && s.memories("alpha").is_empty());
        assert!(!s.dir.join("workspaces/alpha").exists());
        assert!(s.delete("alpha").is_err());
        assert!(s.put(&AgentSpec::new("../evil", "x")).is_err());
    }

    #[test]
    fn recall_ranks_by_overlap_then_recency() {
        let s = Store::open(tmp("mem")).unwrap();
        s.remember("a", "the build failed on tuesday", 1).unwrap();
        s.remember("a", "lunch is at noon", 2).unwrap();
        s.remember("a", "build passed after the fix", 3).unwrap();
        let hits = s.recall("a", "why did the build fail", 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].text, "build passed after the fix");
        assert_eq!(s.recall("a", "", 1)[0].text, "build passed after the fix");
        assert!(s.recall("a", "zebra", 3).is_empty());
    }

    #[test]
    fn confine_rejects_escapes() {
        let d = tmp("confine");
        fs::create_dir_all(d.join("sub")).unwrap();
        assert!(confine(&d, "notes.txt").is_ok());
        assert!(confine(&d, "sub/new/deep.txt").is_ok());
        assert!(confine(&d, "../x").is_err());
        assert!(confine(&d, "/etc/passwd").is_err());
        assert!(confine(&d, "").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/", d.join("link")).unwrap();
            assert!(confine(&d, "link/etc/passwd").is_err());
        }
    }
}
