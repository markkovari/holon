//! `comp-fswatch` — what changed in a directory, for a component that cannot look.
//!
//! ## Why this is a process and not a component
//!
//! Watching a filesystem needs a syscall — inotify, FSEvents, kqueue — and a
//! `wasm32-wasip2` guest has none of them. That is the sandbox working rather
//! than a gap to route around (ADR-0095), so the part that needs an operating
//! system is native and is reached over HTTP exactly like the gate, the database
//! and the model provider are.
//!
//! `components/fs-watcher` is the component side: it holds the WIT contract and
//! dials this. Nothing here knows what a goal is.
//!
//! ## An allow-list, for the same reason `comp-checks` has one
//!
//! The directory in a request can come from a model. `comp-checks` takes
//! `--allow` because "the input comes from an agent, and 'probably fine' is not
//! a boundary", and a filesystem watcher is the same shape with a worse failure:
//! a path is a way to read somewhere nobody agreed to.
//!
//! So a request names a directory and this refuses it unless an operator listed
//! it. `--allow-path /var/log` permits `/var/log` and anything beneath it.
//! Nothing is permitted by default; a daemon started with no `--allow-path`
//! refuses everything, which is the correct behaviour for a capability nobody
//! has scoped yet.
//!
//! ## Polling, and what the cursor is
//!
//! `POST /poll {"dir": "...", "cursor": "..."}` answers with the changes since
//! that cursor. The cursor here is a millisecond timestamp; the contract says it
//! is opaque, so an implementation over a real watch queue can put an offset
//! there without the component noticing.
//!
//! An empty cursor means "start from now" and reports nothing. A first call that
//! replayed a directory's history would hand a caller thousands of events it did
//! not ask for, and there is no way for it to say it wanted only the future.
//!
//! ## What it deliberately does not do
//!
//! It does not watch. It stats the tree on each poll and compares against what it
//! saw last time, which is O(entries) per call and honest about it: a real watch
//! syscall is the next version, and the contract above was written so that this
//! one can be replaced without a caller changing.
//!
//!   comp-fswatch --addr 127.0.0.1:8car --allow-path /var/log --allow-path /tmp/x

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use axum::{extract::State, routing::post, Json, Router};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(
    name = "comp-fswatch",
    about = "Report directory changes to a component that cannot look."
)]
struct Args {
    /// Where to listen. Loopback by default: this hands out filesystem contents
    /// and has no authentication of its own.
    #[arg(long, default_value = "127.0.0.1:8car")]
    addr: String,

    /// A directory this may report on, repeatable.
    ///
    /// An allow-list rather than a root to chroot into, for the same reason
    /// egress is an allow-list: the input comes from an agent. Empty means
    /// nothing is permitted, which is what a capability nobody has scoped
    /// should do.
    #[arg(long = "allow-path")]
    allow_path: Vec<PathBuf>,

    /// Most events returned in one poll. The rest are reported as `truncated`
    /// rather than dropped silently.
    #[arg(long, default_value_t = 256)]
    page: usize,
}

/// What the last poll saw, per directory: path -> (mtime_ms, len).
///
/// Held in memory, so a restart makes every watched directory look unchanged
/// until something moves. That is the honest failure for a poller: the
/// alternative is persisting a snapshot and reporting a restart as a thousand
/// modifications.
type Snapshot = HashMap<PathBuf, (u64, u64)>;

struct Daemon {
    allowed: Vec<PathBuf>,
    page: usize,
    seen: Mutex<HashMap<PathBuf, Snapshot>>,
}

#[derive(Deserialize)]
struct PollReq {
    dir: String,
    #[serde(default)]
    cursor: String,
}

