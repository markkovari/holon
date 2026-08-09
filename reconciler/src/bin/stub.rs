//! A stand-in control plane for benchmarks: serves authored YAML specs and nothing
//! else.
//!
//! The thing under test in `bench/` is the host, the reconciler and the ingress. A
//! real `platform-domain` would drag in orgs, auth and a records store to measure
//! none of them, so this serves the four endpoints the reconciler actually calls and
//! stops there.
//!
//! It replaces `stub-control-plane.py`. Not for its own sake: this one reads the same
//! `comp/v1` YAML the e2e fixtures use, through the same `AppSpec::to_manifest`, so a
//! benchmark and a test cannot disagree about what a manifest means — and a fixture
//! that stops parsing breaks the build rather than one script.
//!
//! ```
//! comp-stub --port 8099 --spec bench/autoscale/app.yaml \
//!   --artifact gate=components/target/gate_domain.composed.wasm
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::extract::Query;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use comp_reconciler::spec::AppSpec;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-stub", about = "A stand-in control plane that serves app specs")]
struct Args {
    #[arg(long, default_value = "8099")]
    port: u16,

    /// One or more `comp/v1` YAML specs. Repeat the flag, or point at a directory.
    #[arg(long = "spec", required = true)]
    specs: Vec<std::path::PathBuf>,

    /// `id=path/to.wasm`, repeated. The bytes the reconciler distributes.
    #[arg(long = "artifact")]
    artifacts: Vec<String>,

    /// Overrides `tenant:` in every spec, for benchmarks that vary it.
    #[arg(long)]
    tenant: Option<String>,
}

#[derive(Default)]
struct State {
    manifests: Vec<Value>,
    artifacts: HashMap<String, Vec<u8>>,
    pushed: HashMap<String, String>,
}

type Shared = Arc<Mutex<State>>;

fn load(paths: &[std::path::PathBuf], tenant: Option<&str>) -> Result<Vec<Value>> {
    let mut files = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut found: Vec<_> = std::fs::read_dir(p)
                .with_context(|| format!("reading {}", p.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
                .collect();
            found.sort();
            files.extend(found);
        } else {
            files.push(p.clone());
        }
    }
    files
        .iter()
        .map(|f| {
            let text = std::fs::read_to_string(f)
                .with_context(|| format!("reading {}", f.display()))?;
            let spec = AppSpec::parse(&text).with_context(|| format!("in {}", f.display()))?;
            Ok(serde_json::to_value(spec.to_manifest(tenant)?)?)
        })
        .collect()
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mut artifacts = HashMap::new();
    for a in &args.artifacts {
        let (id, path) = a.split_once('=').context("--artifact wants id=path")?;
        artifacts.insert(id.to_string(), std::fs::read(path).with_context(|| path.to_string())?);
    }
    let manifests = load(&args.specs, args.tenant.as_deref())?;
    eprintln!(
        "comp-stub: {} app(s) on :{} — {}",
        manifests.len(),
        args.port,
        manifests.iter().filter_map(|m| m["app"].as_str()).collect::<Vec<_>>().join(", ")
    );
    let state: Shared = Arc::new(Mutex::new(State { manifests, artifacts, ..Default::default() }));

    let app = Router::new()
        .route("/api/internal/revisions", get(revisions))
        .route("/api/internal/pending-pushes", get(pending))
        .route("/api/internal/artifact", get(artifact))
        .route("/api/internal/pushed", post(pushed))
        // Accepted and dropped: the reconciler posts what it could not schedule, and
        // a benchmark reads that from its log rather than from here.
        .route("/api/internal/status", post(|| async { StatusCode::OK }))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", args.port)).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn revisions(axum::extract::State(s): axum::extract::State<Shared>) -> Json<Value> {
    let s = s.lock().unwrap();
    // A component with no digest is one the reconciler still has to distribute;
    // filling it in once pushed is what closes that queue.
    let revisions: Vec<Value> = s
        .manifests
        .iter()
        .map(|m| {
            let mut m = m.clone();
            if let Some(cs) = m["components"].as_array_mut() {
                for c in cs {
                    if let Some(d) = c["id"].as_str().and_then(|id| s.pushed.get(id)) {
                        c["digest"] = json!(d);
                    }
                }
            }
            json!({ "revision": 1, "manifest": m })
        })
        .collect();
    Json(json!({ "revisions": revisions }))
}

async fn pending(axum::extract::State(s): axum::extract::State<Shared>) -> Json<Value> {
    let s = s.lock().unwrap();
    let pending: Vec<Value> = s
        .artifacts
        .keys()
        .filter(|id| !s.pushed.contains_key(*id))
        .map(|id| json!({ "key": id }))
        .collect();
    Json(json!({ "pending": pending }))
}

async fn artifact(
    axum::extract::State(s): axum::extract::State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> (StatusCode, Vec<u8>) {
    let key = q.get("key").cloned().unwrap_or_default();
    match s.lock().unwrap().artifacts.get(&key) {
        Some(b) => (StatusCode::OK, b.clone()),
        None => (StatusCode::NOT_FOUND, Vec::new()),
    }
}

async fn pushed(
    axum::extract::State(s): axum::extract::State<Shared>,
    Json(body): Json<Value>,
) -> StatusCode {
    if let (Some(k), Some(d)) = (body["key"].as_str(), body["digest"].as_str()) {
        s.lock().unwrap().pushed.insert(k.to_string(), d.to_string());
    }
    StatusCode::OK
}
