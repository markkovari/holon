//! `virt-git` — git's object model over `blob:store`. No disk, no working copy.
//!
//! See `wit/git.wit` for why this exists. The short version: agents are
//! components and components have no filesystem, so a working copy is not
//! something an agent can ever touch — but a content-addressed object store is,
//! and that is what git already is.
//!
//! Two backing stores, for one reason each:
//!
//!   * **objects → `blob:store`.** Immutable and named by their own content, so a
//!     plain put is safe: two writers storing the same object write identical
//!     bytes to the same key.
//!   * **refs → `comp:store/cas`.** The only mutable thing in git, so the only
//!     thing that needs a guarded write. Two branches moving one ref without CAS
//!     is a lost update (ADR-0065), and the lost one is somebody's work.
//!
//! The serialisation lives in `git.rs`, tested against the real `git` binary.

#[allow(warnings)]
mod bindings;
mod git;

use bindings::blob::store::blobstore;
use bindings::comp::store::cas;
use bindings::wasi::keyvalue::store as kv;
use bindings::exports::vgit::store::objects::{
    CommitInfo, GitError, Guest as ObjectsGuest, TreeEntry,
};
use bindings::exports::vgit::store::refs::Guest as RefsGuest;
use bindings::exports::vgit::store::worktree::{Changed, Guest as WorktreeGuest, PathChange};
use bindings::wasi::config::store as config;

struct Component;

/// The container objects live in. One per repository, so two repositories in one
/// deployment cannot see each other's objects.
fn container() -> String {
    config::get("git-container").ok().flatten().filter(|s| !s.is_empty()).unwrap_or_else(|| "git".into())
}

fn store_err(e: blobstore::BlobError) -> GitError {
    match e {
        blobstore::BlobError::NotFound => GitError::NotFound("object".into()),
        blobstore::BlobError::BackendUnavailable(m) => GitError::Unavailable(m),
    }
}

/// Objects are keyed by their id. Fanned out by the first two characters the way
/// git does, so a listing of one prefix stays small on a backend that lists.
fn key(id: &str) -> String {
    format!("o/{}/{}", &id[..2.min(id.len())], id)
}

fn valid_id(id: &str) -> Result<(), GitError> {
    if id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(GitError::Invalid(format!("{id:?} is not an object id")))
    }
}

/// Write a framed object. Returns its id.
fn put_object(kind: &str, payload: &[u8]) -> Result<String, GitError> {
    let framed = git::frame(kind, payload);
    let id = git::id_of(&framed);
    // Content-addressed, so a re-write is the same bytes. Skipping the write when
    // it is already there turns a re-run into a read, which is most of why a
    // swarm exploring nearby ideas is affordable.
    match blobstore::exists(&container(), &key(&id)) {
        Ok(true) => return Ok(id),
        Ok(false) => {}
        Err(e) => return Err(store_err(e)),
    }
    blobstore::put(&container(), &key(&id), &framed, "application/x-git-object")
        .map_err(store_err)?;
    Ok(id)
}

fn get_object(id: &str, want: &str) -> Result<Vec<u8>, GitError> {
    valid_id(id)?;
    let raw = blobstore::get(&container(), &key(id)).map_err(|e| match e {
        blobstore::BlobError::NotFound => GitError::NotFound(id.to_string()),
        other => store_err(other),
    })?;
    let (kind, payload) = git::unframe(&raw).map_err(GitError::Corrupt)?;
    if kind != want {
        return Err(GitError::Corrupt(format!("{id} is a {kind}, not a {want}")));
    }
    Ok(payload.to_vec())
}

fn entry_out(e: git::Entry) -> TreeEntry {
    TreeEntry { mode: e.mode, name: e.name, id: e.id }
}

fn entry_in(e: &TreeEntry) -> git::Entry {
    git::Entry { mode: e.mode.clone(), name: e.name.clone(), id: e.id.clone() }
}

