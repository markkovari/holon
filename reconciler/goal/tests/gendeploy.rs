//! The loop deployed through the platform, one environment per branch. The gate is `mock-fitness`:
//! a linked gate in a deployed graph gets no egress (ADR-0008).

use std::time::{Duration, Instant};

use holon_fleet::fleet::{repo_root, Fleet};
use holon_goal::generation::{default_strategies, fan_out_from, on_hosts};
use serde_json::{json, Value};

struct Api {
    base: String,
    http: reqwest::blocking::Client,
    token: String,
}

impl Api {
    fn new(base: String) -> Self {
        let http =
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().unwrap();
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline {
            if http.get(&base).send().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let mut me = Self { base, http, token: String::new() };
        let body = json!({ "email": "ada@deploy.test", "password": "password123" });
        let _ = me.raw("/api/register", body.clone());
        me.token = me.raw("/api/login", body)["token"].as_str().unwrap_or_default().to_string();
        assert!(!me.token.is_empty(), "could not log in to the control plane");
        me
    }

    fn raw(&self, path: &str, body: Value) -> Value {
        self.http
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .ok()
            .and_then(|r| r.json().ok())
            .unwrap_or(Value::Null)
    }

    fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .unwrap();
        (r.status().as_u16(), r.json().unwrap_or(Value::Null))
    }

    fn upload(&self, id: &str, file: &str, config: &str) -> u16 {
        let wasm =
            std::fs::read(repo_root().join("components/target/wasm32-wasip2/release").join(file))
                .unwrap_or_else(|e| {
                    panic!("missing {file} — run `cargo xtask build --force`: {e}")
                });
        let mut url = format!("{}/api/components?id={id}", self.base);
        if !config.is_empty() {
            url.push_str(&format!("&config={config}"));
        }
        self.http.post(url).bearer_auth(&self.token).body(wasm).send().unwrap().status().as_u16()
    }
}

fn wait_for(fleet: &Fleet, needle: &str, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if fleet.node_log("n1").contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

const BRANCHES: usize = 2;

/// Seeds differ by `generation::STRIDE`, so a rule per seed makes the branches disagree.
const MOCK_SCRIPT: &str = r#"{"rules":[
  {"when":"make it 42","seed":700,"text":"=== FILE: src/lib.rs\npub fn answer() -> u32 { 42 }\n=== END"},
  {"when":"make it 42","seed":800,"text":"=== FILE: src/lib.rs\npub fn answer() -> u32 { 41 }\n=== END"}
]}"#;

