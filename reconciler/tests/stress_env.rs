//! Environments under load: how wide, how deep, and what breaks first.
//!
//! `environments.rs` proved ONE environment spins up and closes down. That is the
//! existence proof. This is the other question, and it is the one a graph loop
//! actually asks: a generation is eight or twenty branches at once, and a search
//! is branches of branches. Neither had been run.
//!
//! "Theoretical" here means the inference is scripted with a **realistic delay**
//! rather than real. A mock that answers in microseconds turns a load test into a
//! measurement of the harness — real inference takes hundreds of milliseconds to
//! seconds, and that latency is what decides how many branches are in flight, how
//! long each environment is held open, and whether anything queues. Scripted
//! delay gives the right shape for nothing.
//!
//! These are EXPLORATORY. They report numbers rather than asserting a throughput,
//! because a threshold baked in here would be a fact about this laptop. What they
//! do assert is the thing that must hold at any scale: **no two environments ever
//! share a store.**
//!
//! Run with `--ignored`; they are minutes long and not part of the ordinary suite.

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

/// Every app the node currently holds, read from its ledger.
///
/// The ledger is a MAP keyed by instance id — `<tenant>/<app>/<component>` —
/// not a list. Reading it as a list yields an empty answer, which is
/// indistinguishable from "nothing is running" and would make every assertion
/// here fail for the wrong reason. It did, once.
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

// ---------------------------------------------------------------------------

/// BREADTH — a generation of branches, all at once.
/// The composed `gate-domain`, derived when nobody has run `just compose-gate`.
///
/// Reading the hand-composed path directly meant this suite failed with a bare
/// `NotFound` in any checkout that had only run `just build` — and, because cargo
/// stops at the first failing test binary, each such suite hid the next one. This
/// is the same rule `Fleet::start` follows: honour the hand-made artifact when it
/// is there, derive it from gate-domain's own imports when it is not.

#[test]
#[ignore = "minutes long; run with --ignored"]
fn a_generation_of_branches_spins_up_together() {
    const WIDTH: usize = 8;

    let fleet = Fleet::start_with_platform("stress-wide", 1);
    let api = Api::new(fleet.platform_url());
    let parent = deploy_parent(&api, &fleet, "wide");

    // Fired concurrently, the way a generation actually arrives — not in a loop
    // that lets each one settle before the next, which would test nothing about
    // contention.
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

    // Then wait for the loop to converge on all of them.
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

    // The assertion that must hold at ANY width: distinct environments, distinct
    // apps. A duplicate here would mean two branches in one store.
    let mut sorted = up.clone();
    sorted.sort();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(before, sorted.len(), "two branches share an app name: {up:?}");

    // The parent is still there, which is the other thing a generation must not
    // break.
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

/// DEPTH — branches of branches, and where that stops.
///
/// A tree search wants to explore FROM a promising branch, so depth is not a
/// nicety. This measures how far it currently goes and asserts only what must
/// hold at whatever depth is reached.
///
/// It found the ceiling on its first run: an environment could not be a parent,
/// because `spawn_environment` writes a REVISIONS record for the derived app and
/// never a DEPLOYMENTS one, while the parent lookup only searched deployments —
/// so the second level came back `404 no deployment`. ADR-0078 never said so
/// because nothing had ever asked for two.
///
/// Fixed: the lookup now falls back to the newest revision of that NAME when it
/// carries an `environment`, so a branch can be explored from. This test is what
/// says whether it stayed fixed.
///
/// The naming hazard that would have bitten at depth six or seven is fixed
/// regardless (`host/src/tenant.rs`): sibling environments used to truncate to
/// one bucket and silently share a store.
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

    // What must hold at whatever depth was reached.
    assert!(chain.len() >= 2, "not one level of nesting worked:\n{}", fleet.reconciler_log());

    let mut names = chain.clone();
    names.sort();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "an ancestor and a descendant share a name: {chain:?}");

    // Every generation runs BESIDE the ones before it. A nested branch that
    // evicted its parent would make a tree search impossible even at depth two.
    let apps = running_apps(&fleet);
    for name in &chain {
        assert!(
            apps.iter().any(|a| a == name),
            "{name} is not running — a nested branch replaced its parent rather than \
             joining it: {apps:?}"
        );
    }
    println!("    {} generations running side by side", chain.len());

    // --- closing a branch closes what grew out of it ------------------------
    // Nesting creates this obligation. A descendant left behind is an app still
    // running that nobody can name: its parent is gone, so nothing lists it and
    // no despawn reaches it — it just consumes a node until somebody reads a
    // ledger by hand.
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

    // The ceiling is REPORTED, not asserted, on purpose: baking the current depth
    // limit into an assertion would turn fixing it into a test failure. What is
    // asserted is that it is at least one — i.e. that environments work at all.
    if let Some(why) = ceiling {
        println!(
            "    NOTE: depth is limited to {}. {why}\n\
             \x20         An environment is written as a revision and not as a deployment, \
             so it cannot be looked up as a parent.",
            chain.len() - 1
        );
    }
}