impl ObjectsGuest for Component {
    fn write_blob(content: Vec<u8>) -> Result<String, GitError> {
        put_object("blob", &content)
    }

    fn read_blob(id: String) -> Result<Vec<u8>, GitError> {
        get_object(&id, "blob")
    }

    fn write_tree(entries: Vec<TreeEntry>) -> Result<String, GitError> {
        let entries: Vec<git::Entry> = entries.iter().map(entry_in).collect();
        let payload = git::tree_payload(&entries).map_err(GitError::Invalid)?;
        put_object("tree", &payload)
    }

    fn read_tree(id: String) -> Result<Vec<TreeEntry>, GitError> {
        let payload = get_object(&id, "tree")?;
        Ok(git::parse_tree(&payload).map_err(GitError::Corrupt)?.into_iter().map(entry_out).collect())
    }

    fn write_commit(info: CommitInfo) -> Result<String, GitError> {
        let c = git::Commit {
            tree: info.tree,
            parents: info.parents,
            author: info.author,
            when: info.when,
            message: info.message,
        };
        let payload = git::commit_payload(&c).map_err(GitError::Invalid)?;
        put_object("commit", &payload)
    }

    fn read_commit(id: String) -> Result<CommitInfo, GitError> {
        let payload = get_object(&id, "commit")?;
        let c = git::parse_commit(&payload).map_err(GitError::Corrupt)?;
        Ok(CommitInfo {
            tree: c.tree,
            parents: c.parents,
            author: c.author,
            when: c.when,
            message: c.message,
        })
    }

    fn has(id: String) -> Result<bool, GitError> {
        valid_id(&id)?;
        blobstore::exists(&container(), &key(&id)).map_err(store_err)
    }
}

// ---- refs ------------------------------------------------------------------

/// A ref name that cannot collide with an object key or escape its namespace.
/// Refs live in the app's own bucket, which the host names — a guest cannot
/// choose it (ADR-0012, ADR-0023). Opened per call rather than cached: the handle
/// is cheap and a cached one outlives the reasons it was valid.
fn ref_bucket() -> Result<kv::Bucket, GitError> {
    // "default" is the name the host maps to this app's own bucket. A guest
    // cannot choose a bucket — it names one from the host's allow-list and the
    // host decides what that resolves to (ADR-0012).
    kv::open("default").map_err(|e| GitError::Unavailable(format!("opening the ref store: {e:?}")))
}

fn ref_key(name: &str) -> Result<String, GitError> {
    if name.is_empty()
        || name.starts_with('/')
        || name.ends_with('/')
        || name.contains("..")
        || name.contains("//")
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
    {
        return Err(GitError::Invalid(format!("{name:?} is not a usable ref name")));
    }
    Ok(format!("r/{name}"))
}

impl RefsGuest for Component {
    fn read(name: String) -> Result<Option<String>, GitError> {
        let k = ref_key(&name)?;
        let b = ref_bucket()?;
        match cas::get(&b, &k) {
            Ok(Some(v)) => Ok(Some(String::from_utf8_lossy(&v.value).trim().to_string())),
            Ok(None) => Ok(None),
            Err(e) => Err(GitError::Unavailable(format!("{e:?}"))),
        }
    }

    fn update(name: String, expect: Option<String>, to: String) -> Result<bool, GitError> {
        valid_id(&to)?;
        let k = ref_key(&name)?;
        // The revision to guard against comes from a READ, not from the caller —
        // the caller knows the sha it expects, and the store knows the revision
        // that sha was at. Checking both is what makes this safe against a ref
        // that moved and moved back.
        let b = ref_bucket()?;
        let current = cas::get(&b, &k).map_err(|e| GitError::Unavailable(format!("{e:?}")))?;
        let (revision, have) = match &current {
            Some(v) => (v.revision, Some(String::from_utf8_lossy(&v.value).trim().to_string())),
            None => (0, None),
        };
        if have.as_deref() != expect.as_deref() {
            // Someone else moved it. Not an error — the caller re-reads and
            // decides, which is the whole point of saying so plainly.
            return Ok(false);
        }
        match cas::set(&b, &k, to.as_bytes(), revision) {
            Ok(cas::Outcome::Committed(_)) => Ok(true),
            Ok(cas::Outcome::Conflict(_)) => Ok(false),
            Err(e) => Err(GitError::Unavailable(format!("{e:?}"))),
        }
    }

