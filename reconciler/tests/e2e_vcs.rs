//! The code store end to end (ADR-0099): every request goes
//!
//!   this test → comp-host (vcs-gateway ⊕ vcs-store) → comp-vcs → NATS JetStream + SurrealDB
//!
//! over real HTTP, through the component model, against a real NATS and a real
//! SurrealDB — never the library. `bash e2e/vcs.sh` brings those two up (a
//! private `nats-server -js` on a free port, the compose SurrealDB), builds what
//! this needs, runs this file and tears everything down.
//!
//! Without `VCS_E2E_NATS_URL` and `VCS_E2E_SURREAL_URL` every test here prints
//! `SKIPPED` and returns: CI compiles this file (`--no-run`) and does not run it,
//! because a skip that returns `ok` is a green tick for a test that did not run.
//! With them set, a missing `comp-host` or unbuilt component is a failure, not a
//! skip — the script promised them.
//!
//! Each test owns its buckets (`--bucket-prefix`), its SurrealDB database, its
//! `comp-vcs` port and its `comp-host`, so they run in parallel and a restart's
//! startup repair only ever sees its own test's workspaces.
//!
//! The scenarios:
//!   A  two agents, two functions of one file, at once: both land, one
//!      `commuted` naming the other, the export has both
//!   B  two agents, one function: one lands, one conflict with both sides;
//!      export and materialize refused; resolve; the materialized directory's
//!      `git write-tree` is the snapshot's `git-tree`
//!   C  a crash at each step of the write order (a real `abort()` of comp-vcs),
//!      then a normal restart: verify clean, the op landed exactly when its
//!      commit point had happened, a retry lands it exactly once
//!   D  `commuted` with `read-at` is exact; without it, it over-reports
//!   E  eight renames to one name at once: one wins, seven `name-taken`
//!   F  record-store's real sources ingested, exported and materialized byte for
//!      byte (git tree ids checked against `git`), then two agents ingesting
//!      edited copies: different functions, the same function, two inserts at
//!      one spot
//!   G  a function moved by ingest, and back: exact both ways
//!   H  a crashed writer's intent blocks export until the lease passes

mod gatelib;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Barrier;
use std::time::{Duration, Instant};

use holon_vcs::model::{
    Agent, CommitResult, Conflict, ConsistencyReport, Content, OpEntry, OpKind, PatchOutcome,
    PatchRequest, Snapshot, SymbolId, SymbolKind, SymbolQuery, SymbolView, Transformation,
};
use holon_vcs::wire;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

const COMPONENT: &str = "shop";

// ---- the stack ------------------------------------------------------------------

struct Services {
    nats: String,
    surreal: String,
    user: String,
    pass: String,
}

