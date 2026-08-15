//! End to end: authored YAML, a real fleet, real requests.
//!
//! Six apps go to ONE fleet at once — three that must serve traffic, three that must
//! be refused with a reason a human can act on. Deploying them together is the point:
//! a refusal that also stops the other five from being placed is a bug this catches
//! and a single-app test cannot.
//!
//! Run with `cargo nextest run --release -E 'test(e2e)'` after `cargo build
//! --release` in `host/`. It needs `nats-server` on PATH and the built `comp-host`.
//!
//! Everything is Rust: the control plane below is an axum stub in-process rather than
//! a script, so the fixtures, the conversion, the assertions and the harness are one
//! language and one test runner.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use comp_reconciler::plug;
use comp_reconciler::spec::AppSpec;
use serde_json::{json, Value};

const SECRET: &str = "test-secret";
const LATTICE: &str = "e2e";

/// Ports are fixed rather than searched: nextest gives this test its own process, and
/// a fixed set makes a leaked child obvious instead of silently moving elsewhere.
const PLATFORM_PORT: u16 = 8399;
const NATS_PORT: u16 = 4332;
const INGRESS_PORT: u16 = 8394;

/// Kills the child on drop, so a failed assertion cannot leave a fleet running.
///
/// The process name is not carried: it is only ever used in the panic message at
/// spawn time, and a field nobody reads is a field that goes stale.
struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is comp/reconciler; the artifacts and fixtures live above it.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn comp_host_bin() -> PathBuf {
    std::env::var("COMP_HOST_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| repo_root().join("host/target/release/comp-host"))
}

fn spawn(name: &'static str, mut cmd: Command) -> Kill {
    let child = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("spawning {name}: {e}"));
    Kill(child)
}

/// The state the stub control plane keeps: the manifests it serves, and the digest
/// each component has been given once its artifact was distributed.
#[derive(Default)]
struct Platform {
    manifests: Vec<Value>,
    pushed: HashMap<String, String>,
    artifacts: HashMap<String, Vec<u8>>,
    unschedulable: Vec<Value>,
}

type Shared = Arc<Mutex<Platform>>;