    fn list_refs(prefix: String) -> Result<Vec<(String, String)>, GitError> {
        let want = format!("r/{prefix}");
        let mut out = Vec::new();
        for info in blobstore::list_objects(&container(), &want).map_err(store_err)? {
            if let Some(name) = info.name.strip_prefix("r/") {
                if let Ok(Some(sha)) = Self::read(name.to_string()) {
                    out.push((name.to_string(), sha));
                }
            }
        }
        Ok(out)
    }

    fn delete(name: String) -> Result<(), GitError> {
        let k = ref_key(&name)?;
        blobstore::delete(&container(), &k).map_err(store_err)
    }
}

// ---- worktree --------------------------------------------------------------

/// Walk from a commit to the tree entry for `path`.
fn entry_at(commit: &str, segs: &[String]) -> Result<Option<TreeEntry>, GitError> {
    let info = <Component as ObjectsGuest>::read_commit(commit.to_string())?;
    let mut tree = info.tree;
    for (i, seg) in segs.iter().enumerate() {
        let entries = <Component as ObjectsGuest>::read_tree(tree.clone())?;
        let Some(found) = entries.into_iter().find(|e| &e.name == seg) else {
            return Ok(None);
        };
        if i + 1 == segs.len() {
            return Ok(Some(found));
        }
        if found.mode != "40000" {
            // A path continuing through a file is not a missing path, it is a
            // caller who thinks a file is a directory.
            return Err(GitError::Invalid(format!("{} is a file, not a directory", found.name)));
        }
        tree = found.id;
    }
    Ok(None)
}

/// Rewrite `tree` so that `segs` maps to `want` (or is removed), returning the
/// new tree id. Recursive, and only along the path — every sibling subtree is
/// reused by id, which is why the cost is the depth of the change rather than
/// the size of the repository.
fn splice(tree: &str, segs: &[String], want: Option<(String, String)>) -> Result<String, GitError> {
    let mut entries = if tree.is_empty() {
        Vec::new()
    } else {
        <Component as ObjectsGuest>::read_tree(tree.to_string())?
    };
    let name = &segs[0];

    if segs.len() == 1 {
        entries.retain(|e| &e.name != name);
        if let Some((mode, id)) = want {
            entries.push(TreeEntry { mode, name: name.clone(), id });
        }
    } else {
        let sub = entries
            .iter()
            .find(|e| &e.name == name && e.mode == "40000")
            .map(|e| e.id.clone())
            .unwrap_or_default();
        let new_sub = splice(&sub, &segs[1..], want)?;
        entries.retain(|e| &e.name != name);
        // An empty subtree is dropped rather than written. Git has no empty
        // directories, and keeping one would make our tree ids disagree with
        // what git would produce for the same content.
        if !new_sub.is_empty() {
            entries.push(TreeEntry { mode: "40000".into(), name: name.clone(), id: new_sub });
        }
    }

    if entries.is_empty() {
        return Ok(String::new());
    }
    <Component as ObjectsGuest>::write_tree(entries)
}

