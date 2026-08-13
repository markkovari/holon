//! A branch gets its own environment, and its own store.
//!
//! Until now every branch of a generation was a concurrent call carrying its own
//! base tree, which works only for as long as a run keeps nothing. The moment a
//! branch wants to KEEP something — a compiled artifact, a partial index, a git
//! object — it needs somewhere of its own, and that is what an environment is: a
//! derived app, so the bucket name derives with it (ADR-0023, ADR-0078).
//!
//! What was missing was a DOOR. `spawn_environment` set the manifest's ingress to
//! null, which is right about the hazard — the parent's hostname on two apps makes
//! the ingress route to whichever it saw last — and leaves an environment with no
//! address at all. A swarm branch is something that must be driven, and an app
//! with no address cannot be. ADR-0083 derives the host instead.
//!
//! So this test asserts three things that are each other's preconditions:
//!
//!   * an environment ANSWERS, on its own derived hostname
//!   * what it writes goes to ITS store, not the parent's
//!   * and not to any sibling's — asserted with all three writing the same KEY,
//!     because different keys would pass against one shared bucket

use std::time::Duration;

use comp_reconciler::fleet::{repo_root, Fleet};
use comp_reconciler::generation::{default_strategies, on_hosts};
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
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        while std::time::Instant::now() < deadline {
            if http.get(&base).send().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let mut me = Self { base, http, token: String::new() };
        let body = json!({ "email": "ada@branch.test", "password": "password123" });
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

    fn delete(&self, path: &str) -> (u16, Value) {
        let r =
            self.http.delete(format!("{}{path}", self.base)).bearer_auth(&self.token).send().unwrap();
        (r.status().as_u16(), r.json().unwrap_or(Value::Null))
    }

    fn upload(&self, id: &str, wasm: Vec<u8>) -> u16 {
        self.http
            .post(format!("{}/api/components?id={id}", self.base))
            .bearer_auth(&self.token)
            .body(wasm)
            .send()
            .unwrap()
            .status()
            .as_u16()
    }
}

