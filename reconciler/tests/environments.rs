//! Spin an environment up, and close it down again, on a real fleet.
//!
//! ADR-0078 proved only that desired state changed; this proves a spawned environment actually STARTS
//! somewhere with its own store and a despawned one actually STOPS. It runs the real `platform-domain`
//! as the control plane, since environments are a platform feature `comp-stub` does not know.

use std::time::Duration;

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
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().unwrap();
        // The control plane is a component like any other, so it comes up with its host: poll, do not sleep.
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        while std::time::Instant::now() < deadline {
            if http.get(&base).send().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let mut me = Self { base, http, token: String::new() };
        let body = json!({ "email": "ada@env.test", "password": "password123" });
        let _ = me.post_raw("/api/register", body.clone());
        let v = me.post_raw("/api/login", body);
        me.token = v["token"].as_str().unwrap_or_default().to_string();
        assert!(!me.token.is_empty(), "could not log in to the control plane: {v}");
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
        let r = self
            .http
            .delete(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .send()
            .unwrap();
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

/// Poll a node's log until it says something, so the test never sleeps on a number (convergence is a
/// reconcile interval away and the interval is a knob).
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

/// Is `app` in the node's ledger, the list of start commands it has accepted and persists so a reboot
/// is not a data-loss event (ADR-0022)? The stop path writes no log line, so the ledger answers "is it gone".
fn ledger(fleet: &Fleet) -> String {
    let p = fleet.state_dir().join("n1/instances.json");
    std::fs::read_to_string(&p).unwrap_or_else(|e| {
        // A missing ledger would make every "is it gone" check pass for the wrong reason.
        let listing: Vec<String> = std::fs::read_dir(fleet.state_dir())
            .map(|d| {
                d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into()).collect()
            })
            .unwrap_or_default();
        panic!("no ledger at {} ({e}) — state dir holds {listing:?}", p.display())
    })
}

fn in_ledger(fleet: &Fleet, app: &str) -> bool {
    ledger(fleet).contains(app)
}

fn wait_until_gone(fleet: &Fleet, app: &str, within: Duration) -> bool {
    let deadline = std::time::Instant::now() + within;
    while std::time::Instant::now() < deadline {
        if !in_ledger(fleet, app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// The composed `gate-domain`, derived when nobody has run `cargo xtask compose gate`, as `Fleet::start`
/// does (reading the hand-composed path directly failed with a bare `NotFound`).

#[test]
fn an_environment_spins_up_and_closes_down() {
    let fleet = Fleet::start_with_platform("envs", 1);
    let api = Api::new(fleet.platform_url());

    // A one-component app, deployed as a person would. The COMPOSED gate (records and shaper inside), so
    // this is about environments, not wiring (`crossnode.rs` covers the linked case).
    let wasm = composed_gate();
    assert!(matches!(api.upload("gate", wasm), 200 | 201), "upload failed");
    let (code, dep) = api.post(
        "/api/deployments",
        json!({ "name": "graph", "nodes": [{"id": "gate"}], "edges": [] }),
    );
    assert_eq!(code, 201, "deploy failed: {dep}");
    let id = dep["id"].as_str().unwrap().to_string();

    // The first save composes and stages; the reconciler distributes; the second save records the
    // revision. Polled, since distribution is the loop's job.
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let mut saved = false;
    let mut why = Value::Null;
    while std::time::Instant::now() < deadline && !saved {
        let (code, body) = api.post(&format!("/api/deployments/{id}/save"), json!({}));
        saved = code == 200;
        why = body;
        if !saved {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    assert!(
        saved,
        "the deployment never became saveable — last answer: {why}\n--- reconciler ---\n{}",
        fleet.reconciler_log()
    );

    // The parent has to be running before an environment means anything.
    assert!(
        wait_for(&fleet, "started ada/graph/", Duration::from_secs(120)),
        "the parent app never started:\n--- n1 ---\n{}\n--- reconciler ---\n{}",
        fleet.node_log("n1"),
        fleet.reconciler_log()
    );

    let (code, spawned) = api.post("/api/environments", json!({ "app": "graph", "env": "node-7" }));
    assert_eq!(code, 201, "spawn failed: {spawned}");
    assert_eq!(spawned["app"], json!("graph-env-node-7"));

    assert!(
        wait_for(&fleet, "started ada/graph-env-node-7/", Duration::from_secs(120)),
        "the environment never started — desired state changed and the loop did \
         not converge, which is the half ADR-0078 asserted and never proved:\n{}",
        fleet.node_log("n1")
    );

    // Its own store makes it a parallel environment, not a second copy of one (ADR-0023: the bucket is
    // named after the app).
    let n1 = fleet.node_log("n1");
    assert!(
        n1.contains("ada/graph-env-node-7/") && n1.contains("ada/graph/"),
        "both the parent and the environment should be running here:\n{n1}"
    );

    let (code, removed) = api.delete("/api/environments?app=graph&env=node-7");
    assert_eq!(code, 200, "despawn failed: {removed}");

    assert!(
        in_ledger(&fleet, "graph-env-node-7"),
        "the environment should still be in the node's ledger right after despawn — \
         if it is not, this assertion is not measuring the reconciler"
    );
    assert!(
        wait_until_gone(&fleet, "graph-env-node-7", Duration::from_secs(120)),
        "the environment was despawned and the node still holds it — an \
         environment nobody wants must not keep running:\n{}",
        ledger(&fleet)
    );

    // And the parent is untouched by its child going away. The ledger is pretty-printed, so the needle
    // carries the space; the closing quote keeps `"app": "graph"` from matching `"app": "graph-env-node-7"`.
    assert!(
        in_ledger(&fleet, "\"app\": \"graph\""),
        "closing an environment took the parent with it:\n{}",
        ledger(&fleet)
    );
    println!("    spun up, ran beside its parent, and closed down");
}