#[test]
fn a_generation_runs_one_branch_per_environment() {
    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let fleet = Fleet::start_with_platform("deploy", 1);
    let api = Api::new(fleet.platform_url());

    for (id, file, config) in [
        ("probe", "driver_probe.wasm", ""),
        ("driver", "agent_driver.wasm", ""),
        ("writer", "agent_writer.wasm", ""),
        ("llm", "mock_provider.wasm", "mock-model,mock-script"),
        ("gate", "mock_fitness.wasm", "gate-script"),
    ] {
        assert!(matches!(api.upload(id, file, config), 200 | 201), "upload {id} failed");
    }

    // plug = provider, socket = consumer (the link table keys on socket).
    let nodes = json!([
        { "id": "probe" },
        { "id": "driver" },
        { "id": "writer" },
        { "id": "llm", "config": { "mock-model": "mock-agent", "mock-script": MOCK_SCRIPT } },
        { "id": "gate", "config": { "gate-script": "{\"be-42\":\"42\"}" } },
    ]);
    // Versions must follow the packages (they drifted once; no CI runs this). `graph:run` `use`s
    // `graph:agent` and `graph:fitness`, hence their probe edges.
    let edges = json!([
        { "plug": "driver", "socket": "probe",  "iface": "graph:run/driver@0.2.0" },
        { "plug": "writer", "socket": "probe",  "iface": "graph:agent/writer@0.2.0" },
        { "plug": "gate",   "socket": "probe",  "iface": "graph:fitness/evaluator@0.2.0" },
        { "plug": "writer", "socket": "driver", "iface": "graph:agent/writer@0.2.0" },
        { "plug": "gate",   "socket": "driver", "iface": "graph:fitness/evaluator@0.2.0" },
        { "plug": "llm",    "socket": "writer", "iface": "llm:inference/inference@0.1.0" },
    ]);
    // Linked, not fused: this shape does not fuse cleanly.
    let (code, dep) = api.post(
        "/api/deployments",
        json!({ "name": "swarm", "strategy": "linked", "nodes": nodes, "edges": edges }),
    );
    assert_eq!(code, 201, "deploy failed: {dep}");
    let id = dep["id"].as_str().unwrap().to_string();

    let deadline = Instant::now() + Duration::from_secs(150);
    let (mut saved, mut why) = (false, Value::Null);
    while Instant::now() < deadline && !saved {
        let (code, body) = api.post(&format!("/api/deployments/{id}/save"), json!({}));
        (saved, why) = (code == 200, body);
        if !saved {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    assert!(saved, "the graph never became saveable: {why}\n{}", fleet.reconciler_log());
    let parent_host = why["ingress"].as_str().expect("no ingress host").to_string();
    assert!(
        wait_for(&fleet, "started ada/swarm/", Duration::from_secs(150)),
        "the parent graph never started:\n{}",
        fleet.node_log("n1")
    );

    let names: Vec<String> = (0..BRANCHES).map(|i| format!("branch-{i}")).collect();
    for n in &names {
        let (code, spawned) = api.post("/api/environments", json!({ "app": "swarm", "env": n }));
        assert_eq!(code, 201, "spawning {n} failed: {spawned}");
    }
    for n in &names {
        assert!(
            wait_for(&fleet, &format!("started ada/swarm-env-{n}/"), Duration::from_secs(180)),
            "environment {n} never started:\n{}",
            fleet.node_log("n1")
        );
    }

    // Derived hostnames (ADR-0083), each a strict suffix of the parent's.
    let hosts: Vec<String> = names.iter().map(|n| format!("{n}.{parent_host}")).collect();
    let strategies = on_hosts(&default_strategies(BRANCHES as u16), &hosts);

    let base = json!({
        "text": "make it 42",
        "writable": ["src/lib.rs"],
        "context": [{ "path": "src/lib.rs", "content": "pub fn answer() -> u32 { 0 }" }],
        "previous": [],
        "checks": [{ "id": "be-42", "required": true, "weight": 1, "command": ["unused"] }],
        "base_commit": "5555555555555555555555555555555555555555",
        "base_tree": [{ "path": "src/lib.rs", "content": "pub fn answer() -> u32 { 0 }\n" }],
        "max_attempts": 1,
        "seed": 0,
    });

    let url = format!("http://127.0.0.1:{}/run", fleet.ingress_port);
    let timeout = Duration::from_secs(120);

    // A real run, retried, not a readiness signal: those have gone green over broken deployments.
    for host in &hosts {
        let mut warm = strategies[0].clone();
        warm.host = host.clone();
        let deadline = Instant::now() + Duration::from_secs(150);
        let ok = loop {
            // seed 700 is in the script; 999 would come back provider-down.
            let r = fan_out_from(
                &url,
                "unused",
                &base,
                std::slice::from_ref(&warm),
                None,
                700,
                timeout,
            );
            if r[0].note.is_empty() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(500));
        };
        assert!(ok, "environment at {host} never served a run:\n{}", fleet.node_log("n1"));
    }

    // Branch 0 uses seed 700 (writes 42), branch 1 uses 700 + STRIDE = 800 (writes 41).
    let entries = fan_out_from(&url, &parent_host, &base, &strategies, None, 700, timeout);
    for e in &entries {
        println!(
            "    {:<9} accepted={:<5} score={:<5} host-ok={} {}",
            e.branch,
            e.accepted,
            e.score,
            e.note.is_empty(),
            e.files.get(0).and_then(|f| f["content"].as_str()).unwrap_or("").replace('\n', " ")
        );
    }

    assert_eq!(entries.len(), BRANCHES);
    assert!(
        entries.iter().all(|e| e.note.is_empty()),
        "a branch never ran in its environment: {entries:?}"
    );

    // They disagree because they are separate environments, not one app answering twice.
    assert!(entries[0].accepted, "branch 0 was seeded to write 42 and pass: {entries:?}");
    assert!(!entries[1].accepted, "branch 1 was seeded to write 41 and fail: {entries:?}");
    assert_ne!(
        entries[0].digest, entries[1].digest,
        "the two environments produced the same candidate — they are one app wearing two names: {entries:?}"
    );

    // Distinct in the fleet's ledger: a shared app would show one instance.
    let ledger =
        std::fs::read_to_string(fleet.state_dir().join("n1/instances.json")).unwrap_or_default();
    for n in &names {
        assert!(
            ledger.contains(&format!("swarm-env-{n}")),
            "environment {n} is not in the node's ledger — it answered but was never really placed:\n{ledger}"
        );
    }

    println!(
        "    {BRANCHES} branches, each in its own deployed environment: 42 accepted, 41 refused"
    );
}