/// A control plane that does exactly what the reconciler needs and nothing else:
/// hand out revisions, hand over artifact bytes, accept the digest that comes back,
/// and record what could not be scheduled.
async fn serve_platform(state: Shared) {
    let app = Router::new()
        .route(
            "/api/internal/revisions",
            get({
                let state = state.clone();
                move || {
                    let state = state.clone();
                    async move {
                        let p = state.lock().unwrap();
                        // A component with no digest yet is one the reconciler must
                        // push first; that queue is what `pushed` below closes.
                        let out: Vec<Value> = p
                            .manifests
                            .iter()
                            .map(|m| {
                                let mut m = m.clone();
                                for c in m["components"].as_array_mut().unwrap() {
                                    let id = c["id"].as_str().unwrap().to_string();
                                    if let Some(d) = p.pushed.get(&id) {
                                        c["digest"] = json!(d);
                                    }
                                }
                                json!({ "revision": 1, "manifest": m })
                            })
                            .collect();
                        Json(json!({ "revisions": out }))
                    }
                }
            }),
        )
        .route(
            "/api/internal/pending-pushes",
            get({
                let state = state.clone();
                move || {
                    let state = state.clone();
                    async move {
                        let p = state.lock().unwrap();
                        let pending: Vec<Value> = p
                            .artifacts
                            .keys()
                            .filter(|id| !p.pushed.contains_key(*id))
                            .map(|id| json!({ "key": id }))
                            .collect();
                        Json(json!({ "pending": pending }))
                    }
                }
            }),
        )
        .route(
            "/api/internal/artifact",
            get({
                let state = state.clone();
                move |q: axum::extract::Query<HashMap<String, String>>| {
                    let state = state.clone();
                    async move {
                        let key = q.get("key").cloned().unwrap_or_default();
                        match state.lock().unwrap().artifacts.get(&key) {
                            Some(b) => (StatusCode::OK, b.clone()),
                            None => (StatusCode::NOT_FOUND, Vec::new()),
                        }
                    }
                }
            }),
        )
        .route(
            "/api/internal/pushed",
            post({
                let state = state.clone();
                move |Json(body): Json<Value>| {
                    let state = state.clone();
                    async move {
                        if let (Some(id), Some(d)) =
                            (body["key"].as_str(), body["digest"].as_str())
                        {
                            state
                                .lock()
                                .unwrap()
                                .pushed
                                .insert(id.to_string(), d.to_string());
                        }
                        StatusCode::OK
                    }
                }
            }),
        )
        .route(
            "/api/internal/status",
            post({
                let state = state.clone();
                move |Json(body): Json<Value>| {
                    let state = state.clone();
                    async move {
                        if let Some(u) = body["unschedulable"].as_array() {
                            state.lock().unwrap().unschedulable.extend(u.clone());
                        }
                        StatusCode::OK
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", PLATFORM_PORT)).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// Every fixture, converted through the same code path a real deploy uses.
fn load_fixtures() -> Vec<Value> {
    let dir = repo_root().join("e2e/fixtures");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no fixtures in {}", dir.display());
    files
        .iter()
        .map(|f| {
            let text = std::fs::read_to_string(f).unwrap();
            let spec = AppSpec::parse(&text)
                .unwrap_or_else(|e| panic!("{}: {e:#}", f.file_name().unwrap().to_string_lossy()));
            serde_json::to_value(spec.to_manifest(None).unwrap()).unwrap()
        })
        .collect()
}

fn post_json(host: &str) -> Result<u16, String> {
    let body = json!({ "key": "e2e", "capacity": 100_000_000u64, "refill": 100_000_000u64 });
    let out = Command::new("curl")
        .args([
            "-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "10",
            "-X", "POST", "-H", "content-type: application/json", "-H",
        ])
        .arg(format!("Host: {host}"))
        .args(["-d", &body.to_string(), &format!("http://127.0.0.1:{INGRESS_PORT}/api/ratelimit")])
        .output()
        .map_err(|e| e.to_string())?;
    String::from_utf8_lossy(&out.stdout).trim().parse().map_err(|e| format!("{e}"))
}

/// Poll, do not sleep on a number. Inventory is a heartbeat behind reality and a
/// parked app has to be activated first, so a snapshot taken at the wrong moment
/// fails on a working system — a mistake already made twice here (ADR-0042, 0045).
fn serves(host: &str, within: Duration) -> Result<(), String> {
    let deadline = Instant::now() + within;
    let mut last = String::from("never answered");
    while Instant::now() < deadline {
        match post_json(host) {
            Ok(200) => return Ok(()),
            Ok(code) => last = format!("HTTP {code}"),
            Err(e) => last = e,
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(last)
}

#[test]
fn six_manifests_one_fleet() {
    let root = repo_root();
    let raw = root.join("components/target/wasm32-wasip2/release");
    for f in [&raw.join("gate_domain.wasm"), &raw.join("record_store.wasm"), &raw.join("shaper.wasm")] {
        assert!(f.exists(), "missing {} — run `just build`", f.display());
    }
    // Composed here rather than by `just compose-gate`, which is the whole point of
    // `plug`: the fused artifact is derived from what `gate-domain` imports, so this
    // test needs no second manual step and cannot run against a stale composition
    // someone built by hand three commits ago.
    let catalog = plug::Catalog::scan(&plug::default_dirs(&root));
    let composed = plug::compose_to("gate-domain", &catalog, &root.join("components/target/composed"))
        .expect("gate-domain composes with what it imports");
    assert!(
        comp_host_bin().exists(),
        "missing {} — run `cargo build --release` in host/",
        comp_host_bin().display()
    );

    let state: Shared = Arc::new(Mutex::new(Platform {
        manifests: load_fixtures(),
        // The fused app gets the composed artifact; the linked one gets the three raw
        // components. That difference IS the two strategies (ADR-0005).
        artifacts: HashMap::from([
            ("gate".into(), std::fs::read(&composed).unwrap()),
            ("record-store".into(), std::fs::read(raw.join("record_store.wasm")).unwrap()),
            ("shaper".into(), std::fs::read(raw.join("shaper.wasm")).unwrap()),
        ]),
        ..Default::default()
    }));

    let rt = tokio::runtime::Runtime::new().unwrap();
    {
        let state = state.clone();
        rt.spawn(async move { serve_platform(state).await });
    }
    let _guard = rt.enter();

    let dir = tempfile::tempdir().unwrap();
    let sp = dir.path();

    let mut nats = Command::new("nats-server");
    nats.args(["-js", "-sd"])
        .arg(sp.join("nats"))
        .args(["-a", "127.0.0.1", "-p", &NATS_PORT.to_string()]);
    let _nats = spawn("nats-server", nats);
    std::thread::sleep(Duration::from_secs(2));

    let nats_url = format!("nats://127.0.0.1:{NATS_PORT}");
    let mut hosts = Vec::new();
    for n in 1..=2 {
        let mut c = Command::new(comp_host_bin());
        c.args(["--lattice-nats", &nats_url, "--node", &format!("n{n}"), "--lattice", LATTICE])
            .args(["--addr", &format!("127.0.0.1:391{n}")])
            .args(["--advertise-addr", &format!("127.0.0.1:391{n}")])
            .arg("--state-dir")
            .arg(sp.join(format!("n{n}")));
        hosts.push(spawn("comp-host", c));
    }
    std::thread::sleep(Duration::from_secs(2));

    let mut rec = Command::new(env!("CARGO_BIN_EXE_comp-reconciler"));
    rec.args(["--platform-url", &format!("http://127.0.0.1:{PLATFORM_PORT}")])
        .args(["--secret", SECRET, "--nats-url", &nats_url, "--lattice", LATTICE])
        .args(["--interval", "3"]);
    let _rec = spawn("comp-reconciler", rec);

    let mut ing = Command::new(env!("CARGO_BIN_EXE_comp-ingress"));
    ing.args(["--addr", &format!("127.0.0.1:{INGRESS_PORT}")])
        .args(["--nats-url", &nats_url, "--lattice", LATTICE, "--refresh-secs", "2"]);
    let _ing = spawn("comp-ingress", ing);

    // Serving is checked by INVOKING, never by reading inventory: an app that is
    // placed but does not answer is exactly the failure a status check misses.
    let mut failures: Vec<String> = Vec::new();
    for (host, what) in [
        ("fused.e2e.test", "a fused artifact serves over HTTP"),
        ("linked.e2e.test", "a runtime-linked graph serves, so both imports were bound"),
        ("zero.e2e.test", "a parked app is activated by the request itself"),
    ] {
        match serves(host, Duration::from_secs(90)) {
            Ok(()) => println!("    PASS  {host:22} {what}"),
            Err(e) => {
                println!("    FAIL  {host:22} {what} ({e})");
                failures.push(format!("{host}: {e}"));
            }
        }
    }

    // Refusals are checked by their REASON. "It was refused" would pass for a refusal
    // with the wrong reason, and a reason nobody can act on is barely better than a
    // crash — so each names what an operator would need in order to fix it.
    let deadline = Instant::now() + Duration::from_secs(30);
    let expected: [(&str, &[&str]); 3] = [
        ("conflict", &["records:store/store", "record-store", "shaper"]),
        ("ungrantable", &["wasi:blobstore/blobstore"]),
        ("unplaceable", &["region", "mars"]),
    ];
    loop {
        let said = state.lock().unwrap().unschedulable.clone();
        let complete = expected.iter().all(|(app, needles)| {
            said.iter().any(|u| {
                u["app"] == *app
                    && needles.iter().all(|n| u["reason"].as_str().is_some_and(|r| r.contains(n)))
            })
        });
        if complete || Instant::now() > deadline {
            for (app, needles) in expected {
                let reason = said
                    .iter()
                    .find(|u| u["app"] == app)
                    .and_then(|u| u["reason"].as_str())
                    .unwrap_or("");
                let missing: Vec<&str> =
                    needles.iter().copied().filter(|n| !reason.contains(n)).collect();
                if reason.is_empty() {
                    println!("    FAIL  {app:12} was not refused at all");
                    failures.push(format!("{app} was not refused"));
                } else if !missing.is_empty() {
                    println!("    FAIL  {app:12} reason missing {missing:?}: {reason}");
                    failures.push(format!("{app}: reason missing {missing:?}"));
                } else {
                    println!("    PASS  {app:12} {}", &reason[..reason.len().min(78)]);
                }
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    // Only a shared fleet can show this: a broken manifest must not take healthy apps
    // down with it.
    let still = ["fused.e2e.test", "linked.e2e.test", "zero.e2e.test"]
        .iter()
        .filter(|h| serves(h, Duration::from_secs(10)).is_ok())
        .count();
    println!("    {still}/3 apps still serving alongside 3 refused ones");
    if still != 3 {
        failures.push("a refused manifest interfered with healthy apps".into());
    }

    assert!(failures.is_empty(), "{failures:#?}");
}