#[derive(Serialize)]
struct Event {
    path: String,
    kind: &'static str,
    at: u64,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Daemon {
    /// Is `dir` inside something an operator listed?
    ///
    /// Canonicalised on both sides before comparing, so `/var/log/../etc` is
    /// judged as `/etc` — a prefix test on the string a caller sent would let
    /// `..` walk straight out of the allow-list.
    fn permits(&self, dir: &Path) -> bool {
        let Ok(real) = dir.canonicalize() else { return false };
        self.allowed.iter().any(|a| a.canonicalize().map(|a| real.starts_with(a)).unwrap_or(false))
    }

    fn snapshot(dir: &Path) -> Snapshot {
        let mut out = Snapshot::new();
        let Ok(entries) = std::fs::read_dir(dir) else { return out };
        for e in entries.flatten() {
            let Ok(meta) = e.metadata() else { continue };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            out.insert(e.path(), (mtime, meta.len()));
        }
        out
    }
}

async fn poll(State(d): State<std::sync::Arc<Daemon>>, Json(req): Json<PollReq>) -> Json<Value> {
    let dir = PathBuf::from(&req.dir);
    if !d.permits(&dir) {
        // Not-permitted and no-such-directory are different answers on purpose:
        // one is a decision an operator made and the other is a fact about the
        // disk, and a caller retrying a refusal forever is the worse mistake.
        return Json(json!({ "error": "not-permitted", "detail": req.dir }));
    }
    if !dir.is_dir() {
        return Json(json!({ "error": "no-such-directory", "detail": req.dir }));
    }

    let fresh = Daemon::snapshot(&dir);
    let mut seen = d.seen.lock().unwrap();
    let previous = seen.get(&dir).cloned();
    seen.insert(dir.clone(), fresh.clone());
    drop(seen);

    // An empty cursor, or a directory never polled before, means "from now".
    let Some(before) = previous.filter(|_| !req.cursor.is_empty()) else {
        return Json(json!({ "events": [], "cursor": now_ms().to_string(), "truncated": false }));
    };

    let at = now_ms();
    let mut events: Vec<Event> = Vec::new();
    for (path, (mtime, len)) in &fresh {
        match before.get(path) {
            None => events.push(Event { path: path.display().to_string(), kind: "created", at }),
            Some((m, l)) if m != mtime || l != len => {
                events.push(Event { path: path.display().to_string(), kind: "modified", at })
            }
            Some(_) => {}
        }
    }
    for path in before.keys() {
        if !fresh.contains_key(path) {
            events.push(Event { path: path.display().to_string(), kind: "removed", at });
        }
    }
    // Stable order, so a caller diffing two polls sees a diff and not a shuffle.
    events.sort_by(|a, b| a.path.cmp(&b.path));

    let truncated = events.len() > d.page;
    events.truncate(d.page);
    Json(json!({ "events": events, "cursor": at.to_string(), "truncated": truncated }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.allow_path.is_empty() {
        eprintln!(
            "comp-fswatch: no --allow-path given, so every request will be refused. \
             That is deliberate — a watcher nobody has scoped watches nothing."
        );
    }
    let allowed: Vec<PathBuf> = args.allow_path.clone();
    println!(
        "comp-fswatch: listening on http://{} | {} allowed path(s) | page {}",
        args.addr,
        allowed.len(),
        args.page
    );
    let state = std::sync::Arc::new(Daemon { allowed, page: args.page, seen: Mutex::default() });
    let app = Router::new().route("/poll", post(poll)).with_state(state);
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon(allow: &[&Path]) -> Daemon {
        Daemon {
            allowed: allow.iter().map(|p| p.to_path_buf()).collect(),
            page: 256,
            seen: Mutex::default(),
        }
    }

    /// The allow-list is the boundary, and `..` is how a path escapes one.
    ///
    /// The check canonicalises before comparing, so a request for
    /// `<allowed>/../<elsewhere>` is judged as `<elsewhere>` and refused. A
    /// prefix test on the string as sent would have let it through.
    #[test]
    fn a_dot_dot_cannot_walk_out_of_the_allow_list() {
        let tmp = std::env::temp_dir().canonicalize().expect("temp dir");
        let inside = tmp.join("fswatch-allowed");
        std::fs::create_dir_all(&inside).expect("mkdir");
        let d = daemon(&[&inside]);

        assert!(d.permits(&inside), "the listed directory itself");
        assert!(!d.permits(&inside.join("..")), "the parent is not inside it");
        assert!(!d.permits(&tmp), "nor is anything above it");
        assert!(!d.permits(Path::new("/etc")), "nor is somewhere unrelated");
    }

    /// Nothing is permitted by default.
    #[test]
    fn an_unscoped_watcher_watches_nothing() {
        let d = daemon(&[]);
        assert!(!d.permits(&std::env::temp_dir()));
        assert!(!d.permits(Path::new("/")));
    }

    /// A created file, a modified one and a removed one, told apart.
    #[test]
    fn a_snapshot_diff_names_what_actually_changed() {
        let dir = std::env::temp_dir().join(format!("fswatch-diff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("stays"), b"same").expect("write");
        std::fs::write(dir.join("goes"), b"bye").expect("write");
        let before = Daemon::snapshot(&dir);

        std::fs::write(dir.join("arrives"), b"new").expect("write");
        std::fs::remove_file(dir.join("goes")).expect("rm");
        std::fs::write(dir.join("stays"), b"changed size").expect("write");
        let after = Daemon::snapshot(&dir);

        assert!(
            after.contains_key(&dir.join("arrives")) && !before.contains_key(&dir.join("arrives"))
        );
        assert!(before.contains_key(&dir.join("goes")) && !after.contains_key(&dir.join("goes")));
        assert_ne!(
            before.get(&dir.join("stays")),
            after.get(&dir.join("stays")),
            "a size change must be visible; mtime alone can be too coarse to see"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
