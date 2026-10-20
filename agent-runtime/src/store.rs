//! Everything an agent leaves on disk, under one state directory:
//!
//! ```text
//! agents/<name>.json      the spec (edit it and the next run uses it)
//! memory/<name>.jsonl     things the agent chose to remember
//! runs/<name>.jsonl       one finished run per line, with every step
//! workspaces/<name>/      the only place its file tools can touch
//! ```

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::spec::{validate_name, AgentSpec};

pub use agent_wire::{RunRecord, Status, Step};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MemoryItem {
    pub at: u64,
    pub text: String,
}

/// One vector per text, from an embedding service.
pub type Vectors = Vec<Vec<f32>>;

#[derive(Serialize, Deserialize)]
struct MemoryVec {
    text: String,
    vec: Vec<f32>,
}

/// How far below the best match a memory may score and still be recalled. Embedding scores
/// of unrelated text sit well above zero, so a fixed floor would be arbitrary.
const RECALL_SPREAD: f32 = 0.08;

/// How long a deleted agent stays in `trash/` before it is removed for good.
pub const TRASH_DAYS: u64 = 30;

/// A deleted agent waiting in `trash/`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Trashed {
    /// The directory under `trash/`: `<name>-<unix seconds of the deletion>`.
    pub id: String,
    pub name: String,
    pub deleted_at: u64,
    /// The projects it belonged to, so a restore can rejoin the ones that still exist.
    #[serde(default)]
    pub projects: Vec<String>,
    /// How many runs of it are kept.
    #[serde(default)]
    pub runs: usize,
}

#[derive(Clone)]
pub struct Store {
    dir: PathBuf,
    /// One in-memory recall index per agent, shared by every clone of the store.
    indexes: Arc<Mutex<HashMap<String, MemoryIndex>>>,
}

/// An agent's memories and their vectors, kept in memory so a recall is a dot
/// product per row and not a re-parse of two JSONL files (7.5 ms at a thousand
/// memories, 75 ms at ten thousand: bench/agent-memory). Both files are
/// append-only, so each call reads only the bytes added since the last one.
/// Vectors are stored unit-length: cosine is then a plain dot product.
#[derive(Default)]
struct MemoryIndex {
    /// Bytes of `memory/<a>.jsonl` and `memory/<a>.vecs.jsonl` consumed so far,
    /// always ending on a line boundary.
    mem_off: u64,
    side_off: u64,
    items: Vec<MemoryItem>,
    /// Row in `rows` for each text that has a vector.
    row_of: HashMap<String, usize>,
    rows: Vec<Vec<f32>>,
    /// `items[i]`'s row, once it has one. Resolved when something changed, so a
    /// recall on an unchanged index never hashes a text.
    item_row: Vec<Option<usize>>,
    dirty: bool,
}

fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
    v
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut lanes = [0f32; 8];
    let ((ca, ra), (cb, rb)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    let tail: f32 = ra.iter().zip(rb).map(|(x, y)| x * y).sum();
    for (x, y) in ca.iter().zip(cb) {
        for i in 0..8 {
            lanes[i] += x[i] * y[i];
        }
    }
    lanes.iter().sum::<f32>() + tail
}

/// Complete lines appended to `path` after `*off`, advancing `*off` past them.
/// A file that shrank was replaced: `None`, and the caller starts over.
fn read_new_lines<T: for<'a> Deserialize<'a>>(path: &Path, off: &mut u64) -> Option<Vec<T>> {
    let Ok(mut f) = fs::File::open(path) else {
        return (*off == 0).then(Vec::new);
    };
    let len = f.metadata().ok()?.len();
    if len < *off {
        return None;
    }
    if len == *off {
        return Some(vec![]);
    }
    f.seek(SeekFrom::Start(*off)).ok()?;
    let mut buf = Vec::with_capacity((len - *off) as usize);
    f.take(len - *off).read_to_end(&mut buf).ok()?;
    // Only whole lines: a writer may be mid-append.
    let end = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    *off += end as u64;
    Some(
        String::from_utf8_lossy(&buf[..end])
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect(),
    )
}

