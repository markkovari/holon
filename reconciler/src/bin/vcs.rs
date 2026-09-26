//! `comp-vcs` — the agent-native code store (ADR-0099), served over loopback
//! HTTP to the `vcs-store` component, which gives every other component
//! `holon:vcs/code-store` and `holon:vcs/files`.
//!
//! ## ADR-0095's three questions
//!
//! 1. **Something WASI does not give a guest?** Yes: a held NATS connection
//!    (JetStream ObjectStore + KV, compare-and-set) and a SurrealDB WebSocket.
//!    `async-nats` and `surrealdb` need sockets and a runtime; the host wires no
//!    `wasi:sockets`. `materialize` also writes a directory, which a component
//!    cannot (ADR-0023).
//! 2. **The smallest it could be?** Every decision — commutation, conflicts,
//!    names, positions, the write order and its recovery — is `holon-vcs`'s
//!    engine, the same code that builds for `wasm32-wasip2`; this file is the
//!    engine's two native adapters, a JSON mapping one route per WIT function,
//!    an allow-list for `materialize`, and startup repair. No policy.
//! 3. **A contract a component could have answered?** Yes, and it stays WIT:
//!    `wit/vcs/vcs.wit`. `components/vcs-store` exports it and makes each call
//!    one request here; `CONTRACT.md` beside this file's component is the wire.
//!
//! ## Routes
//!
//! `POST /v1/<wit function>` (`apply-patch`, `resolve-conflict`, `revert-op`,
//! `query-symbol`, `snapshot-export`, `list-conflicts`, `oplog`, `oplog-head`,
//! `verify`, `repair`, `ingest-file`, `ingest-tree`, `materialize`,
//! `read-blob`), JSON in and out as `holon_vcs::wire` defines, errors as
//! `{error, detail, message}` with a status by kind. `GET /health` is open;
//! everything else takes `Authorization: Bearer <token>` when `--token` is set.
//!
//! ## Startup
//!
//! Before it listens, it runs `repair` on every workspace the oplog bucket
//! knows: ops a previous process left half-done are rolled forward when their
//! commit point happened, and aborted when it cannot happen any more (past the
//! lease — `--lease-secs`). `/health` reports what that did.
//!
//! ## Crash injection — for tests only
//!
//! `--crash-after-step <step>[:<n>]` / `--crash-before-step <step>[:<n>]` abort
//! the process (`std::process::abort`, no unwinding, nothing flushed) at the
//! `n`-th write of a named step of the write order, counted from the first
//! request after startup repair:
//!
//! | step | the write |
//! |---|---|
//! | `blob`    | a content blob put (before the op exists) |
//! | `intent`  | the op appended to the log, `pending` |
//! | `graph`   | a write-ahead graph record (patch, symbol index, conflict) |
//! | `claim`   | a name reservation taken |
//! | `tip`     | a symbol pointer compare-and-set — the commit point |
//! | `finish`  | a graph write after the commit point (status, mirror, conflict state) |
//! | `release` | a name reservation given back |
//! | `commit`  | the op moved `pending → committed` (or `aborted`) |
//!
//! It refuses to start with either unless `--i-know-this-is-a-test` is given
//! too, and says so on every line it logs.
//!
//!   comp-vcs --addr 127.0.0.1:8014 --nats-url nats://127.0.0.1:4222 \
//!            --surreal-url 127.0.0.1:8000 --allow-path /srv/checkouts

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use axum::extract::{DefaultBodyLimit, Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use holon_vcs::error::VcsError;
use holon_vcs::graph::{
    ConflictEffect, ConflictRecord, Graph, PatchRecord, PatchStatus, SymbolRecord, SymbolUpdate,
};
use holon_vcs::model::{
    ConflictState, Hash, OpId, PatchRequest, RepairReport, ResolutionRequest, SymbolId,
};
use holon_vcs::oplog::{NewOp, OpLog, OpState, StoredOp};
use holon_vcs::store::{BlobStore, CasMismatch, PointerStore, PointerValue, Revision};
use holon_vcs::{extract, git, wire, Engine};

type VResult<T> = std::result::Result<T, VcsError>;

/// Request bodies up to this size: an `ingest-tree` of a component's sources.
const MAX_BODY: usize = 64 * 1024 * 1024;

#[derive(Parser, Debug)]
#[command(
    name = "comp-vcs",
    about = "The agent-native code store (ADR-0099), for components over loopback HTTP."
)]
struct Args {
    /// Shared secret a caller must send as `Authorization: Bearer <token>`.
    /// See `comp_reconciler::daemon_auth`. No token means no check, said loudly.
    #[arg(long)]
    token: Option<String>,
    /// Same, read from a file (a systemd `LoadCredential` path). Wins over `--token`.
    #[arg(long)]
    token_file: Option<PathBuf>,

    /// Where to listen. Loopback by default.
    #[arg(long, default_value = "127.0.0.1:8014")]
    addr: String,

    /// NATS with JetStream: blobs (ObjectStore), pointers and the oplog (KV).
    #[arg(long, env = "VCS_NATS_URL", default_value = "nats://127.0.0.1:4222")]
    nats_url: String,
    /// Bucket names are `<prefix>-blobs`, `<prefix>-pointers`, `<prefix>-oplog`.
    #[arg(long, default_value = "holon-vcs")]
    bucket_prefix: String,