fn wait_for(fleet: &Fleet, needle: &str, within: Duration) -> bool {
    let deadline = std::time::Instant::now() + within;
    while std::time::Instant::now() < deadline {
        if fleet.node_log("n1").contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// One call to a branch, by its own hostname.
///
/// Returns the PARSED answer, and `None` for anything that was not one. The first
/// version of this returned the raw body and the assertions used `contains`,
/// which passed against a null ingress: the ingress echoes the hostname it could
/// not route — `no replica of "branch-0.swarm.ada.test" is currently placed` —
/// and `contains("branch-0")` is perfectly true of that. An error that quotes
/// what you asked for will satisfy any substring check for what you asked for.
fn call(port: u16, host: &str, path: &str) -> Option<Value> {
    let http =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().unwrap();
    let r = http.get(format!("http://127.0.0.1:{port}{path}")).header("host", host).send().ok()?;
    if !r.status().is_success() {
        return None;
    }
    // `kv-probe` answers JSON and nothing else does; a body that will not parse
    // came from something that is not the branch.
    serde_json::from_str(&r.text().ok()?).ok()
}

const BRANCHES: usize = 3;

#[test]
fn every_branch_writes_its_own_store_and_none_writes_the_parents() {
    let fleet = Fleet::start_with_platform("branch", 1);
    let api = Api::new(fleet.platform_url());

    // `kv-probe` because it is the only component in the catalogue that takes its
    // bucket from the request rather than hardcoding `default`, and because its
    // whole world is http + keyvalue — so a store that turned out to be shared
    // could not be blamed on a link.
    let wasm = std::fs::read(
        repo_root().join("components/target/wasm32-wasip2/release/kv_probe.wasm"),
    )
    .expect("run `just build`");
    assert!(matches!(api.upload("kv", wasm), 200 | 201), "upload failed");

    let (code, dep) =
        api.post("/api/deployments", json!({ "name": "swarm", "nodes": [{"id": "kv"}], "edges": [] }));
    assert_eq!(code, 201, "deploy failed: {dep}");
    let id = dep["id"].as_str().unwrap().to_string();

    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let (mut saved, mut why) = (false, Value::Null);
    while std::time::Instant::now() < deadline && !saved {
        let (code, body) = api.post(&format!("/api/deployments/{id}/save"), json!({}));
        (saved, why) = (code == 200, body);
        if !saved {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    assert!(saved, "the deployment never became saveable: {why}\n{}", fleet.reconciler_log());
    let parent_host = why["ingress"].as_str().expect("no ingress host").to_string();
    assert!(
        wait_for(&fleet, "started ada/swarm/", Duration::from_secs(120)),
        "the parent never started:\n{}",
        fleet.node_log("n1")
    );

    // --- one environment per branch -----------------------------------------
    let names: Vec<String> = (0..BRANCHES).map(|i| format!("branch-{i}")).collect();
    for n in &names {
        let (code, spawned) = api.post("/api/environments", json!({ "app": "swarm", "env": n }));
        assert_eq!(code, 201, "spawning {n} failed: {spawned}");
    }
    for n in &names {
        assert!(
            wait_for(&fleet, &format!("started ada/swarm-env-{n}/"), Duration::from_secs(150)),
            "environment {n} never started:\n{}",
            fleet.node_log("n1")
        );
    }

    // The hostnames the strategies will use. Derived, so they cannot collide with
    // the parent — which is the whole reason a null ingress was the wrong fix.
    let hosts: Vec<String> = names.iter().map(|n| format!("{n}.{parent_host}")).collect();
    let strategies = on_hosts(&default_strategies(BRANCHES as u16), &hosts);
    assert_eq!(strategies[1].host, hosts[1], "a branch must carry its own address");

    // --- IT ANSWERS ON ITS OWN NAME -----------------------------------------
    // Which an environment could not do at all before ADR-0083: the manifest's
    // ingress was nulled, so the branch existed, ran, and had no door.
    let port = fleet.ingress_port;
    for (i, host) in hosts.iter().enumerate() {
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        let ok = loop {
            // `open: ok` from the probe itself. Not "did not say 503": an ingress
            // that cannot route still answers, and what it answers with quotes the
            // host it failed on.
            if call(port, host, "/who?name=default").map(|v| v["open"] == json!("ok")) == Some(true)
            {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(500));
        };
        assert!(
            ok,
            "branch {i} never answered on {host} — an environment with no ingress host has no \
             door, and a branch that cannot be driven is not a branch:\n{}",
            fleet.node_log("n1")
        );
    }

    // --- THE SAME KEY, FROM EVERY BRANCH ------------------------------------
    // The same key on purpose. Different keys would pass just as happily against
    // one shared bucket, which is the bug this is looking for.
    for (i, host) in hosts.iter().enumerate() {
        let r = call(port, host, &format!("/put?name=default&k=claim&v=branch-{i}"));
        assert_eq!(
            r.as_ref().map(|v| v["put"].clone()),
            Some(json!("claim")),
            "branch {i} could not write: {r:?}"
        );
    }

    for (i, host) in hosts.iter().enumerate() {
        let got = call(port, host, "/get?name=default&k=claim");
        assert_eq!(
            got.as_ref().and_then(|v| v["value"].as_str()),
            Some(format!("branch-{i}").as_str()),
            "branch {i} read {got:?} — every branch wrote the SAME key, so a shared store shows \
             up here as all three reading whichever wrote last"
        );
    }

    // --- AND THE PARENT NEVER SAW ANY OF IT ---------------------------------
    // An environment derives its app name so the bucket derives with it. If the
    // derivation were cosmetic, the parent would be holding the last write.
    // Retried until the parent ANSWERS, then asserted on what it said. `None` is
    // "did not answer", and the first version of this treated that as "did not
    // hold the write" — so a parent that was merely unreachable would have proved
    // isolation. Under the full suite it WAS unreachable, and the test failed
    // claiming the opposite of what had happened.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let parent = loop {
        if let Some(v) = call(port, &parent_host, "/get?name=default&k=claim") {
            break Some(v);
        }
        if std::time::Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    let parent = parent.unwrap_or_else(|| {
        panic!(
            "the parent never answered, so this could not tell an isolated store from an \
             unreachable one:\n{}",
            fleet.node_log("n1")
        )
    });
    assert_eq!(
        parent["found"],
        json!(false),
        "the parent's store holds a branch's write ({parent}) — the environments are one \
         store wearing three names"
    );

    // --- closing a branch closes it -----------------------------------------
    let (code, _) = api.delete("/api/environments?app=swarm&env=branch-0");
    assert_eq!(code, 200, "despawn failed");

    println!(
        "    {BRANCHES} branches, {BRANCHES} stores, one key: each read back its own, and the \
         parent read none of them"
    );
}