fn walk(tree: &str, prefix: &str, out: &mut Vec<String>) -> Result<(), GitError> {
    for e in <Component as ObjectsGuest>::read_tree(tree.to_string())? {
        let path = if prefix.is_empty() { e.name.clone() } else { format!("{prefix}/{}", e.name) };
        if e.mode == "40000" {
            walk(&e.id, &path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// Every (path, blob id) in a tree, for diffing.
fn flatten(tree: &str, prefix: &str, out: &mut Vec<(String, String)>) -> Result<(), GitError> {
    for e in <Component as ObjectsGuest>::read_tree(tree.to_string())? {
        let path = if prefix.is_empty() { e.name.clone() } else { format!("{prefix}/{}", e.name) };
        if e.mode == "40000" {
            flatten(&e.id, &path, out)?;
        } else {
            out.push((path, e.id));
        }
    }
    Ok(())
}

impl WorktreeGuest for Component {
    fn read_path(commit: String, path: String) -> Result<Option<Vec<u8>>, GitError> {
        let segs = git::split_path(&path).map_err(GitError::Invalid)?;
        match entry_at(&commit, &segs)? {
            Some(e) if e.mode != "40000" => {
                <Component as ObjectsGuest>::read_blob(e.id).map(Some)
            }
            Some(_) => Err(GitError::Invalid(format!("{path} is a directory"))),
            None => Ok(None),
        }
    }

    fn list_paths(commit: String, prefix: String) -> Result<Vec<String>, GitError> {
        let info = <Component as ObjectsGuest>::read_commit(commit)?;
        let mut out = Vec::new();
        walk(&info.tree, "", &mut out)?;
        if !prefix.is_empty() {
            out.retain(|p| p.starts_with(&prefix));
        }
        out.sort();
        Ok(out)
    }

    fn commit_changes(
        base: String,
        changes: Vec<PathChange>,
        author: String,
        when: u64,
        message: String,
    ) -> Result<String, GitError> {
        if changes.is_empty() {
            return Err(GitError::Invalid("no changes — that is not a commit".into()));
        }
        let (mut tree, parents) = if base.is_empty() {
            (String::new(), Vec::new())
        } else {
            let info = <Component as ObjectsGuest>::read_commit(base.clone())?;
            (info.tree, vec![base.clone()])
        };

        for c in &changes {
            let segs = git::split_path(&c.path).map_err(GitError::Invalid)?;
            let want = if c.remove {
                None
            } else {
                let id = <Component as ObjectsGuest>::write_blob(c.content.clone())?;
                let mode = if c.mode.is_empty() { "100644".to_string() } else { c.mode.clone() };
                Some((mode, id))
            };
            tree = splice(&tree, &segs, want)?;
        }

        if tree.is_empty() {
            // Deleting everything is legal in git; writing the empty tree keeps
            // that representable rather than erroring on a valid intent.
            tree = <Component as ObjectsGuest>::write_tree(Vec::new()).unwrap_or_else(|_| {
                // The empty tree's id is a constant git knows by heart.
                "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string()
            });
        }

        <Component as ObjectsGuest>::write_commit(CommitInfo {
            tree,
            parents,
            author,
            when,
            message,
        })
    }

    fn diff(before: String, after: String) -> Result<Vec<Changed>, GitError> {
        let mut a = Vec::new();
        let mut b = Vec::new();
        if !before.is_empty() {
            let info = <Component as ObjectsGuest>::read_commit(before)?;
            flatten(&info.tree, "", &mut a)?;
        }
        if !after.is_empty() {
            let info = <Component as ObjectsGuest>::read_commit(after)?;
            flatten(&info.tree, "", &mut b)?;
        }
        let mut out = Vec::new();
        for (path, id) in &b {
            match a.iter().find(|(p, _)| p == path) {
                None => out.push(Changed { path: path.clone(), kind: "added".into() }),
                Some((_, old)) if old != id => {
                    out.push(Changed { path: path.clone(), kind: "modified".into() })
                }
                Some(_) => {}
            }
        }
        for (path, _) in &a {
            if !b.iter().any(|(p, _)| p == path) {
                out.push(Changed { path: path.clone(), kind: "deleted".into() });
            }
        }
        out.sort_by(|x, y| x.path.cmp(&y.path));
        Ok(out)
    }
}

bindings::export!(Component with_types_in bindings);