    /// SurrealDB (`host:port`, WebSocket): the symbol/patch graph.
    #[arg(long, env = "VCS_SURREAL_URL", default_value = "127.0.0.1:8000")]
    surreal_url: String,
    #[arg(long, default_value = "holon")]
    surreal_ns: String,
    #[arg(long, default_value = "vcs")]
    surreal_db: String,
    #[arg(long, default_value = "root")]
    surreal_user: String,
    /// The compose SurrealDB's well-known dev password; set a real one.
    #[arg(long, default_value = "root")]
    surreal_pass: String,
    /// Same, from a file. Wins over `--surreal-pass`.
    #[arg(long)]
    surreal_pass_file: Option<PathBuf>,

    /// Everything in memory, nothing durable: for trying it and for tests.
    /// Ignores the NATS and SurrealDB flags.
    #[arg(long)]
    memory: bool,

    /// How long a pending op is presumed in flight before a reader or repair
    /// fences and aborts it. Fractions allowed.
    #[arg(long, default_value_t = 30.0)]
    lease_secs: f64,

    /// A directory `materialize` may write under, repeatable. Empty: every
    /// `materialize` is refused.
    #[arg(long = "allow-path")]
    allow_path: Vec<PathBuf>,

    /// TEST ONLY: abort the process right after the n-th write of a step (see
    /// the module docs). `<step>[:<n>]`, n from 1.
    #[arg(long, value_name = "STEP[:N]")]
    crash_after_step: Option<String>,
    /// TEST ONLY: abort right before the n-th write of a step.
    #[arg(long, value_name = "STEP[:N]")]
    crash_before_step: Option<String>,
    /// Required with either crash flag.
    #[arg(long)]
    i_know_this_is_a_test: bool,
}

// ---- fault injection ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Blob,
    Intent,
    Graph,
    Claim,
    Tip,
    Finish,
    Release,
    Commit,
}