fn services(test: &str) -> Option<Services> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    match (get("VCS_E2E_NATS_URL"), get("VCS_E2E_SURREAL_URL")) {
        (Some(nats), Some(surreal)) => Some(Services {
            nats,
            surreal,
            user: get("VCS_E2E_SURREAL_USER").unwrap_or_else(|| "root".into()),
            pass: get("VCS_E2E_SURREAL_PASS").unwrap_or_else(|| "root".into()),
        }),
        _ => {
            eprintln!(
                "SKIPPED e2e_vcs::{test}: set VCS_E2E_NATS_URL and VCS_E2E_SURREAL_URL \
                 (bash e2e/vcs.sh does) — this needs a real NATS JetStream and SurrealDB, \
                 and it did NOT run"
            );
            None
        }
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A refusal, as the gateway sent it.
#[derive(Debug)]
struct Refusal {
    status: u16,
    body: wire::ErrorBody,
}

impl Refusal {
    fn is(&self, error: &str) -> bool {
        self.body.error == error
    }
}

struct Stack {
    name: String,
    svc: Services,
    port: u16,
    prefix: String,
    db: String,
    /// `--allow-path` for materialize.
    allow: tempfile::TempDir,
    logs: tempfile::TempDir,
    vcs: Option<Child>,
    starts: u32,
    http: reqwest::blocking::Client,
    gate: Option<gatelib::Gate>,
}

impl Drop for Stack {
    fn drop(&mut self) {
        if let Some(mut c) = self.vcs.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if std::thread::panicking() {
            for i in 1..=self.starts {
                let p = self.logs.path().join(format!("vcs-{i}.log"));
                let text = std::fs::read_to_string(&p).unwrap_or_default();
                let tail: Vec<&str> = text.lines().rev().take(25).collect();
                eprintln!(
                    "--- [{}] comp-vcs start #{i} ({}), last lines ---\n{}",
                    self.name,
                    p.display(),
                    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
                );
            }
        }
    }
}

impl Stack {
    fn up(test: &str) -> Option<Stack> {
        let svc = services(test)?;
        let tag = format!("{test}-{}-{}", std::process::id(), nanos() % 1_000_000_000);
        let port = free_port();
        let mut s = Stack {
            name: test.to_string(),
            svc,
            port,
            prefix: format!("e2e-{tag}"),
            db: format!("e2e_{}", tag.replace('-', "_")),
            allow: tempfile::tempdir().unwrap(),
            logs: tempfile::tempdir().unwrap(),
            vcs: None,
            starts: 0,
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(600))
                .build()
                .unwrap(),
            gate: None,
        };
        s.start_vcs(&[]);
        let url = format!("vcs-url=http://127.0.0.1:{port}");
        let egress = format!("127.0.0.1:{port}");
        let gate =
            gatelib::Gate::compose_and_start_with_egress("vcs", "vcs-gateway", &[&url], &[&egress]);
        match gate {
            Some(g) => s.gate = Some(g),
            None => panic!(
                "[{test}] VCS_E2E_* is set, so this run was asked for — but comp-host or the \
                 vcs-gateway/vcs-store components are not built (bash e2e/vcs.sh builds them)"
            ),
        }
        Some(s)
    }

    fn allow_root(&self) -> PathBuf {
        self.allow.path().canonicalize().unwrap()
    }

    /// Start comp-vcs on this stack's port, and wait for `/health`.
    fn start_vcs(&mut self, extra: &[&str]) {
        assert!(self.vcs.is_none(), "comp-vcs is already running");
        self.starts += 1;
        let log = std::fs::File::create(self.logs.path().join(format!("vcs-{}.log", self.starts)))
            .unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_comp-vcs"));
        cmd.args(["--addr", &format!("127.0.0.1:{}", self.port)])
            .args(["--nats-url", &self.svc.nats])
            .args(["--bucket-prefix", &self.prefix])
            .args(["--surreal-url", &self.svc.surreal])
            .args(["--surreal-ns", "holon_vcs_e2e", "--surreal-db", &self.db])
            .args(["--surreal-user", &self.svc.user, "--surreal-pass", &self.svc.pass])
            .arg("--allow-path")
            .arg(self.allow_root())
            .args(extra)
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        let mut child = cmd.spawn().expect("spawn comp-vcs");
        let t0 = Instant::now();
        loop {
            if let Ok(r) = self.http.get(format!("http://127.0.0.1:{}/health", self.port)).send() {
                let v: Value = r.json().unwrap_or(Value::Null);
                if v["ok"] == true {
                    break;
                }
            }
            if let Ok(Some(st)) = child.try_wait() {
                self.vcs = None;
                panic!("[{}] comp-vcs exited during startup: {st}", self.name);
            }
            assert!(
                t0.elapsed() < Duration::from_secs(60),
                "[{}] comp-vcs never became healthy",
                self.name
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        self.vcs = Some(child);
    }

    fn stop_vcs(&mut self) {
        if let Some(mut c) = self.vcs.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    fn restart_vcs(&mut self, extra: &[&str]) {
        self.stop_vcs();
        self.start_vcs(extra);
    }

    /// Wait for a crash-armed comp-vcs to have aborted itself.
    fn wait_crashed(&mut self) -> ExitStatus {
        let mut c = self.vcs.take().expect("a running comp-vcs");
        let t0 = Instant::now();
        loop {
            if let Some(st) = c.try_wait().unwrap() {
                return st;
            }
            if t0.elapsed() > Duration::from_secs(30) {
                let _ = c.kill();
                panic!("[{}] comp-vcs was armed to crash and is still running", self.name);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn vcs_health(&self) -> Value {
        self.http
            .get(format!("http://127.0.0.1:{}/health", self.port))
            .send()
            .unwrap()
            .json()
            .unwrap()
    }

    fn ws(&self, tag: &str) -> String {
        format!("{}/{tag}-{}", self.name, nanos())
    }

    /// One route of the gateway.
    fn raw(&self, func: &str, body: &Value) -> Result<Value, Refusal> {
        let (status, text) =
            self.gate.as_ref().unwrap().post(&wire::route(func), None, body.clone());
        if status == 200 {
            return Ok(serde_json::from_str(&text).unwrap_or_else(|e| {
                panic!("{func}: 200 with a body that is not JSON ({e}): {text}")
            }));
        }
        let body: wire::ErrorBody = serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("{func}: HTTP {status} with a body that is not an error ({e}): {text}")
        });
        Err(Refusal { status, body })
    }

    fn call<T: DeserializeOwned>(&self, func: &str, body: Value) -> Result<T, Refusal> {
        self.raw(func, &body).map(|v| {
            serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{func}: {e}: {v}"))
        })
    }

    fn ok<T: DeserializeOwned>(&self, func: &str, body: Value) -> T {
        self.call(func, body).unwrap_or_else(|r| panic!("{func} refused: {r:?}"))
    }

    // ---- the contract, typed -----------------------------------------------------

    fn apply(&self, r: &PatchRequest) -> Result<CommitResult, Refusal> {
        self.call("apply-patch", serde_json::to_value(r).unwrap())
    }

    fn head(&self, ws: &str) -> u64 {
        self.ok("oplog-head", json!({ "workspace": ws }))
    }

    /// The symbol's view, or nothing when it is not live (`symbol-not-found`).
    fn query(&self, ws: &str, id: &SymbolId) -> Vec<SymbolView> {
        match self.call(
            "query-symbol",
            json!({ "workspace": ws, "query": SymbolQuery::Symbol(id.clone()) }),
        ) {
            Ok(v) => v,
            Err(r) if r.is("symbol-not-found") && r.status == 404 => Vec::new(),
            Err(r) => panic!("query-symbol {id}: {r:?}"),
        }
    }

    fn view(&self, ws: &str, id: &SymbolId) -> SymbolView {
        let v = self.query(ws, id);
        assert_eq!(v.len(), 1, "{id} should be live: {v:?}");
        v.into_iter().next().unwrap()
    }

    fn content(&self, ws: &str, id: &SymbolId) -> String {
        match self.view(ws, id).content {
            Content::Inline(s) => s,
            Content::Blob(h) => String::from_utf8(self.blob(&h)).unwrap(),
        }
    }

    fn blob(&self, h: &str) -> Vec<u8> {
        let b: wire::Bytes = self.ok("read-blob", json!({ "blob": h }));
        b.0
    }

    fn export(&self, ws: &str) -> Result<Snapshot, Refusal> {
        self.call("snapshot-export", json!({ "workspace": ws, "component": COMPONENT }))
    }

    fn files(&self, snap: &Snapshot) -> BTreeMap<String, Vec<u8>> {
        snap.entries.iter().map(|e| (e.path.clone(), self.blob(&e.blob))).collect()
    }

    fn file(&self, ws: &str, path: &str) -> String {
        let snap = self.export(ws).unwrap_or_else(|r| panic!("export refused: {r:?}"));
        let files = self.files(&snap);
        String::from_utf8(
            files.get(path).unwrap_or_else(|| panic!("no {path} in {:?}", files.keys())).clone(),
        )
        .unwrap()
    }

    fn verify(&self, ws: &str) -> ConsistencyReport {
        self.ok("verify", json!({ "workspace": ws }))
    }

    fn conflicts(&self, ws: &str) -> Vec<Conflict> {
        self.ok("list-conflicts", json!({ "workspace": ws, "state": "open" }))
    }

    fn oplog(&self, ws: &str) -> Vec<OpEntry> {
        self.ok("oplog", json!({ "workspace": ws, "limit": 10_000 }))
    }

    fn materialize(&self, ws: &str, dest: &Path) -> Result<wire::Materialized, Refusal> {
        self.call("materialize", json!({ "workspace": ws, "component": COMPONENT, "dest": dest }))
    }

    fn ingest(
        &self,
        ws: &str,
        path: &str,
        text: &str,
        by: &str,
        read_at: Option<u64>,
    ) -> wire::IngestReport {
        let file =
            wire::SourceFile { path: path.into(), content: wire::Bytes(text.as_bytes().to_vec()) };
        self.ok(
            "ingest-file",
            json!({ "workspace": ws, "component": COMPONENT, "file": file, "by": agent(by), "read-at": read_at }),
        )
    }

    fn ingest_tree(
        &self,
        ws: &str,
        files: &[(String, Vec<u8>)],
        read_at: Option<u64>,
        prune: bool,
    ) -> Vec<wire::IngestReport> {
        let files: Vec<wire::SourceFile> = files
            .iter()
            .map(|(p, b)| wire::SourceFile { path: p.clone(), content: wire::Bytes(b.clone()) })
            .collect();
        self.ok(
            "ingest-tree",
            json!({ "workspace": ws, "component": COMPONENT, "files": files, "by": agent("importer"),
                    "read-at": read_at, "prune": prune }),
        )
    }
}

fn agent(id: &str) -> Agent {
    Agent { id: id.into(), goal: Some("e2e".into()), model: None }
}

fn sym(path: &str, name: &str) -> SymbolId {
    SymbolId::new(COMPONENT, path, name, SymbolKind::Function)
}

fn req(
    ws: &str,
    id: &SymbolId,
    parent: Option<&str>,
    change: Transformation,
    by: &str,
    read_at: Option<u64>,
) -> PatchRequest {
    PatchRequest {
        workspace: ws.into(),
        symbol: id.clone(),
        parent: parent.map(String::from),
        change,
        agent: agent(by),
        message: Some(format!("{by}'s edit")),
        depends_on: vec![],
        implements: vec![],
        wit_binding: None,
        read_at,
        position: None,
    }
}

fn inline(s: &str) -> Content {
    Content::Inline(s.into())
}

fn landed(r: &CommitResult) -> bool {
    matches!(r.outcome, PatchOutcome::Applied | PatchOutcome::Commuted)
}

fn assert_clean(v: &ConsistencyReport) {
    assert!(v.issues.is_empty(), "verify found issues: {:?}", v.issues);
    assert!(v.in_flight.is_empty(), "ops still in flight: {:?}", v.in_flight);
}

fn assert_ingested(r: &wire::IngestReport) {
    for p in &r.patches {
        match &p.outcome {
            wire::Outcome::Ok(c) if c.outcome != PatchOutcome::Conflicted => {}
            other => panic!("{}: {} {:?} did not land: {other:?}", r.path, p.symbol, p.edit),
        }
    }
}

/// `git write-tree` of a directory, in a throwaway repository.
fn git_tree_of(dir: &Path) -> String {
    let git_dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "core.autocrlf=false", "-c", "core.fileMode=true"])
            .args(args)
            .env("GIT_DIR", git_dir.path())
            .env("GIT_WORK_TREE", dir)
            .stderr(Stdio::inherit())
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["add", "-A", "-f"]);
    git(&["write-tree"])
}

fn write_tree(dir: &Path, files: &[(String, Vec<u8>)]) {
    for (p, b) in files {
        let path = dir.join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b).unwrap();
    }
}