impl MemoryIndex {
    fn add_vec(&mut self, text: String, vec: Vec<f32>) {
        if !self.row_of.contains_key(&text) {
            self.row_of.insert(text, self.rows.len());
            self.rows.push(unit(vec));
            self.dirty = true;
        }
    }

    fn resolve(&mut self) {
        if !self.dirty && self.item_row.len() == self.items.len() {
            return;
        }
        self.item_row.resize(self.items.len(), None);
        for (m, r) in self.items.iter().zip(self.item_row.iter_mut()) {
            if r.is_none() {
                *r = self.row_of.get(&m.text).copied();
            }
        }
        self.dirty = false;
    }

    /// Catch up with both files. Rebuilds from scratch if either was replaced.
    fn sync(&mut self, mem: &Path, side: &Path) {
        for attempt in 0..2 {
            let items = read_new_lines::<MemoryItem>(mem, &mut self.mem_off);
            let vecs = read_new_lines::<MemoryVec>(side, &mut self.side_off);
            match (items, vecs) {
                (Some(items), Some(vecs)) => {
                    self.items.extend(items);
                    self.dirty = true;
                    for v in vecs {
                        self.add_vec(v.text, v.vec);
                    }
                    return;
                }
                _ if attempt == 0 => *self = Self::default(),
                _ => return,
            }
        }
    }
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
        for sub in ["agents", "memory", "runs", "workspaces", "projects"] {
            fs::create_dir_all(dir.join(sub)).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok(Self { dir, indexes: Arc::default() })
    }

    /// Where shared connector definitions live (`connectors/<name>.json`); may not exist.
    pub fn connectors_dir(&self) -> PathBuf {
        self.dir.join("connectors")
    }