impl Step {
    const ALL: [Step; 8] = [
        Step::Blob,
        Step::Intent,
        Step::Graph,
        Step::Claim,
        Step::Tip,
        Step::Finish,
        Step::Release,
        Step::Commit,
    ];
    fn name(self) -> &'static str {
        match self {
            Step::Blob => "blob",
            Step::Intent => "intent",
            Step::Graph => "graph",
            Step::Claim => "claim",
            Step::Tip => "tip",
            Step::Finish => "finish",
            Step::Release => "release",
            Step::Commit => "commit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum When {
    Before,
    After,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Plan {
    when: When,
    step: Step,
    n: u32,
}

impl Plan {
    fn parse(when: When, s: &str) -> Result<Self> {
        let (name, n) = match s.split_once(':') {
            Some((a, b)) => {
                (a, b.parse::<u32>().with_context(|| format!("{s:?}: n is not a number"))?)
            }
            None => (s, 1),
        };
        if n == 0 {
            bail!("{s:?}: n counts from 1");
        }
        let step = Step::ALL.into_iter().find(|st| st.name() == name).with_context(|| {
            let names: Vec<_> = Step::ALL.iter().map(|s| s.name()).collect();
            format!("{name:?} is not a step; one of {}", names.join(", "))
        })?;
        Ok(Plan { when, step, n })
    }

    fn describe(&self) -> String {
        let w = if self.when == When::Before { "before" } else { "after" };
        format!("{w}:{}:{}", self.step.name(), self.n)
    }
}

/// Counts writes by step and, when a plan is armed, aborts at its point. With
/// `trace`, records the steps it saw (for the unit tests that pin the write
/// order this daemon relies on).
#[derive(Default)]
struct Fuse {
    plan: Option<Plan>,
    armed: AtomicBool,
    seen: AtomicU32,
    trace: Option<Mutex<Vec<Step>>>,
}

impl Fuse {
    fn at(&self, when: When, step: Step) {
        if when == When::After {
            if let Some(t) = &self.trace {
                t.lock().unwrap().push(step);
            }
        }
        let Some(plan) = self.plan else { return };
        if plan.when != when || plan.step != step || !self.armed.load(Ordering::SeqCst) {
            return;
        }
        let n = self.seen.fetch_add(1, Ordering::SeqCst) + 1;
        if n == plan.n {
            eprintln!("comp-vcs: TEST FAULT INJECTION: aborting {}", plan.describe());
            std::process::abort();
        }
    }
    async fn write<T>(&self, step: Step, f: impl std::future::Future<Output = T>) -> T {
        self.at(When::Before, step);
        let out = f.await;
        self.at(When::After, step);
        out
    }
}

/// A store behind the fuse. Reads pass straight through.
struct Fault<T> {
    inner: T,
    fuse: Arc<Fuse>,
}

impl<T: BlobStore> BlobStore for Fault<T> {
    async fn put(&self, bytes: Vec<u8>) -> VResult<Hash> {
        self.fuse.write(Step::Blob, self.inner.put(bytes)).await
    }
    async fn get(&self, hash: &str) -> VResult<Option<Vec<u8>>> {
        self.inner.get(hash).await
    }
    async fn contains(&self, hash: &str) -> VResult<bool> {
        self.inner.contains(hash).await
    }
}

impl<T: PointerStore> PointerStore for Fault<T> {
    async fn get(&self, key: &str) -> VResult<Option<(PointerValue, Revision)>> {
        self.inner.get(key).await
    }
    async fn cas(
        &self,
        key: &str,
        expected: Option<Revision>,
        value: &PointerValue,
    ) -> VResult<std::result::Result<Revision, CasMismatch>> {
        let step = if !key.contains("/name/") {
            Step::Tip
        } else if value.value.is_some() {
            Step::Claim
        } else {
            Step::Release
        };
        self.fuse.write(step, self.inner.cas(key, expected, value)).await
    }
}

impl<T: OpLog> OpLog for Fault<T> {
    async fn append(&self, ws: &str, op: NewOp) -> VResult<StoredOp> {
        self.fuse.write(Step::Intent, self.inner.append(ws, op)).await
    }
    async fn get(&self, ws: &str, id: OpId) -> VResult<Option<StoredOp>> {
        self.inner.get(ws, id).await
    }
    async fn finish(&self, ws: &str, id: OpId, state: OpState) -> VResult<OpState> {
        self.fuse.write(Step::Commit, self.inner.finish(ws, id, state)).await
    }
    async fn list(&self, ws: &str, after: Option<OpId>, limit: u32) -> VResult<Vec<StoredOp>> {
        self.inner.list(ws, after, limit).await
    }
    async fn latest(&self, ws: &str) -> VResult<Option<OpId>> {
        self.inner.latest(ws).await
    }
    async fn settled(&self, ws: &str) -> VResult<OpId> {
        self.inner.settled(ws).await
    }
    async fn advance_settled(&self, ws: &str, to: OpId) -> VResult<()> {
        // A hint, not part of any op's write order.
        self.inner.advance_settled(ws, to).await
    }
}

impl<T: Graph> Graph for Fault<T> {
    async fn ensure_symbol(&self, ws: &str, key: &str, id: &SymbolId) -> VResult<()> {
        self.fuse.write(Step::Graph, self.inner.ensure_symbol(ws, key, id)).await
    }
    async fn update_symbol(&self, ws: &str, key: &str, u: SymbolUpdate) -> VResult<()> {
        self.fuse.write(Step::Finish, self.inner.update_symbol(ws, key, u)).await
    }
    async fn symbol(&self, ws: &str, key: &str) -> VResult<Option<SymbolRecord>> {
        self.inner.symbol(ws, key).await
    }
    async fn symbols_named(&self, ws: &str, id: &SymbolId) -> VResult<Vec<String>> {
        self.inner.symbols_named(ws, id).await
    }
    async fn symbols_in_component(&self, ws: &str, c: &str) -> VResult<Vec<SymbolRecord>> {
        self.inner.symbols_in_component(ws, c).await
    }
    async fn put_patch(&self, ws: &str, p: &PatchRecord) -> VResult<()> {
        self.fuse.write(Step::Graph, self.inner.put_patch(ws, p)).await
    }
    async fn patch(&self, ws: &str, hash: &str) -> VResult<Option<PatchRecord>> {
        self.inner.patch(ws, hash).await
    }
    async fn mark_patch(
        &self,
        ws: &str,
        hash: &str,
        status: PatchStatus,
        op: OpId,
        set_op: bool,
    ) -> VResult<()> {
        self.fuse.write(Step::Finish, self.inner.mark_patch(ws, hash, status, op, set_op)).await
    }
    async fn dependents(&self, ws: &str, target: &SymbolId) -> VResult<Vec<Hash>> {
        self.inner.dependents(ws, target).await
    }
    async fn put_conflict(&self, ws: &str, c: &ConflictRecord, op: OpId) -> VResult<()> {
        self.fuse.write(Step::Graph, self.inner.put_conflict(ws, c, op)).await
    }
    async fn abandon_if_uncommitted(&self, ws: &str, id: &str, op: OpId) -> VResult<()> {
        self.fuse.write(Step::Finish, self.inner.abandon_if_uncommitted(ws, id, op)).await
    }
    async fn apply_conflict_effect(&self, ws: &str, e: &ConflictEffect, op: OpId) -> VResult<()> {
        self.fuse.write(Step::Finish, self.inner.apply_conflict_effect(ws, e, op)).await
    }
    async fn conflict(&self, ws: &str, id: &str) -> VResult<Option<ConflictRecord>> {
        self.inner.conflict(ws, id).await
    }
    async fn conflicts(
        &self,
        ws: &str,
        state: Option<ConflictState>,
    ) -> VResult<Vec<ConflictRecord>> {
        self.inner.conflicts(ws, state).await
    }
    async fn open_conflicts_for(&self, ws: &str, key: &str) -> VResult<Vec<ConflictRecord>> {
        self.inner.open_conflicts_for(ws, key).await
    }
}

// ---- backends -------------------------------------------------------------------

type LiveEngine = Engine<
    Fault<holon_vcs::nats::NatsBlobs>,
    Fault<holon_vcs::store::KvPointers<holon_vcs::nats::NatsKv>>,
    Fault<holon_vcs::surreal::SurrealGraph>,
    Fault<holon_vcs::oplog::KvOpLog<holon_vcs::nats::NatsKv>>,
>;
type MemEngine = Engine<
    Fault<holon_vcs::mem::MemBlobs>,
    Fault<holon_vcs::mem::MemPointers>,
    Fault<holon_vcs::mem::MemGraph>,
    Fault<holon_vcs::mem::MemOpLog>,
>;

enum Backend {
    Live(Box<LiveEngine>),
    Mem(Box<MemEngine>),
}

/// Run `$body` with `$e` bound to the engine, whichever backend it is.
macro_rules! on {
    ($d:expr, |$e:ident| $body:expr) => {
        match &$d.backend {
            Backend::Live($e) => $body,
            Backend::Mem($e) => $body,
        }
    };
}

fn wrap<T>(inner: T, fuse: &Arc<Fuse>) -> Fault<T> {
    Fault { inner, fuse: fuse.clone() }
}

fn mem_backend(fuse: &Arc<Fuse>, lease_ms: u64) -> Backend {
    let e = Engine::new(
        wrap(holon_vcs::mem::MemBlobs::new(), fuse),
        wrap(holon_vcs::mem::pointers(), fuse),
        wrap(holon_vcs::mem::MemGraph::new(), fuse),
        wrap(holon_vcs::mem::oplog(), fuse),
    )
    .with_lease_ms(lease_ms);
    Backend::Mem(Box::new(e))
}

async fn live_backend(args: &Args, fuse: &Arc<Fuse>, lease_ms: u64) -> Result<Backend> {
    let cfg = holon_vcs::nats::NatsConfig::prefixed(&args.bucket_prefix);
    let (blobs, pointers, log) = holon_vcs::nats::connect(&args.nats_url, &cfg)
        .await
        .with_context(|| format!("NATS at {}", args.nats_url))?;
    let mut sc = holon_vcs::surreal::SurrealConfig::new(&args.surreal_url);
    sc.namespace = args.surreal_ns.clone();
    sc.database = args.surreal_db.clone();
    sc.username = args.surreal_user.clone();
    sc.password = match &args.surreal_pass_file {
        Some(p) => std::fs::read_to_string(p)
            .with_context(|| format!("--surreal-pass-file {}", p.display()))?
            .trim()
            .to_string(),
        None => args.surreal_pass.clone(),
    };
    let graph = holon_vcs::surreal::SurrealGraph::connect(&sc)
        .await
        .with_context(|| format!("SurrealDB at {}", args.surreal_url))?;
    let e =
        Engine::new(wrap(blobs, fuse), wrap(pointers, fuse), wrap(graph, fuse), wrap(log, fuse))
            .with_lease_ms(lease_ms);
    Ok(Backend::Live(Box::new(e)))
}

// ---- the daemon -----------------------------------------------------------------

struct Daemon {
    backend: Backend,
    allowed: Vec<PathBuf>,
    fuse: Arc<Fuse>,
    lease_ms: u64,
    /// Workspaces this process repaired at startup or has served since.
    seen: Mutex<BTreeSet<String>>,
    startup: Mutex<Vec<Value>>,
}

/// A route's answer: the status and the JSON body.
type Answer = (u16, Value);

fn bad_request(e: impl std::fmt::Display) -> Answer {
    let body = wire::ErrorBody::new("bad-request", e.to_string());
    (400, json(&body))
}

fn refusal(e: &VcsError) -> Answer {
    let body = wire::ErrorBody::from(e);
    (wire::status_of(&body.error), json(&body))
}

fn json<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn answer<T: Serialize>(r: VResult<T>) -> Answer {
    match r {
        Ok(v) => (200, json(&v)),
        Err(e) => refusal(&e),
    }
}

fn parse<T: DeserializeOwned>(body: &[u8]) -> std::result::Result<T, Answer> {
    serde_json::from_slice(body).map_err(bad_request)
}

impl Daemon {
    fn note(&self, ws: &str) {
        self.seen.lock().unwrap().insert(ws.to_string());
    }

    /// Where `dest` may be written: the canonical path, when its parent exists
    /// and is inside something an operator listed (canonicalised on both sides,
    /// so `..` and a symlinked parent cannot walk out). `dest` itself need not
    /// exist.
    fn permits(&self, dest: &Path) -> Option<PathBuf> {
        let parent = dest.parent()?;
        let name = dest.file_name()?;
        let real_parent = parent.canonicalize().ok()?;
        let permitted = self
            .allowed
            .iter()
            .any(|a| a.canonicalize().map(|a| real_parent.starts_with(a)).unwrap_or(false));
        permitted.then(|| real_parent.join(name))
    }

    /// One WIT function, by name, over a JSON body.
    async fn dispatch(&self, func: &str, body: &[u8]) -> Answer {
        match self.dispatch_inner(func, body).await {
            Ok(a) | Err(a) => a,
        }
    }

    async fn dispatch_inner(&self, func: &str, body: &[u8]) -> std::result::Result<Answer, Answer> {
        Ok(match func {
            "apply-patch" => {
                let req: PatchRequest = parse(body)?;
                self.note(&req.workspace);
                answer(on!(self, |e| e.apply_patch(req).await))
            }
            "resolve-conflict" => {
                let req: ResolutionRequest = parse(body)?;
                self.note(&req.workspace);
                answer(on!(self, |e| e.resolve_conflict(req).await))
            }
            "revert-op" => {
                let r: wire::RevertOp = parse(body)?;
                self.note(&r.workspace);
                answer(on!(self, |e| e.revert_op(&r.workspace, r.op, r.by.clone()).await))
            }
            "query-symbol" => {
                let r: wire::QuerySymbol = parse(body)?;
                answer(on!(self, |e| e.query_symbol(&r.workspace, r.query.clone()).await))
            }
            "snapshot-export" => {
                let r: wire::Export = parse(body)?;
                answer(on!(self, |e| e.snapshot_export(&r.workspace, &r.component).await))
            }
            "list-conflicts" => {
                let r: wire::ListConflicts = parse(body)?;
                answer(on!(self, |e| e.list_conflicts(&r.workspace, r.state).await))
            }
            "oplog" => {
                let r: wire::Oplog = parse(body)?;
                answer(on!(self, |e| e.oplog(&r.workspace, r.after, r.limit).await))
            }
            "oplog-head" => {
                let r: wire::Workspace = parse(body)?;
                answer(on!(self, |e| e.oplog_head(&r.workspace).await))
            }
            "verify" => {
                let r: wire::Workspace = parse(body)?;
                answer(on!(self, |e| e.verify(&r.workspace).await))
            }
            "repair" => {
                let r: wire::Workspace = parse(body)?;
                self.note(&r.workspace);
                answer(on!(self, |e| e.repair(&r.workspace).await))
            }
            "ingest-file" => {
                let r: wire::IngestFile = parse(body)?;
                if let Err(e) = git::validate_path(&r.file.path) {
                    return Err(refusal(&e));
                }
                self.note(&r.workspace);
                let rep = on!(self, |e| extract::ingest_file(
                    e.as_ref(),
                    &r.workspace,
                    &r.component,
                    &r.file.path,
                    &r.file.content.0,
                    &r.by,
                    r.read_at,
                )
                .await);
                answer(rep.map(wire::IngestReport::from))
            }
            "ingest-tree" => {
                let r: wire::IngestTree = parse(body)?;
                for f in &r.files {
                    if let Err(e) = git::validate_path(&f.path) {
                        return Err(refusal(&e));
                    }
                }
                self.note(&r.workspace);
                let files: Vec<(String, Vec<u8>)> =
                    r.files.into_iter().map(|f| (f.path, f.content.0)).collect();
                let rep = on!(self, |e| extract::ingest_tree(
                    e.as_ref(),
                    &r.workspace,
                    &r.component,
                    &files,
                    &r.by,
                    r.read_at,
                    r.prune,
                )
                .await);
                answer(rep.map(|v| v.into_iter().map(wire::IngestReport::from).collect::<Vec<_>>()))
            }
            "materialize" => {
                let r: wire::Materialize = parse(body)?;
                self.materialize(r).await
            }
            "read-blob" => {
                let r: wire::ReadBlob = parse(body)?;
                if !holon_vcs::store::is_hash(&r.blob) {
                    return Err(refusal(&VcsError::Invalid(format!(
                        "{:?} is not a sha-256 hash",
                        r.blob
                    ))));
                }
                match on!(self, |e| e.blobs().get(&r.blob).await) {
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
    }

    async fn materialize(&self, r: wire::Materialize) -> Answer {
        let not_permitted = || {
            let body = wire::ErrorBody::new("not-permitted", r.dest.clone());
            (403, json(&body))
        };
        let Some(dest) = self.permits(Path::new(&r.dest)) else { return not_permitted() };
        if dest.exists() {
            let empty = std::fs::read_dir(&dest).map(|mut d| d.next().is_none()).unwrap_or(false);
            if !dest.is_dir() || !empty {
                return refusal(&VcsError::Invalid(format!(
                    "{} exists and is not an empty directory",
                    dest.display()
                )));
            }
        }
        let snap = match on!(self, |e| e.snapshot_export(&r.workspace, &r.component).await) {
            Ok(s) => s,
            Err(e) => return refusal(&e),
        };
        let mut files = 0u32;
        let mut bytes = 0u64;
        let io = |e: std::io::Error| {
            refusal(&VcsError::Storage(format!("writing {}: {e}", dest.display())))
        };
        if let Err(e) = std::fs::create_dir_all(&dest) {
            return io(e);
        }
        for entry in &snap.entries {
            if let Err(e) = git::validate_path(&entry.path) {
                return refusal(&e);
            }
            let content = match on!(self, |e| e.blobs().get(&entry.blob).await) {
                Ok(Some(b)) => b,
                Ok(None) => {
                    return refusal(&VcsError::Storage(format!("blob {} is missing", entry.blob)))
                }
                Err(e) => return refusal(&e),
            };
            let path = dest.join(&entry.path);
            if let Some(parent) = path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return io(e);
                }
            }
            if let Err(e) = std::fs::write(&path, &content) {
                return io(e);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = if entry.executable { 0o755 } else { 0o644 };
                if let Err(e) =
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                {
                    return io(e);
                }
            }
            files += 1;
            bytes += content.len() as u64;
        }
        let out =
            wire::Materialized { dir: dest.display().to_string(), snapshot: snap, files, bytes };
        (200, json(&out))
    }

    async fn healthy(&self) -> (bool, bool) {
        let kv = on!(self, |e| e.pointers().get("health/probe").await).is_ok();
        let graph = on!(self, |e| e.graph().symbol("health", "probe").await).is_ok();
        (kv, graph)
    }

    /// `repair` on every workspace the log knows, before listening.
    async fn startup_repair(&self) -> Result<()> {
        let workspaces = match &self.backend {
            Backend::Live(e) => holon_vcs::nats::workspaces(&e.log().inner)
                .await
                .context("listing the oplog bucket's workspaces")?,
            Backend::Mem(_) => Vec::new(),
        };
        for ws in workspaces {
            let rep: RepairReport = on!(self, |e| e.repair(&ws).await)
                .with_context(|| format!("repairing workspace {ws:?}"))?;
            if !rep.rolled_forward.is_empty()
                || !rep.aborted.is_empty()
                || !rep.remaining.is_empty()
            {
                println!(
                    "comp-vcs: repaired {ws:?}: rolled forward {:?}, aborted {:?}, {} fixed, {} remaining",
                    rep.rolled_forward,
                    rep.aborted,
                    rep.fixed.len(),
                    rep.remaining.len()
                );
            }
            self.startup.lock().unwrap().push(json!({
                "workspace": ws,
                "rolled-forward": rep.rolled_forward,
                "aborted": rep.aborted,
                "fixed": rep.fixed.len(),
                "remaining": rep.remaining.len(),
            }));
            self.note(&ws);
        }
        Ok(())
    }
}

type Shared = Arc<Daemon>;

fn respond((status, body): Answer) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (code, Json(body)).into_response()
}

async fn call(
    State(d): State<Shared>,
    UrlPath(func): UrlPath<String>,
    body: axum::body::Bytes,
) -> Response {
    respond(d.dispatch(&func, &body).await)
}

async fn health(State(d): State<Shared>) -> Json<Value> {
    let (kv, graph) = d.healthy().await;
    let backend = match d.backend {
        Backend::Live(_) => "nats+surrealdb",
        Backend::Mem(_) => "memory",
    };
    Json(json!({
        "ok": kv && graph,
        "backend": backend,
        "pointers": kv,
        "graph": graph,
        "lease-ms": d.lease_ms,
        "workspaces": d.seen.lock().unwrap().len(),
        "startup-repair": d.startup.lock().unwrap().clone(),
        "fault-injection": d.fuse.plan.map(|p| p.describe()),
    }))
}

fn app(d: Shared, token: Option<String>) -> Router {
    let api = Router::new()
        .route("/v1/{func}", post(call))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(axum::middleware::from_fn(comp_reconciler::daemon_auth::require_token))
        .layer(axum::Extension(Arc::new(token)))
        .with_state(d.clone());
    Router::new().route("/health", get(health)).with_state(d).merge(api)
}

fn fault_plan(args: &Args) -> Result<Option<Plan>> {
    let plan = match (&args.crash_after_step, &args.crash_before_step) {
        (Some(_), Some(_)) => bail!("--crash-after-step and --crash-before-step are exclusive"),
        (Some(s), None) => Some(Plan::parse(When::After, s)?),
        (None, Some(s)) => Some(Plan::parse(When::Before, s)?),
        (None, None) => None,
    };
    if plan.is_some() && !args.i_know_this_is_a_test {
        bail!(
            "--crash-*-step aborts this process in the middle of a write, on purpose. \
             It exists for crash-consistency tests; pass --i-know-this-is-a-test to confirm \
             that is what this is"
        );
    }
    Ok(plan)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let plan = fault_plan(&args)?;
    let token =
        comp_reconciler::daemon_auth::resolve_token(args.token.clone(), args.token_file.clone());
    comp_reconciler::daemon_auth::warn_if_unauthenticated("comp-vcs", &token);
    if let Some(p) = plan {
        eprintln!(
            "comp-vcs: TEST FAULT INJECTION ARMED ({}): this process will abort itself mid-write",
            p.describe()
        );
    }
    let lease_ms = (args.lease_secs.max(0.0) * 1000.0) as u64;
    let fuse = Arc::new(Fuse { plan, ..Default::default() });
    let backend = if args.memory {
        eprintln!("comp-vcs: --memory: nothing is durable");
        mem_backend(&fuse, lease_ms)
    } else {
        live_backend(&args, &fuse, lease_ms).await?
    };
    let d = Arc::new(Daemon {
        backend,
        allowed: args.allow_path.clone(),
        fuse: fuse.clone(),
        lease_ms,
        seen: Mutex::new(BTreeSet::new()),
        startup: Mutex::new(Vec::new()),
    });
    d.startup_repair().await?;
    // Counted from the first request, never from startup repair.
    fuse.armed.store(true, Ordering::SeqCst);
    if args.allow_path.is_empty() {
        eprintln!("comp-vcs: no --allow-path given, so every materialize will be refused");
    }
    let listener = tokio::net::TcpListener::bind(&args.addr)
        .await
        .with_context(|| format!("binding {}", args.addr))?;
    println!(
        "comp-vcs: listening on http://{} | {} | lease {} ms | {} allowed path(s)",
        args.addr,
        if args.memory {
            "memory".to_string()
        } else {
            format!("{} + {}", args.nats_url, args.surreal_url)
        },
        lease_ms,
        args.allow_path.len()
    );
    axum::serve(listener, app(d, token)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use holon_vcs::model::{Agent, Content, SymbolKind, Transformation};

    fn daemon(allowed: Vec<PathBuf>, trace: bool) -> Daemon {
        let fuse =
            Arc::new(Fuse { trace: trace.then(|| Mutex::new(Vec::new())), ..Default::default() });
        Daemon {
            backend: mem_backend(&fuse, 30_000),
            allowed,
            fuse,
            lease_ms: 30_000,
            seen: Mutex::new(BTreeSet::new()),
            startup: Mutex::new(Vec::new()),
        }
    }

    fn sym(name: &str) -> SymbolId {
        SymbolId::new("c", "src/lib.rs", name, SymbolKind::Function)
    }

    fn patch(
        name: &str,
        parent: Option<&str>,
        change: Transformation,
        read_at: Option<u64>,
    ) -> Value {
        let mut r = PatchRequest {
            workspace: "w".into(),
            symbol: sym(name),
            parent: parent.map(String::from),
            change,
            agent: Agent::named("t"),
            message: None,
            depends_on: vec![],
            implements: vec![],
            wit_binding: None,
            read_at: None,
            position: None,
        };
        r.read_at = read_at;
        json(&r)
    }

    async fn post(d: &Daemon, func: &str, body: Value) -> Answer {
        d.dispatch(func, body.to_string().as_bytes()).await
    }

    fn inline(s: &str) -> Content {
        Content::Inline(s.into())
    }

    /// The request mapping, route by route, through a conflict and its
    /// resolution: every status and body is the contract's.
    #[tokio::test]
    async fn routes_map_one_to_one_onto_the_contract() {
        let d = daemon(vec![], false);
        let (s, a) = post(
            &d,
            "apply-patch",
            patch("f", None, Transformation::Create(inline("fn f() {}\n")), None),
        )
        .await;
        assert_eq!(s, 200, "{a}");
        assert_eq!(a["outcome"], "applied");
        let base = a["patch"].as_str().unwrap().to_string();

        let (s, x) = post(
            &d,
            "apply-patch",
            patch("f", Some(&base), Transformation::Replace(inline("fn f() { 1 }\n")), None),
        )
        .await;
        assert_eq!((s, x["outcome"].as_str()), (200, Some("applied")));
        let (s, y) = post(
            &d,
            "apply-patch",
            patch("f", Some(&base), Transformation::Replace(inline("fn f() { 2 }\n")), None),
        )
        .await;
        assert_eq!((s, y["outcome"].as_str()), (200, Some("conflicted")));
        let cid = y["conflict"].as_str().unwrap().to_string();

        let (s, e) = post(&d, "snapshot-export", json!({"workspace": "w", "component": "c"})).await;
        assert_eq!(s, 409);
        assert_eq!(e["error"], "unresolved-conflict");
        assert_eq!(e["detail"], json!([cid]));

        let (s, list) =
            post(&d, "list-conflicts", json!({"workspace": "w", "state": "open"})).await;
        assert_eq!(s, 200);
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["left"]["content"], json!({"inline": "fn f() { 1 }\n"}));

        let res = json!({
            "workspace": "w", "conflict": cid,
            "resolution": {"replace": {"inline": "fn f() { 3 }\n"}},
            "agent": {"id": "resolver"}, "message": null
        });
        let (s, r) = post(&d, "resolve-conflict", res).await;
        assert_eq!(s, 200, "{r}");
        let (s, snap) =
            post(&d, "snapshot-export", json!({"workspace": "w", "component": "c"})).await;
        assert_eq!(s, 200);
        let blob = snap["entries"][0]["blob"].as_str().unwrap();
        let (s, b) = post(&d, "read-blob", json!({"blob": blob})).await;
        assert_eq!(s, 200);
        let got: wire::Bytes = serde_json::from_value(b).unwrap();
        assert_eq!(got.0, b"fn f() { 3 }\n");

        let (s, head) = post(&d, "oplog-head", json!({"workspace": "w"})).await;
        assert_eq!(s, 200);
        let (s, log) = post(&d, "oplog", json!({"workspace": "w", "limit": 100})).await;
        assert_eq!(s, 200);
        assert_eq!(log.as_array().unwrap().last().unwrap()["id"], head);

        let (s, q) =
            post(&d, "query-symbol", json!({"workspace": "w", "query": {"component": "c"}})).await;
        assert_eq!((s, q[0]["content"].clone()), (200, json!({"inline": "fn f() { 3 }\n"})));

        let (s, v) = post(&d, "verify", json!({"workspace": "w"})).await;
        assert_eq!((s, v["issues"].clone()), (200, json!([])));
        let (s, rep) = post(&d, "repair", json!({"workspace": "w"})).await;
        assert_eq!((s, rep["remaining"].clone()), (200, json!([])));

        let op = r["op"].as_u64().unwrap();
        let (s, rev) =
            post(&d, "revert-op", json!({"workspace": "w", "op": op, "by": {"id": "t"}})).await;
        assert_eq!(s, 200, "{rev}");
        assert_eq!(rev["kind"], json!({"revert": op}));
    }

    #[tokio::test]
    async fn refusals_carry_their_kind_status_and_payload() {
        let d = daemon(vec![], false);
        let (s, e) = post(&d, "apply-patch", json!({"nope": 1})).await;
        assert_eq!((s, e["error"].as_str()), (400, Some("bad-request")));
        let (s, e) = post(&d, "frobnicate", json!({})).await;
        assert_eq!((s, e["error"].as_str()), (404, Some("not-found")));
        // replace without a parent: invalid
        let (s, e) =
            post(&d, "apply-patch", patch("f", None, Transformation::Replace(inline("x")), None))
                .await;
        assert_eq!((s, e["error"].as_str()), (400, Some("invalid")), "{e}");
        // name-taken: rename g onto f
        post(
            &d,
            "apply-patch",
            patch("f", None, Transformation::Create(inline("fn f() {}\n")), None),
        )
        .await;
        let (_, g) = post(
            &d,
            "apply-patch",
            patch("g", None, Transformation::Create(inline("fn g() {}\n")), None),
        )
        .await;
        let (s, e) = post(
            &d,
            "apply-patch",
            patch("g", g["patch"].as_str(), Transformation::Rename("f".into()), None),
        )
        .await;
        assert_eq!((s, e["error"].as_str()), (409, Some("name-taken")), "{e}");
        let back: wire::ErrorBody = serde_json::from_value(e).unwrap();
        assert_eq!(back.into_error(), VcsError::NameTaken(sym("f")));
        let (s, e) =
            post(&d, "revert-op", json!({"workspace": "w", "op": 999, "by": {"id": "t"}})).await;
        assert_eq!((s, e["error"].as_str()), (404, Some("not-found")), "{e}");
        let (s, e) = post(&d, "read-blob", json!({"blob": "zz"})).await;
        assert_eq!((s, e["error"].as_str()), (400, Some("invalid")));
        let (s, e) = post(&d, "read-blob", json!({"blob": "0".repeat(64)})).await;
        assert_eq!((s, e["error"].as_str()), (404, Some("not-found")));
        let (s, e) = post(
            &d,
            "ingest-file",
            json!({
                "workspace": "w", "component": "c", "by": {"id": "t"},
                "file": {"path": "../escape.rs", "content": ""}
            }),
        )
        .await;
        assert_eq!((s, e["error"].as_str()), (400, Some("invalid")), "{e}");
    }

    #[tokio::test]
    async fn ingest_and_materialize_round_trip_a_file_byte_for_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let d = daemon(vec![root.clone()], false);
        let text = "//! doc\nuse std::fmt;\n\nfn a() {}\n\n/// b\nfn b() -> u8 { 1 } // why\n";
        let file = json!({"path": "src/lib.rs", "content": base64_of(text.as_bytes())});
        let (s, rep) = post(
            &d,
            "ingest-file",
            json!({"workspace": "w", "component": "c", "file": file, "by": {"id": "t"}}),
        )
        .await;
        assert_eq!(s, 200, "{rep}");
        assert!(
            rep["patches"].as_array().unwrap().iter().all(|p| p["outcome"]["ok"].is_object()),
            "{rep}"
        );

        let (s, e) = post(
            &d,
            "materialize",
            json!({"workspace": "w", "component": "c", "dest": "/etc/holon-vcs-test"}),
        )
        .await;
        assert_eq!((s, e["error"].as_str()), (403, Some("not-permitted")));
        let dest = root.join("out");
        let (s, m) =
            post(&d, "materialize", json!({"workspace": "w", "component": "c", "dest": dest}))
                .await;
        assert_eq!(s, 200, "{m}");
        assert_eq!(std::fs::read_to_string(dest.join("src/lib.rs")).unwrap(), text);
        assert_eq!(m["files"], 1);
        assert_eq!(m["bytes"], text.len());
        // Not into a directory that already has something in it.
        let (s, e) =
            post(&d, "materialize", json!({"workspace": "w", "component": "c", "dest": dest}))
                .await;
        assert_eq!((s, e["error"].as_str()), (400, Some("invalid")));
    }

    fn base64_of(b: &[u8]) -> Value {
        json(&wire::Bytes(b.to_vec()))
    }

    /// The step names the crash flags take are the write order the engine
    /// documents (`recovery`), as this daemon's wrapper classifies it.
    #[tokio::test]
    async fn the_fuse_sees_the_documented_write_order() {
        let d = daemon(vec![], true);
        let (s, a) = post(
            &d,
            "apply-patch",
            patch("f", None, Transformation::Create(inline("fn f() {}\n")), None),
        )
        .await;
        assert_eq!(s, 200, "{a}");
        let trace = d.fuse.trace.as_ref().unwrap().lock().unwrap().clone();
        let first = |st: Step| {
            trace.iter().position(|s| *s == st).unwrap_or_else(|| panic!("no {st:?} in {trace:?}"))
        };
        assert!(first(Step::Blob) < first(Step::Intent));
        assert!(first(Step::Intent) < first(Step::Graph));
        assert!(first(Step::Graph) < first(Step::Claim));
        assert!(first(Step::Claim) < first(Step::Tip));
        assert!(first(Step::Tip) < first(Step::Finish));
        assert!(first(Step::Finish) < first(Step::Commit));
        assert_eq!(trace.iter().filter(|s| **s == Step::Tip).count(), 1, "{trace:?}");
        assert_eq!(*trace.last().unwrap(), Step::Commit, "{trace:?}");
    }

    #[test]
    fn crash_flags_parse_and_refuse_without_the_confirmation() {
        assert_eq!(
            Plan::parse(When::After, "tip").unwrap(),
            Plan { when: When::After, step: Step::Tip, n: 1 }
        );
        assert_eq!(Plan::parse(When::Before, "graph:3").unwrap().n, 3);
        assert!(Plan::parse(When::After, "tip:0").is_err());
        assert!(Plan::parse(When::After, "nope").is_err());
        let args = |extra: &[&str]| {
            let mut v = vec!["comp-vcs", "--memory"];
            v.extend_from_slice(extra);
            Args::parse_from(v)
        };
        assert!(fault_plan(&args(&["--crash-after-step", "tip"])).is_err());
        assert!(fault_plan(&args(&["--crash-after-step", "tip", "--i-know-this-is-a-test"]))
            .unwrap()
            .is_some());
        assert!(fault_plan(&args(&[
            "--crash-after-step",
            "tip",
            "--crash-before-step",
            "tip",
            "--i-know-this-is-a-test"
        ]))
        .is_err());
        assert!(fault_plan(&args(&[])).unwrap().is_none());
    }

    #[test]
    fn materialize_is_bounded_by_the_allow_list() {
        let tmp = tempfile::tempdir().unwrap();
        let inside = tmp.path().canonicalize().unwrap().join("allowed");
        std::fs::create_dir_all(&inside).unwrap();
        let d = daemon(vec![inside.clone()], false);
        assert_eq!(d.permits(&inside.join("new")), Some(inside.join("new")));
        assert!(d.permits(&inside.join("../sibling")).is_none());
        assert!(d.permits(&inside.join("missing/deeper")).is_none(), "the parent must exist");
        assert!(daemon(vec![], false).permits(&inside.join("new")).is_none(), "unscoped: nothing");
    }
}