fn read_dir_files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for (p, b) in holon_vcs::extract::read_tree(dir, |_| true).unwrap() {
        out.insert(p, b);
    }
    out
}

/// Run `f(i)` on `n` threads released at once; results in thread order.
fn at_once<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let barrier = Barrier::new(n);
    std::thread::scope(|s| {
        let hs: Vec<_> = (0..n)
            .map(|i| {
                let (f, b) = (&f, &barrier);
                s.spawn(move || {
                    b.wait();
                    f(i)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

fn timed(test: &str, t0: Instant) {
    eprintln!("e2e_vcs::{test}: {:.2}s", t0.elapsed().as_secs_f64());
}

// ---- fixtures ---------------------------------------------------------------------

const ORDERS: &str = "src/orders.rs";

/// Two functions next to each other: git's adjacency conflict, and not ours.
const ORDERS_RS: &str = "//! Orders.\n\npub struct Order {\n    pub lines: Vec<(u32, u32)>,\n}\n\n/// The order's total, in cents.\npub fn compute_total(o: &Order) -> u32 {\n    o.lines.iter().map(|(q, p)| q * p).sum()\n}\n\n/// An order must have a line.\npub fn validate_order(o: &Order) -> Result<(), String> {\n    if o.lines.is_empty() {\n        return Err(\"empty\".into());\n    }\n    Ok(())\n}\n";

fn with_line(text: &str, after: &str, line: &str) -> String {
    assert!(text.contains(after), "{after:?} not in the text");
    text.replacen(after, &format!("{after}{line}\n"), 1)
}

const TOTAL_OPEN: &str = "pub fn compute_total(o: &Order) -> u32 {\n";
const VALIDATE_OPEN: &str = "pub fn validate_order(o: &Order) -> Result<(), String> {\n";

fn setup_orders(s: &Stack, ws: &str) -> (u64, SymbolView, SymbolView) {
    let r = s.ingest(ws, ORDERS, ORDERS_RS, "setup", None);
    assert_ingested(&r);
    let h = s.head(ws);
    (h, s.view(ws, &sym(ORDERS, "compute_total")), s.view(ws, &sym(ORDERS, "validate_order")))
}

fn text_of(v: &SymbolView) -> String {
    match &v.content {
        Content::Inline(s) => s.clone(),
        Content::Blob(_) => panic!("a fixture symbol should be inline"),
    }
}

// ---- A ------------------------------------------------------------------------------

#[test]
fn a_two_agents_two_functions_both_land_one_commutes() {
    let t0 = Instant::now();
    let Some(s) = Stack::up("a") else { return };
    let ws = s.ws("a");
    let (h, total, validate) = setup_orders(&s, &ws);
    let edits = [
        (
            total.clone(),
            with_line(&text_of(&total), TOTAL_OPEN, "    // agent A: totals are cents"),
        ),
        (
            validate.clone(),
            with_line(&text_of(&validate), VALIDATE_OPEN, "    // agent B: validated first"),
        ),
    ];
    let results = at_once(2, |i| {
        let (v, new) = &edits[i];
        let r = req(
            &ws,
            &v.id,
            Some(&v.tip),
            Transformation::Replace(inline(new)),
            ["a", "b"][i],
            Some(h),
        );
        s.apply(&r).unwrap_or_else(|e| panic!("agent {i}: {e:?}"))
    });
    for r in &results {
        assert!(landed(r), "both edits land: {results:?}");
    }
    // The one that landed second had not seen the other: it commuted past it,
    // and says which patch.
    let commuted: Vec<(usize, &CommitResult)> =
        results.iter().enumerate().filter(|(_, r)| r.outcome == PatchOutcome::Commuted).collect();
    assert!(!commuted.is_empty(), "at least one edit commuted: {results:?}");
    for (i, r) in &commuted {
        assert!(
            r.commuted_with.contains(&results[1 - i].patch),
            "{i} commuted past the other: {results:?}"
        );
    }
    let want = with_line(
        &with_line(ORDERS_RS, TOTAL_OPEN, "    // agent A: totals are cents"),
        VALIDATE_OPEN,
        "    // agent B: validated first",
    );
    assert_eq!(s.file(&ws, ORDERS), want);
    assert_clean(&s.verify(&ws));
    timed("a", t0);
}

// ---- B ------------------------------------------------------------------------------

#[test]
fn b_same_function_conflicts_export_waits_for_the_resolution() {
    let t0 = Instant::now();
    let Some(s) = Stack::up("b") else { return };
    let ws = s.ws("b");
    let (h, total, _) = setup_orders(&s, &ws);
    let sides = [
        with_line(&text_of(&total), TOTAL_OPEN, "    // agent A: prices include tax"),
        with_line(&text_of(&total), TOTAL_OPEN, "    // agent B: prices exclude tax"),
    ];
    let results = at_once(2, |i| {
        let r = req(
            &ws,
            &total.id,
            Some(&total.tip),
            Transformation::Replace(inline(&sides[i])),
            ["a", "b"][i],
            Some(h),
        );
        s.apply(&r).unwrap_or_else(|e| panic!("agent {i}: {e:?}"))
    });
    let won: Vec<usize> = (0..2).filter(|&i| landed(&results[i])).collect();
    let lost: Vec<usize> =
        (0..2).filter(|&i| results[i].outcome == PatchOutcome::Conflicted).collect();
    assert_eq!((won.len(), lost.len()), (1, 1), "one lands, one conflicts: {results:?}");
    let (w, l) = (won[0], lost[0]);
    let cid = results[l].conflict.clone().expect("a conflicted result names its conflict");

    let open = s.conflicts(&ws);
    assert_eq!(open.len(), 1, "{open:?}");
    let c = &open[0];
    assert_eq!(c.id, cid);
    assert_eq!(c.base.as_deref(), Some(total.tip.as_str()));
    assert_eq!(c.left.content, Some(inline(&sides[w])), "left is what landed, verbatim");
    assert_eq!(c.right.content, Some(inline(&sides[l])), "right is the other side, verbatim");

    let refused = s.export(&ws).unwrap_err();
    assert!(refused.is("unresolved-conflict") && refused.status == 409, "{refused:?}");
    assert_eq!(refused.body.detail, json!([cid]));
    let dest = s.allow_root().join("b-refused");
    assert!(s.materialize(&ws, &dest).unwrap_err().is("unresolved-conflict"));

    let merged = with_line(
        &sides[w],
        TOTAL_OPEN,
        if w == 0 {
            "    // agent B: prices exclude tax"
        } else {
            "    // agent A: prices include tax"
        },
    );
    let res: CommitResult = s.ok(
        "resolve-conflict",
        json!({ "workspace": ws, "conflict": cid, "resolution": Transformation::Replace(inline(&merged)),
                "agent": agent("resolver"), "message": "both notes" }),
    );
    assert!(landed(&res), "{res:?}");
    assert!(s.conflicts(&ws).is_empty());

    let want = ORDERS_RS.replacen(&text_of(&total), &merged, 1);
    let snap = s.export(&ws).unwrap();
    assert_eq!(String::from_utf8(s.files(&snap)[ORDERS].clone()).unwrap(), want);

    let dest = s.allow_root().join("b-out");
    let m = s.materialize(&ws, &dest).unwrap();
    assert_eq!(m.files, 1);
    assert_eq!(std::fs::read_to_string(dest.join(ORDERS)).unwrap(), want);
    assert_eq!(
        Some(git_tree_of(&dest)),
        snap.git_tree,
        "the materialized tree is the snapshot's git tree"
    );
    assert_eq!(m.snapshot.git_tree, snap.git_tree);
    // Outside --allow-path: refused, and nothing written.
    let outside = tempfile::tempdir().unwrap();
    let r = s.materialize(&ws, &outside.path().join("x")).unwrap_err();
    assert!(r.is("invalid") && r.body.message.contains("not-permitted"), "{r:?}");
    assert!(!outside.path().join("x").exists());
    assert_clean(&s.verify(&ws));
    timed("b", t0);
}

// ---- C ------------------------------------------------------------------------------

/// What the crashed op does.
#[derive(Clone, Copy, Debug)]
enum Op {
    Replace,
    Create,
    Rename,
}

#[test]
fn c_crash_at_every_write_step_then_restart_repairs() {
    let t0 = Instant::now();
    let Some(mut s) = Stack::up("c") else { return };
    // (flag, step, op, whether the op had landed: its commit point — the tip
    // CAS — had happened)
    let cases: &[(&str, &str, Op, bool)] = &[
        ("--crash-after-step", "blob", Op::Replace, false),
        ("--crash-after-step", "intent", Op::Replace, false),
        ("--crash-after-step", "graph", Op::Replace, false),
        ("--crash-before-step", "tip", Op::Replace, false),
        ("--crash-after-step", "tip", Op::Replace, true),
        ("--crash-after-step", "finish", Op::Replace, true),
        ("--crash-after-step", "commit", Op::Replace, true),
        ("--crash-after-step", "claim", Op::Create, false),
        ("--crash-after-step", "tip", Op::Create, true),
        ("--crash-before-step", "release", Op::Rename, true),
    ];
    let lease = ["--lease-secs", "1"];
    for &(flag, step, op, want_landed) in cases {
        let case = format!("{flag} {step} ({op:?})");
        let ws = s.ws(&format!("c-{step}"));
        s.restart_vcs(&lease);
        let f = sym("src/lib.rs", "f");
        let g = sym("src/lib.rs", "g");
        let h = sym("src/lib.rs", "h");
        let pf = s
            .apply(&req(
                &ws,
                &f,
                None,
                Transformation::Create(inline("fn f() {}\n")),
                "setup",
                None,
            ))
            .unwrap();
        let pg = s
            .apply(&req(
                &ws,
                &g,
                None,
                Transformation::Create(inline("fn g() {}\n")),
                "setup",
                None,
            ))
            .unwrap();
        let setup_ops = s.oplog(&ws).len();
        let the_op = match op {
            Op::Replace => req(
                &ws,
                &f,
                Some(&pf.patch),
                Transformation::Replace(inline("fn f() { 1 }\n")),
                "x",
                None,
            ),
            Op::Create => req(
                &ws,
                &sym("src/lib.rs", "k"),
                None,
                Transformation::Create(inline("fn k() {}\n")),
                "x",
                None,
            ),
            Op::Rename => {
                req(&ws, &g, Some(&pg.patch), Transformation::Rename("h".into()), "x", None)
            }
        };
        let has_landed = |s: &Stack| match op {
            Op::Replace => s.content(&ws, &f) == "fn f() { 1 }\n",
            Op::Create => !s.query(&ws, &sym("src/lib.rs", "k")).is_empty(),
            Op::Rename => !s.query(&ws, &h).is_empty(),
        };

        s.restart_vcs(&[lease[0], lease[1], flag, step, "--i-know-this-is-a-test"]);
        assert_eq!(
            s.vcs_health()["fault-injection"].as_str().map(|p| p
                .split(':')
                .nth(1)
                .unwrap()
                .to_string()),
            Some(step.to_string())
        );
        let crashed_at = Instant::now();
        let r = s.apply(&the_op).expect_err(&format!("{case}: the daemon died mid-request"));
        assert!(r.is("storage-error") && r.status == 503, "{case}: {r:?}");
        let st = s.wait_crashed();
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(st.signal(), Some(6), "{case}: comp-vcs aborted (SIGABRT), got {st}");
        }
        // Past the lease, so startup repair may fence what never landed.
        std::thread::sleep(Duration::from_millis(1200).saturating_sub(crashed_at.elapsed()));
        s.start_vcs(&lease);

        let startup = s.vcs_health()["startup-repair"].clone();
        let mine =
            startup.as_array().unwrap().iter().find(|e| e["workspace"] == json!(ws)).cloned();
        assert_clean(&s.verify(&ws));
        assert_eq!(has_landed(&s), want_landed, "{case}: landed? startup repair said {mine:?}");
        if step != "blob" {
            let mine = mine
                .unwrap_or_else(|| panic!("{case}: startup repair did not see {ws}: {startup}"));
            let key = if want_landed { "rolled-forward" } else { "aborted" };
            // After the commit write the op was already committed: nothing to roll forward.
            if step != "commit" {
                assert_eq!(mine[key].as_array().map(|a| a.len()), Some(1), "{case}: {mine}");
            }
        }

        // The retry lands it exactly once.
        let again = s.apply(&the_op).unwrap_or_else(|e| panic!("{case}: retry refused: {e:?}"));
        // (Not landed: `applied` or `commuted` — the op carries no read-at, so
        // g's creation, after f's, counts as commuted past.)
        if want_landed {
            assert_eq!(again.outcome, PatchOutcome::Duplicate, "{case}: {again:?}");
        } else {
            assert!(landed(&again), "{case}: {again:?}");
        }
        assert!(has_landed(&s), "{case}: the retry landed it");
        let log = s.oplog(&ws);
        let applies = log.iter().filter(|e| e.kind == OpKind::Apply(again.patch.clone())).count();
        assert_eq!(applies, 1, "{case}: exactly one committed op carries the patch: {log:?}");
        assert_eq!(log.len(), setup_ops + 1, "{case}: one op beyond the setup");
        s.export(&ws).unwrap_or_else(|e| panic!("{case}: export after repair: {e:?}"));
        assert_clean(&s.verify(&ws));
        eprintln!("e2e_vcs::c {case}: landed={want_landed} ok");
    }
    timed("c", t0);
}

// ---- D ------------------------------------------------------------------------------

#[test]
fn d_commuted_is_exact_with_read_at_and_over_reports_without() {
    let t0 = Instant::now();
    let Some(s) = Stack::up("d") else { return };
    // f and g exist; X edits g; then Y edits f three ways.
    // Returns Y's result, X's patch and g's creation.
    let run =
        |tag: &str, read: &dyn Fn(u64, u64) -> Option<u64>| -> (CommitResult, String, String) {
            let ws = s.ws(tag);
            let (f, g) = (sym("src/lib.rs", "f"), sym("src/lib.rs", "g"));
            let pf = s
                .apply(&req(
                    &ws,
                    &f,
                    None,
                    Transformation::Create(inline("fn f() {}\n")),
                    "setup",
                    None,
                ))
                .unwrap();
            let pg = s
                .apply(&req(
                    &ws,
                    &g,
                    None,
                    Transformation::Create(inline("fn g() {}\n")),
                    "setup",
                    None,
                ))
                .unwrap();
            let before_x = s.head(&ws);
            let x = s
                .apply(&req(
                    &ws,
                    &g,
                    Some(&pg.patch),
                    Transformation::Replace(inline("fn g() { 1 }\n")),
                    "x",
                    Some(before_x),
                ))
                .unwrap();
            let after_x = s.head(&ws);
            let y = s
                .apply(&req(
                    &ws,
                    &f,
                    Some(&pf.patch),
                    Transformation::Replace(inline("fn f() { 1 }\n")),
                    "y",
                    read(before_x, after_x),
                ))
                .unwrap();
            (y, x.patch, pg.patch)
        };
    // Y read after X's edit: it saw it, nothing commuted.
    let (y, _, _) = run("d-seen", &|_, after| Some(after));
    assert_eq!((y.outcome, y.commuted_with.len()), (PatchOutcome::Applied, 0), "{y:?}");
    // Y read before X's edit: it commuted past exactly X.
    let (y, x, _) = run("d-unseen", &|before, _| Some(before));
    assert_eq!((y.outcome, y.commuted_with.clone()), (PatchOutcome::Commuted, vec![x]), "{y:?}");
    // No read-at: measured from the op that landed f's parent (f's creation), so
    // everything after it counts — X's edit AND g's creation, both of which Y had
    // seen. The documented over-report (ADR-0099, *`commuted`, exactly*).
    let (y, x, g_created) = run("d-unknown", &|_, _| None);
    assert_eq!(
        (y.outcome, y.commuted_with.clone()),
        (PatchOutcome::Commuted, vec![g_created, x]),
        "{y:?}"
    );
    timed("d", t0);
}

// ---- E ------------------------------------------------------------------------------

#[test]
fn e_eight_renames_to_one_name_one_wins() {
    let t0 = Instant::now();
    let Some(s) = Stack::up("e") else { return };
    let ws = s.ws("e");
    const N: usize = 8;
    let tips: Vec<(SymbolId, String)> = (0..N)
        .map(|i| {
            let id = sym("src/lib.rs", &format!("s{i}"));
            let r = s
                .apply(&req(
                    &ws,
                    &id,
                    None,
                    Transformation::Create(inline(&format!("fn s{i}() {{}}\n"))),
                    "setup",
                    None,
                ))
                .unwrap();
            (id, r.patch)
        })
        .collect();
    let results = at_once(N, |i| {
        let (id, tip) = &tips[i];
        s.apply(&req(
            &ws,
            id,
            Some(tip),
            Transformation::Rename("target".into()),
            &format!("r{i}"),
            None,
        ))
    });
    let wins = results.iter().filter(|r| matches!(r, Ok(c) if landed(c))).count();
    assert_eq!(wins, 1, "exactly one rename lands: {results:?}");
    for r in results.iter().filter_map(|r| r.as_ref().err()) {
        assert!(r.is("name-taken") && r.status == 409, "{r:?}");
        let id: SymbolId = serde_json::from_value(r.body.detail.clone()).unwrap();
        assert_eq!(id.name, "target");
    }
    assert_eq!(s.query(&ws, &sym("src/lib.rs", "target")).len(), 1);
    let live: Vec<SymbolView> = s.ok(
        "query-symbol",
        json!({ "workspace": ws, "query": SymbolQuery::Component(COMPONENT.into()) }),
    );
    assert_eq!(live.len(), N, "renames lose no symbol");
    assert_clean(&s.verify(&ws));
    timed("e", t0);
}

// ---- F ------------------------------------------------------------------------------

const IDLIST: &str = "src/idlist.rs";

/// record-store's tracked files, component-relative.
fn record_store() -> Vec<(String, Vec<u8>)> {
    let root = gatelib::repo_root();
    let out = Command::new("git")
        .args(["ls-files", "components/record-store"])
        .current_dir(&root)
        .output()
        .expect("git ls-files");
    let mut files: Vec<(String, Vec<u8>)> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|p| {
            (
                p.trim_start_matches("components/record-store/").to_string(),
                std::fs::read(root.join(p)).unwrap(),
            )
        })
        .collect();
    files.sort();
    assert!(
        files.iter().any(|f| f.0 == IDLIST) && files.iter().any(|f| f.0.ends_with(".wit")),
        "{:?}",
        files.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
    files
}

const CHUNK_KEY: &str = "pub fn chunk_key(base: &str, seq: u32) -> String {\n";
const IS_CHUNK_KEY: &str = "pub fn is_chunk_key(key: &str) -> bool {\n";
const PAGE_START: &str = "pub fn page_start(ids: &[String], after: &str) -> usize {\n";
const IS_ZERO_END: &str = "fn is_zero(n: &u32) -> bool {\n    *n == 0\n}\n";
const BY_A: &str = "\n/// Agent A's helper.\nfn inserted_by_a() -> u32 {\n    1\n}\n";
const BY_B: &str = "\n/// Agent B's helper.\nfn inserted_by_b() -> u32 {\n    2\n}\n";

#[test]
fn f_real_files_ingest_export_materialize_and_race() {
    let t0 = Instant::now();
    let Some(s) = Stack::up("f") else { return };
    let ws = s.ws("f");
    let files = record_store();

    // The whole component, in, out, and onto disk: byte for byte.
    let reports = s.ingest_tree(&ws, &files, None, false);
    reports.iter().for_each(assert_ingested);
    let symbols: usize = reports.iter().map(|r| r.patches.len()).sum();
    let snap = s.export(&ws).unwrap();
    let got = s.files(&snap);
    assert_eq!(got.len(), files.len());
    for (p, b) in &files {
        assert!(got.get(p) == Some(b), "{p} differs after the round trip");
    }
    let originals = tempfile::tempdir().unwrap();
    write_tree(originals.path(), &files);
    let git_tree = git_tree_of(originals.path());
    assert_eq!(
        snap.git_tree.as_deref(),
        Some(git_tree.as_str()),
        "snapshot git-tree vs git write-tree of the originals"
    );
    let dest = s.allow_root().join("f-out");
    let m = s.materialize(&ws, &dest).unwrap();
    assert_eq!(read_dir_files(&dest), files.iter().cloned().collect::<BTreeMap<_, _>>());
    assert_eq!(git_tree_of(&dest), git_tree);
    assert_eq!(m.bytes as usize, files.iter().map(|f| f.1.len()).sum::<usize>());
    eprintln!(
        "e2e_vcs::f: {} files, {} bytes, {symbols} symbols, git tree {git_tree}",
        files.len(),
        m.bytes
    );
    // Unchanged, re-ingested: nothing to write.
    let h = s.head(&ws);
    let again = s.ingest_tree(&ws, &files, Some(h), true);
    assert_eq!(again.iter().map(|r| r.patches.len()).sum::<usize>(), 0);

    let original =
        String::from_utf8(files.iter().find(|f| f.0 == IDLIST).unwrap().1.clone()).unwrap();

    // Two agents, two neighbouring functions, one read point, at once.
    let h = s.head(&ws);
    let a_copy = with_line(&original, CHUNK_KEY, "    // agent A was here");
    let b_copy = with_line(&original, IS_CHUNK_KEY, "    // agent B was here");
    let reps = at_once(2, |i| s.ingest(&ws, IDLIST, [&a_copy, &b_copy][i], ["a", "b"][i], Some(h)));
    reps.iter().for_each(assert_ingested);
    let both = with_line(&a_copy, IS_CHUNK_KEY, "    // agent B was here");
    assert_eq!(s.file(&ws, IDLIST), both);

    // The same function, from one read point: one conflict, both versions kept.
    let h = s.head(&ws);
    let a_copy = with_line(&both, PAGE_START, "    // A: pages start at zero");
    let b_copy = with_line(&both, PAGE_START, "    // B: pages start at one");
    let reps = at_once(2, |i| s.ingest(&ws, IDLIST, [&a_copy, &b_copy][i], ["a", "b"][i], Some(h)));
    let conflicted: Vec<&wire::IngestPatch> = reps
        .iter()
        .flat_map(|r| &r.patches)
        .filter(
            |p| matches!(&p.outcome, wire::Outcome::Ok(c) if c.outcome == PatchOutcome::Conflicted),
        )
        .collect();
    assert_eq!(conflicted.len(), 1, "exactly one side conflicts: {reps:?}");
    assert_eq!(conflicted[0].symbol.name, "page_start");
    let open = s.conflicts(&ws);
    assert_eq!(open.len(), 1);
    let side = |c: &Option<Content>| match c {
        Some(Content::Inline(t)) => t.clone(),
        other => panic!("{other:?}"),
    };
    let (l, r) = (side(&open[0].left.content), side(&open[0].right.content));
    assert!(
        l.contains("// A: pages") != r.contains("// A: pages")
            && (l.contains("// B: pages") || r.contains("// B: pages")),
        "both versions verbatim:\n{l}\n{r}"
    );
    assert!(s.export(&ws).unwrap_err().is("unresolved-conflict"));
    let res: CommitResult = s.ok(
        "resolve-conflict",
        json!({ "workspace": ws, "conflict": open[0].id, "resolution": Transformation::Replace(inline(&l)),
                "agent": agent("resolver"), "message": null }),
    );
    assert!(landed(&res));
    let resolved = if l.contains("// A: pages") { &a_copy } else { &b_copy };
    assert_eq!(&s.file(&ws, IDLIST), resolved);

    // Two inserts at one spot, at once: both land, neither conflicts.
    let h = s.head(&ws);
    let spot = format!("{IS_ZERO_END}");
    let a_copy = resolved.replacen(&spot, &format!("{spot}{BY_A}"), 1);
    let b_copy = resolved.replacen(&spot, &format!("{spot}{BY_B}"), 1);
    let reps = at_once(2, |i| s.ingest(&ws, IDLIST, [&a_copy, &b_copy][i], ["a", "b"][i], Some(h)));
    reps.iter().for_each(assert_ingested);
    assert!(s.conflicts(&ws).is_empty());
    let got = s.file(&ws, IDLIST);
    let ab = resolved.replacen(&spot, &format!("{spot}{BY_A}{BY_B}"), 1);
    let ba = resolved.replacen(&spot, &format!("{spot}{BY_B}{BY_A}"), 1);
    assert!(
        got == ab || got == ba,
        "both inserts, right after is_zero, in one of the two orders:\n{got}"
    );

    // And the final tree onto disk, checked against git once more.
    let mut want: BTreeMap<String, Vec<u8>> = files.iter().cloned().collect();
    want.insert(IDLIST.into(), got.into_bytes());
    let snap = s.export(&ws).unwrap();
    let dest = s.allow_root().join("f-final");
    s.materialize(&ws, &dest).unwrap();
    assert_eq!(read_dir_files(&dest), want);
    assert_eq!(Some(git_tree_of(&dest)), snap.git_tree);
    assert_clean(&s.verify(&ws));
    timed("f", t0);
}

// ---- G ------------------------------------------------------------------------------

#[test]
fn g_a_function_moved_by_ingest_exports_exactly_and_back() {
    let t0 = Instant::now();
    let Some(s) = Stack::up("g") else { return };
    let ws = s.ws("g");
    let original =
        String::from_utf8(record_store().into_iter().find(|f| f.0 == IDLIST).unwrap().1).unwrap();
    assert_ingested(&s.ingest(&ws, IDLIST, &original, "setup", None));

    // is_zero, cut from above chunk_key and pasted just before enc().
    let block = format!("{IS_ZERO_END}\n");
    let enc = "fn enc<T: Serialize>(";
    assert!(original.contains(&block) && original.contains(enc));
    let moved = original.replacen(&block, "", 1).replacen(enc, &format!("{block}{enc}"), 1);
    assert_ne!(moved, original);
    let h = s.head(&ws);
    let r = s.ingest(&ws, IDLIST, &moved, "mover", Some(h));
    assert_ingested(&r);
    assert!(
        r.patches.iter().any(|p| p.edit == wire::EditKind::Move),
        "a move patch: {:?}",
        r.patches
    );
    assert!(
        !r.patches
            .iter()
            .any(|p| p.edit == wire::EditKind::Create || p.edit == wire::EditKind::Delete),
        "{:?}",
        r.patches
    );
    assert_eq!(s.file(&ws, IDLIST), moved);

    let h = s.head(&ws);
    assert_ingested(&s.ingest(&ws, IDLIST, &original, "mover", Some(h)));
    assert_eq!(s.file(&ws, IDLIST), original);
    assert_clean(&s.verify(&ws));
    timed("g", t0);
}

// ---- H ------------------------------------------------------------------------------

#[test]
fn h_a_crashed_writers_intent_blocks_export_until_the_lease_passes() {
    let t0 = Instant::now();
    let Some(mut s) = Stack::up("h") else { return };
    const LEASE: Duration = Duration::from_secs(4);
    let lease = ["--lease-secs", "4"];
    let ws = s.ws("h");
    s.restart_vcs(&lease);
    let f = sym("src/lib.rs", "f");
    let pf = s
        .apply(&req(&ws, &f, None, Transformation::Create(inline("fn f() {}\n")), "setup", None))
        .unwrap();
    let edit =
        req(&ws, &f, Some(&pf.patch), Transformation::Replace(inline("fn f() { 1 }\n")), "x", None);

    s.restart_vcs(&[lease[0], lease[1], "--crash-before-step", "tip", "--i-know-this-is-a-test"]);
    let crashed = Instant::now();
    assert!(s.apply(&edit).unwrap_err().is("storage-error"));
    s.wait_crashed();
    s.start_vcs(&lease);

    // Inside the lease: in flight, not an inconsistency, and it holds export.
    let v = s.verify(&ws);
    assert!(v.issues.is_empty(), "{:?}", v.issues);
    assert_eq!(v.in_flight.len(), 1, "the crashed op is presumed in flight: {v:?}");
    let refused = s.export(&ws).expect_err("export waits for the op in flight");
    let refused_at = crashed.elapsed();
    assert!(refused.is("concurrent-modification") && refused.status == 409, "{refused:?}");
    assert!(refused_at < LEASE, "refused inside the lease ({refused_at:?})");

    // Past it: the next reader fences the op, and export goes through.
    let snap = loop {
        match s.export(&ws) {
            Ok(snap) => break snap,
            Err(r) if r.is("concurrent-modification") => {
                assert!(
                    crashed.elapsed() < LEASE + Duration::from_secs(10),
                    "still refused long after the lease"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(r) => panic!("{r:?}"),
        }
    };
    let cleared_at = crashed.elapsed();
    assert!(
        cleared_at >= LEASE - Duration::from_millis(500),
        "cleared at {cleared_at:?}, before the lease"
    );
    assert_eq!(
        String::from_utf8(s.files(&snap)["src/lib.rs"].clone()).unwrap(),
        "fn f() {}\n",
        "the crashed edit never landed"
    );
    assert_clean(&s.verify(&ws));
    let again = s.apply(&edit).unwrap();
    assert_eq!(again.outcome, PatchOutcome::Applied);
    eprintln!(
        "e2e_vcs::h: refused at {refused_at:.2?}, cleared at {cleared_at:.2?} (lease {LEASE:?})"
    );
    timed("h", t0);
}