    /// A model block by name, from `<state>/models.json` (`{"qwen": {"kind": "open_ai", ...}}`).
    pub fn model_alias(&self, name: &str) -> Option<serde_json::Value> {
        let all: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(self.dir.join("models.json")).ok()?).ok()?;
        all.get(name).cloned()
    }

    fn path(&self, sub: &str, name: &str, ext: &str) -> Result<PathBuf, String> {
        validate_name(name)?;
        Ok(self.dir.join(sub).join(format!("{name}.{ext}")))
    }

    pub fn list_projects(&self) -> Vec<crate::projects::Project> {
        let mut v: Vec<crate::projects::Project> = fs::read_dir(self.dir.join("projects"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| fs::read_to_string(e.path()).ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn get_project(&self, name: &str) -> Option<crate::projects::Project> {
        let p = self.path("projects", name, "json").ok()?;
        serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
    }

    /// Atomic, like `put`.
    pub fn put_project(&self, project: &crate::projects::Project) -> Result<(), String> {
        let p = self.path("projects", &project.name, "json")?;
        let tmp = p.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(project).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        fs::rename(&tmp, &p).map_err(|e| e.to_string())
    }

    pub fn delete_project(&self, name: &str) -> Result<(), String> {
        let p = self.path("projects", name, "json")?;
        if !p.exists() {
            return Err(format!("no project named {name}"));
        }
        fs::remove_file(p).map_err(|e| e.to_string())
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

    /// The files an agent accumulates, as (where they live, their name in the trash).
    fn agent_files(&self, name: &str) -> Result<[(PathBuf, &'static str); 5], String> {
        Ok([
            (self.path("agents", name, "json")?, "agent.json"),
            (self.path("memory", name, "jsonl")?, "memory.jsonl"),
            (self.path("memory", name, "vecs.jsonl")?, "memory.vecs.jsonl"),
            (self.path("runs", name, "jsonl")?, "runs.jsonl"),
            (self.dir.join("workspaces").join(name), "workspace"),
        ])
    }

    /// Takes the agent out of service: the spec, its runs, memory and workspace move to
    /// `trash/<name>-<time>/` and stay there for [`TRASH_DAYS`], so a deletion is
    /// recoverable (`restore`). Nothing is removed for good here.
    pub fn delete(&self, name: &str) -> Result<(), String> {
        let spec = self.path("agents", name, "json")?;
        if !spec.exists() {
            return Err(format!("no agent named {name}"));
        }
        let at = crate::runtime::unix_now();
        fs::create_dir_all(self.dir.join("trash")).map_err(|e| e.to_string())?;
        // Two deletions of one name in the same second must not share a directory.
        let (id, dir) = (0..)
            .map(|n| if n == 0 { format!("{name}-{at}") } else { format!("{name}-{at}-{n}") })
            .map(|id| (self.dir.join("trash").join(&id), id))
            .find_map(|(dir, id)| fs::create_dir(&dir).ok().map(|()| (id, dir)))
            .expect("an unused trash id");
        let meta = Trashed {
            id,
            name: name.to_string(),
            deleted_at: at,
            projects: self
                .list_projects()
                .into_iter()
                .filter(|p| p.agents.iter().any(|a| a == name))
                .map(|p| p.name)
                .collect(),
            runs: self.runs(name, usize::MAX).len(),
        };
        fs::write(dir.join("meta.json"), serde_json::to_string(&meta).unwrap_or_default())
            .map_err(|e| e.to_string())?;
        for (from, to) in self.agent_files(name)? {
            if from.exists() {
                fs::rename(&from, dir.join(to)).map_err(|e| format!("{}: {e}", from.display()))?;
            }
        }
        self.indexes.lock().unwrap().remove(name);
        Ok(())
    }

    /// What is in the trash, newest deletion first.
    pub fn trash(&self) -> Vec<Trashed> {
        let mut v: Vec<Trashed> = fs::read_dir(self.dir.join("trash"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| fs::read_to_string(e.path().join("meta.json")).ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect();
        v.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at).then(b.id.cmp(&a.id)));
        v
    }

    /// An entry by its id, else the newest one for that agent name. The text from outside
    /// is only ever compared with what `trash` listed, never joined into a path.
    fn trashed(&self, id_or_name: &str) -> Result<Trashed, String> {
        let all = self.trash();
        all.iter()
            .find(|t| t.id == id_or_name)
            .or_else(|| all.iter().find(|t| t.name == id_or_name))
            .cloned()
            .ok_or_else(|| {
                if all.is_empty() {
                    format!("no deleted agent `{id_or_name}`: the trash is empty")
                } else {
                    format!(
                        "no deleted agent `{id_or_name}`; in the trash: {}",
                        all.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
                    )
                }
            })
    }

    /// Puts a deleted agent back exactly as it was. Refused while an agent of that name
    /// exists, so a restore cannot overwrite anything.
    pub fn restore(&self, id_or_name: &str) -> Result<Trashed, String> {
        let t = self.trashed(id_or_name)?;
        let files = self.agent_files(&t.name)?;
        if files.iter().any(|(live, _)| live.exists()) {
            return Err(format!(
                "an agent named {} exists; delete or rename it first, then restore",
                t.name
            ));
        }
        let dir = self.dir.join("trash").join(&t.id);
        for (live, kept) in &files {
            if dir.join(kept).exists() {
                if let Some(parent) = live.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                fs::rename(dir.join(kept), live).map_err(|e| format!("{kept}: {e}"))?;
            }
        }
        self.indexes.lock().unwrap().remove(&t.name);
        let _ = fs::remove_dir_all(dir);
        Ok(t)
    }

    /// Removes one entry from the trash for good.
    pub fn purge(&self, id_or_name: &str) -> Result<Trashed, String> {
        let t = self.trashed(id_or_name)?;
        fs::remove_dir_all(self.dir.join("trash").join(&t.id)).map_err(|e| e.to_string())?;
        Ok(t)
    }

    /// Removes what has been in the trash longer than [`TRASH_DAYS`].
    pub fn sweep_trash(&self, now: u64) -> usize {
        let old: Vec<Trashed> = self
            .trash()
            .into_iter()
            .filter(|t| t.deleted_at + TRASH_DAYS * 86_400 <= now)
            .collect();
        old.iter()
            .filter(|t| fs::remove_dir_all(self.dir.join("trash").join(&t.id)).is_ok())
            .count()
    }

    pub fn workspace(&self, name: &str) -> Result<PathBuf, String> {
        validate_name(name)?;
        let dir = self.dir.join("workspaces").join(name);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(dir)
    }

    /// Puts a file a person sent (a photo from a chat) into the agent's `inbox/` folder under
    /// a name that cannot collide or escape, and returns that name. Only photos for now.
    pub fn save_inbox(&self, name: &str, filename: &str, bytes: &[u8]) -> Result<String, String> {
        const EXTS: [&str; 6] = ["jpg", "jpeg", "png", "webp", "gif", "heic"];
        const MAX: usize = 20 << 20;
        if self.get(name).is_none() {
            return Err(format!("no agent named {name}"));
        }
        if bytes.is_empty() || bytes.len() > MAX {
            return Err(format!("a photo must be 1 byte to {} MB", MAX >> 20));
        }
        // Only the last path component, and only characters that are safe in a file name.
        let base = filename.rsplit(['/', '\\']).next().unwrap_or_default();
        let safe: String = base
            .chars()
            .map(
                |c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' },
            )
            .collect();
        let ext = safe.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        if !EXTS.contains(&ext.as_str()) {
            return Err(format!("not a photo: use one of {}", EXTS.join(", ")));
        }
        let stem: String = safe.trim_start_matches('.').chars().take(60).collect();
        let dir = self.workspace(name)?.join("inbox");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = format!("{}-{stem}", crate::runtime::unix_now_ms());
        fs::write(dir.join(&file), bytes).map_err(|e| e.to_string())?;
        Ok(file)
    }


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

    /// Recall by meaning. `query` is the embedded question; `embed_docs` embeds memory texts
    /// that have no stored vector yet (`None` when the service is down, and then those are
    /// skipped). Vectors live in a sidecar file keyed by text, so memories stay append-only.
    /// Returns the best `k`, dropping any clearly worse than the best match.
    pub fn recall_semantic(
        &self,
        agent: &str,
        query: &[f32],
        k: usize,
        embed_docs: &dyn Fn(&[String]) -> Option<Vectors>,
    ) -> Vec<MemoryItem> {
        let (Ok(mem), Ok(side)) =
            (self.path("memory", agent, "jsonl"), self.path("memory", agent, "vecs.jsonl"))
        else {
            return vec![];
        };
        // The embedding call can be slow, so it runs without the lock held.
        let missing: Vec<String> = {
            let mut all = self.indexes.lock().unwrap();
            let ix = all.entry(agent.to_string()).or_default();
            ix.sync(&mem, &side);
            ix.resolve();
            ix.items
                .iter()
                .zip(&ix.item_row)
                .filter(|(_, r)| r.is_none())
                .map(|(m, _)| m.text.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        let fresh = if missing.is_empty() { None } else { embed_docs(&missing) };
        let q = unit(query.to_vec());
        let mut all = self.indexes.lock().unwrap();
        let ix = all.entry(agent.to_string()).or_default();
        if let Some(vecs) = fresh {
            for (text, vec) in missing.into_iter().zip(vecs) {
                let _ = append(&side, &MemoryVec { text: text.clone(), vec: vec.clone() });
                ix.add_vec(text, vec);
            }
        }
        ix.resolve();
        let scored: Vec<(f32, &MemoryItem)> = ix
            .items
            .iter()
            .zip(&ix.item_row)
            .filter_map(|(m, r)| r.map(|r| (dot(&q, &ix.rows[r]), m)))
            .collect();
        let best = scored.iter().map(|s| s.0).fold(f32::NEG_INFINITY, f32::max);
        let mut kept: Vec<(f32, &MemoryItem)> =
            scored.into_iter().filter(|s| s.0 >= best - RECALL_SPREAD).collect();
        kept.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.at.cmp(&a.1.at)));
        kept.into_iter().take(k).map(|s| s.1.clone()).collect()
    }


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

    /// Every run in one trace, across all agents, oldest first.
    pub fn trace(&self, trace_id: &str) -> Vec<RunRecord> {
        let mut v: Vec<RunRecord> = self
            .list()
            .iter()
            .flat_map(|a| self.runs(&a.name, usize::MAX))
            .filter(|r| r.trace_id == trace_id)
            .collect();
        v.sort_by(|a, b| a.started.cmp(&b.started).then(a.id.cmp(&b.id)));
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
        let d = crate::testutil::dir(&format!("agent-runtime-{tag}"));
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
    fn semantic_recall_ranks_by_vector_embeds_each_text_once_and_survives_a_down_service() {
        let s = Store::open(tmp("sem")).unwrap();
        s.remember("a", "rowed 12k on tuesday", 1).unwrap();
        s.remember("a", "lunch is at noon", 2).unwrap();
        s.remember("a", "new pb on the erg", 3).unwrap();
        let calls = std::cell::Cell::new(0);
        // a toy space: dimension 0 is "rowing", dimension 1 is "food"
        let embed = |t: &[String]| {
            calls.set(calls.get() + t.len());
            Some(
                t.iter()
                    .map(|x| if x.contains("lunch") { vec![0.0, 1.0] } else { vec![1.0, 0.1] })
                    .collect(),
            )
        };
        let hits = s.recall_semantic("a", &[1.0, 0.0], 5, &embed);
        let texts: Vec<_> = hits.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["new pb on the erg", "rowed 12k on tuesday"]);
        assert_eq!(calls.get(), 3);
        let again = s.recall_semantic("a", &[0.0, 1.0], 5, &embed);
        assert_eq!(again[0].text, "lunch is at noon");
        assert_eq!(calls.get(), 3, "vectors are kept, not recomputed");
        // service down: what has a vector still works; a new memory just is not found yet
        s.remember("a", "ran a 5k", 4).unwrap();
        let down = |_: &[String]| None;
        assert_eq!(s.recall_semantic("a", &[1.0, 0.0], 5, &down).len(), 2);
    }

    fn toy(t: &[String]) -> Option<Vectors> {
        Some(
            t.iter()
                .map(|x| if x.contains("lunch") { vec![0.0, 1.0] } else { vec![1.0, 0.1] })
                .collect(),
        )
    }

    #[test]
    fn the_index_picks_up_appended_memories_without_rereading_and_agents_do_not_mix() {
        let s = Store::open(tmp("incr")).unwrap();
        s.remember("a", "rowed 12k", 1).unwrap();
        s.remember("b", "lunch is at noon", 1).unwrap();
        assert_eq!(s.recall_semantic("a", &[1.0, 0.0], 5, &toy).len(), 1);
        s.remember("a", "new pb", 2).unwrap();
        let texts: Vec<_> =
            s.recall_semantic("a", &[1.0, 0.0], 5, &toy).into_iter().map(|m| m.text).collect();
        assert_eq!(texts, ["new pb", "rowed 12k"]);
        // b's memory is never visible to a, whatever the query is closest to
        assert!(!texts.iter().any(|t| t.contains("lunch")));
        assert_eq!(s.recall_semantic("b", &[0.0, 1.0], 5, &toy)[0].text, "lunch is at noon");
    }

    #[test]
    fn a_half_written_line_is_ignored_until_it_is_complete() {
        let d = tmp("torn");
        let s = Store::open(&d).unwrap();
        s.remember("a", "rowed 12k", 1).unwrap();
        assert_eq!(s.recall_semantic("a", &[1.0, 0.0], 5, &toy).len(), 1);
        let path = d.join("memory/a.jsonl");
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"at\":2,\"text\":\"new p").unwrap();
        assert_eq!(s.recall_semantic("a", &[1.0, 0.0], 5, &toy).len(), 1);
        f.write_all(b"b\"}\n").unwrap();
        assert_eq!(s.recall_semantic("a", &[1.0, 0.0], 5, &toy).len(), 2);
    }

    #[test]
    fn a_replaced_or_deleted_memory_file_rebuilds_the_index() {
        let d = tmp("replace");
        let s = Store::open(&d).unwrap();
        s.put(&AgentSpec::new("a", "x")).unwrap();
        s.remember("a", "rowed 12k and then rowed some more", 1).unwrap();
        assert_eq!(s.recall_semantic("a", &[1.0, 0.0], 5, &toy).len(), 1);
        // replaced by a shorter file behind the store's back
        fs::write(d.join("memory/a.jsonl"), "{\"at\":9,\"text\":\"lunch\"}\n").unwrap();
        fs::remove_file(d.join("memory/a.vecs.jsonl")).unwrap();
        let hits = s.recall_semantic("a", &[0.0, 1.0], 5, &toy);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "lunch");
        // deleting the agent forgets its vectors too
        s.delete("a").unwrap();
        assert!(s.recall_semantic("a", &[0.0, 1.0], 5, &toy).is_empty());
        assert!(!d.join("memory/a.vecs.jsonl").exists());
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
