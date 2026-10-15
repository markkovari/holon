//! Environments under load: how wide, how deep, and what breaks first.
//!
//! `environments.rs` proved ONE environment spins up and closes down. A graph loop asks more: a
//! generation is eight or twenty branches at once, and a search is branches of branches.
//!
//! Inference is scripted with a **realistic delay**: a microsecond mock would measure the harness,
//! while real latency decides how many branches are in flight and whether anything queues.
//!
//! EXPLORATORY: numbers are reported, not asserted (a threshold is a fact about this laptop). What is
//! asserted must hold at any scale: **no two environments ever share a store.**
//!
//! Run with `--ignored`; minutes long.

use std::time::{Duration, Instant};

use comp_reconciler::fleet::Fleet;
use serde_json::{json, Value};

mod harness;
use harness::composed_gate;

struct Api {
    base: String,
    http: reqwest::blocking::Client,
    token: String,
}

impl Api {
    fn new(base: String) -> Self {
        let http =
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build().unwrap();
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline {
            if http.get(&base).send().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let mut me = Self { base, http, token: String::new() };
        let body = json!({ "email": "ada@stress.test", "password": "password123" });
        let _ = me.post_raw("/api/register", body.clone());
        let v = me.post_raw("/api/login", body);
        me.token = v["token"].as_str().unwrap_or_default().to_string();
        assert!(!me.token.is_empty(), "could not log in: {v}");
        me
    }

    fn post_raw(&self, path: &str, body: Value) -> Value {
        self.http
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .ok()
            .and_then(|r| r.json().ok())
            .unwrap_or(Value::Null)
    }

    fn post(&self, path: &str, body: Value) -> (u16, Value) {
        match self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
        {
            Ok(r) => (r.status().as_u16(), r.json().unwrap_or(Value::Null)),
            Err(e) => (0, Value::String(format!("transport: {e}"))),
        }
    }

    fn delete(&self, path: &str) -> (u16, Value) {
        match self.http.delete(format!("{}{path}", self.base)).bearer_auth(&self.token).send() {
            Ok(r) => (r.status().as_u16(), r.json().unwrap_or(Value::Null)),
            Err(e) => (0, Value::String(format!("transport: {e}"))),
        }
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

/// Deploy one app and get it running. Returns its name.
fn deploy_parent(api: &Api, fleet: &Fleet, name: &str) -> String {
    let wasm = composed_gate();
    assert!(matches!(api.upload("gate", wasm), 200 | 201), "upload failed");
    let (code, dep) = api
        .post("/api/deployments", json!({ "name": name, "nodes": [{"id": "gate"}], "edges": [] }));
    assert_eq!(code, 201, "deploy failed: {dep}");
    let id = dep["id"].as_str().unwrap().to_string();

    let deadline = Instant::now() + Duration::from_secs(180);
    let mut saved = false;
    let mut why = Value::Null;
    while Instant::now() < deadline && !saved {
        let (code, body) = api.post(&format!("/api/deployments/{id}/save"), json!({}));
        saved = code == 200;
        why = body;
        if !saved {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    assert!(saved, "the deployment never became saveable: {why}\n{}", fleet.reconciler_log());
    assert!(
        wait_for(fleet, &format!("started ada/{name}/"), Duration::from_secs(180)),
        "the parent never started:\n{}",
        fleet.node_log("n1")
    );
    name.to_string()
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

/// Every app the node currently holds, read from its ledger. The ledger is a MAP keyed by instance id
/// (`<tenant>/<app>/<component>`), not a list; reading it as a list yields an answer
/// indistinguishable from "nothing is running".
fn running_apps(fleet: &Fleet) -> Vec<String> {
    let path = fleet.state_dir().join("n1/instances.json");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("no ledger at {} ({e}) — nothing can be measured", path.display())
    });
    let v: Value = serde_json::from_str(&raw).expect("the ledger should be JSON");
    let obj = v.as_object().unwrap_or_else(|| {
        panic!("the ledger is not an object keyed by instance id — it is {raw}")
    });
    let mut out: Vec<String> =
        obj.values().filter_map(|i| i["app"].as_str().map(str::to_string)).collect();
    out.sort();
    out.dedup();
    out
}

/// BREADTH: a generation of branches, all at once.
/// The composed `gate-domain`, derived when nobody has run `cargo xtask compose gate`, as
/// `Fleet::start` does (reading the hand-composed path directly failed with a bare `NotFound`, and
/// cargo stopping at the first failing binary hid the next suite).

#[test]
#[ignore = "minutes long; run with --ignored"]
fn a_generation_of_branches_spins_up_together() {
    const WIDTH: usize = 8;

    let fleet = Fleet::start_with_platform("stress-wide", 1);
    let api = Api::new(fleet.platform_url());
    let parent = deploy_parent(&api, &fleet, "wide");

    // Fired concurrently, as a generation arrives; settling each first would test nothing about contention.
    let started = Instant::now();
    let results: Vec<(usize, u16, Value)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..WIDTH)
            .map(|i| {
                let api = &api;
                let parent = parent.clone();
                s.spawn(move || {
                    let (code, body) = api.post(
                        "/api/environments",
                        json!({ "app": parent, "env": format!("b{i}") }),
                    );
                    (i, code, body)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let accepted: Vec<&(usize, u16, Value)> =
        results.iter().filter(|(_, c, _)| *c == 201).collect();
    let spawn_time = started.elapsed();

    println!("    spawn: {}/{WIDTH} accepted in {:.1}s", accepted.len(), spawn_time.as_secs_f64());
    for (i, code, body) in results.iter().filter(|(_, c, _)| *c != 201) {
        println!("      b{i} refused {code}: {body}");
    }

    let converge = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut up: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        let apps = running_apps(&fleet);
        up = apps.iter().filter(|a| a.starts_with("wide-env-")).cloned().collect();
        if up.len() >= accepted.len() {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    println!(
        "    converged: {}/{} running after {:.1}s",
        up.len(),
        accepted.len(),
        converge.elapsed().as_secs_f64()
    );

    // Must hold at ANY width: distinct environments, distinct apps (a duplicate is two branches in one store).
    let mut sorted = up.clone();
    sorted.sort();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(before, sorted.len(), "two branches share an app name: {up:?}");

    // The parent is still there: the other thing a generation must not break.
    assert!(
        running_apps(&fleet).iter().any(|a| a == "wide"),
        "spawning a generation took the parent down: {:?}",
        running_apps(&fleet)
    );

    assert!(
        !up.is_empty(),
        "not one branch of {WIDTH} converged — this is a ceiling worth knowing about:\n{}",
        fleet.reconciler_log()
    );
    if up.len() < accepted.len() {
        println!(
            "    NOTE: {} accepted but only {} converged within 300s — a real ceiling",
            accepted.len(),
            up.len()
        );
    }
}

/// DEPTH: branches of branches, and where that stops. A tree search explores FROM a promising branch.
/// Asserts only what must hold at whatever depth is reached.
///
/// It found the ceiling on its first run: `spawn_environment` writes a REVISIONS record but never a
/// DEPLOYMENTS one, and the parent lookup only searched deployments (second level: `404 no deployment`).
/// The lookup now falls back to the newest revision of that NAME when it carries an `environment`;
/// this test says whether it stayed fixed. Sibling environments also used to truncate to one bucket and
/// share a store (fixed in `host/src/tenant.rs`).
#[test]
#[ignore = "minutes long; run with --ignored"]
fn branches_of_branches_find_the_depth_ceiling() {
    const WANTED: usize = 4;

    let fleet = Fleet::start_with_platform("stress-deep", 1);
    let api = Api::new(fleet.platform_url());
    let mut current = deploy_parent(&api, &fleet, "deep");

    let mut chain = vec![current.clone()];
    let mut ceiling: Option<String> = None;

    for level in 0..WANTED {
        let started = Instant::now();
        let (code, body) = api.post("/api/environments", json!({ "app": current, "env": "n" }));
        if code != 201 {
            ceiling = Some(format!("depth {} refused {code}: {body}", level + 1));
            break;
        }
        let derived = body["app"].as_str().unwrap_or_default().to_string();
        assert!(!derived.is_empty(), "accepted with no derived name at depth {level}: {body}");

        if !wait_for(&fleet, &format!("started ada/{derived}/"), Duration::from_secs(240)) {
            ceiling = Some(format!("depth {} accepted but never started", level + 1));
            break;
        }
        println!(
            "    depth {}: {derived} started in {:.1}s",
            level + 1,
            started.elapsed().as_secs_f64()
        );
        chain.push(derived.clone());
        current = derived;
    }

    println!("    chain: {chain:?}");
    match &ceiling {
        Some(why) => println!("    CEILING: {why}"),
        None => println!("    no ceiling found within {WANTED} levels"),
    }

    assert!(chain.len() >= 2, "not one level of nesting worked:\n{}", fleet.reconciler_log());

    let mut names = chain.clone();
    names.sort();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "an ancestor and a descendant share a name: {chain:?}");

    // Every generation runs BESIDE the ones before it: a nested branch evicting its parent would make
    // tree search impossible.
    let apps = running_apps(&fleet);
    for name in &chain {
        assert!(
            apps.iter().any(|a| a == name),
            "{name} is not running — a nested branch replaced its parent rather than \
             joining it: {apps:?}"
        );
    }
    println!("    {} generations running side by side", chain.len());

    // --- closing a branch closes what grew out of it ---
    // A descendant left behind is an app nobody can name: its parent is gone, so nothing lists it and
    // no despawn reaches it.
    if chain.len() >= 3 {
        // Close the FIRST environment; everything below it must go too.
        let (code, body) = api.delete("/api/environments?app=deep&env=n");
        assert_eq!(code, 200, "despawn failed: {body}");
        let closed = body["closed"].as_array().cloned().unwrap_or_default();
        println!("    despawned deep-env-n, closing {} generations", closed.len());
        assert_eq!(
            closed.len(),
            chain.len() - 1,
            "closing a branch must close its descendants — {} of {} went: {body}",
            closed.len(),
            chain.len() - 1
        );

        let deadline = Instant::now() + Duration::from_secs(180);
        let mut left = Vec::new();
        while Instant::now() < deadline {
            left =
                running_apps(&fleet).into_iter().filter(|a| a.starts_with("deep-env-")).collect();
            if left.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        assert!(
            left.is_empty(),
            "descendants are still running after their ancestor closed: {left:?}"
        );
        assert!(
            running_apps(&fleet).iter().any(|a| a == "deep"),
            "the cascade took the root deployment with it"
        );
        println!("    the whole subtree is gone, the root is untouched");
    }

    // The ceiling is REPORTED, not asserted: baking the depth limit in would make fixing it a test
    // failure. Only that it is at least one (environments work at all) is asserted.
    if let Some(why) = ceiling {
        println!(
            "    NOTE: depth is limited to {}. {why}\n\
             \x20         An environment is written as a revision and not as a deployment, \
             so it cannot be looked up as a parent.",
            chain.len() - 1
        );
    }
}
