//! E2E for the mesh resilience playground (MESH.md) as ONE composed wasm HTTP
//! component (mesh-domain + records + resilience + proxy-route) on the native
//! Rust host, in front of the REAL flaky upstream (`src/bin/flaky.rs`).
//!
//! Nothing here is simulated: the guarded call is an actual outgoing HTTP hop
//! through `proxy:route`, and the proof that a tripped breaker sheds load is the
//! upstream's own hit counter NOT moving.
//!
//! What it proves:
//!   * a healthy call succeeds on attempt 1
//!   * a two-request blip is ridden out by retries (attempt 3 succeeds)
//!   * `failure_threshold` failures trip the breaker, and while it is OPEN the
//!     upstream is never dialled (hit count frozen) — 503 with a retry-after
//!     that counts down
//!   * after `open_ms` a half-open probe closes the circuit again
//!   * a response slower than `slo_ms` counts as a FAILURE despite its 200
//!   * an unreachable upstream is a failure; a MISSING ROUTE is not (a config
//!     bug must not trip a breaker)
//!   * backoff actually sleeps (total_ms >= the schedule)

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const HOST: &str = "127.0.0.1:3050";
const UPSTREAM: &str = "127.0.0.1:3051";
/// Nothing listens here — a real connect-refused for the unreachable case.
const DEAD: &str = "127.0.0.1:3052";

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn post(path: &str, body: Value) -> (u16, Value) {
    let r = ureq::post(&format!("http://{HOST}{path}"))
        .set("content-type", "application/json")
        .send_string(&body.to_string());
    match r {
        Ok(resp) => (resp.status(), json_of(resp)),
        Err(ureq::Error::Status(s, resp)) => (s, json_of(resp)),
        Err(e) => panic!("POST {path}: {e}"),
    }
}
fn get(base: &str, path: &str) -> (u16, Value) {
    match ureq::get(&format!("http://{base}{path}")).call() {
        Ok(resp) => (resp.status(), json_of(resp)),
        Err(ureq::Error::Status(s, resp)) => (s, json_of(resp)),
        Err(e) => panic!("GET {path}: {e}"),
    }
}
fn json_of(resp: ureq::Response) -> Value {
    serde_json::from_str(&resp.into_string().unwrap_or_default()).unwrap_or(Value::Null)
}

/// How many times the upstream was actually hit for this tag.
fn upstream_hits(id: &str) -> u64 {
    get(UPSTREAM, &format!("/count?id={id}")).1["hits"].as_u64().unwrap_or(0)
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn start_upstream() -> Kill {
    let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/release/flaky");
    assert!(bin.exists(), "flaky upstream not built: {bin:?} (run `just e2e-mesh`)");
    let child = Command::new(&bin).arg(UPSTREAM).spawn().expect("spawn flaky");
    let guard = Kill(child);
    for _ in 0..100 {
        if ureq::get(&format!("http://{UPSTREAM}/")).call().is_ok() {
            return guard;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("flaky upstream did not start");
}

fn start_host() -> Kill {
    let bin = root().join("host/target/release/vet-host");
    let component = root().join("components/target/mesh_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-mesh`)");
    assert!(component.exists(), "composed wasm missing (just compose-mesh)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", HOST, "--kv", "memory"])
        .env("VET_TENANT", "mesh")
        // the proxy:route table — /upstream is the flaky server, /dead is nothing.
        .env("CFG_ROUTES", format!("/upstream=http://{UPSTREAM}/,/dead=http://{DEAD}/"))
        .spawn()
        .expect("spawn vet-host");
    let guard = Kill(child);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&format!("http://{HOST}/")).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("mesh host did not start");
}

#[test]
fn resilience_patterns() {
    let _up = start_upstream();
    let _host = start_host();

    // ===== healthy: one attempt, circuit stays closed =======================
    let (code, r) = post("/api/call", json!({ "key": "ok", "path": "/upstream/hit?id=ok" }));
    assert_eq!(code, 200, "healthy call: {r}");
    assert_eq!(r["state"], "closed");
    assert_eq!(r["attempts"].as_array().unwrap().len(), 1, "no retries needed: {r}");
    assert_eq!(upstream_hits("ok"), 1, "exactly one upstream request");

    // ===== a blip is ridden out by retries ==================================
    // fail_n=2 -> the first two requests 500, the third succeeds. attempts=4 is
    // enough; base 40ms doubling means the whole thing still finishes fast.
    let (code, r) = post(
        "/api/call",
        json!({ "key": "blip", "path": "/upstream/hit?id=blip&fail_n=2",
                "attempts": 4, "base_ms": 40, "factor_pct": 200, "jitter": false,
                "failure_threshold": 5 }),
    );
    assert_eq!(code, 200, "retries rode out the blip: {r}");
    let tries = r["attempts"].as_array().unwrap();
    assert_eq!(tries.len(), 3, "failed twice, succeeded on the third: {r}");
    assert_eq!(tries[0]["ok"], false);
    assert_eq!(tries[2]["ok"], true);
    assert_eq!(upstream_hits("blip"), 3);
    // backoff really slept: 40ms before attempt 2 + 80ms before attempt 3.
    assert!(r["total_ms"].as_u64().unwrap() >= 120, "backoff waited: {r}");
    assert_eq!(r["state"], "closed", "a success clears the streak");

    // ===== the breaker trips, then sheds without touching the upstream ======
    let trip = |n: u64| {
        post(
            "/api/call",
            json!({ "key": "down", "path": "/upstream/hit?id=down&fail=1",
                    "attempts": 1, "failure_threshold": 2, "open_ms": 1500,
                    "success_threshold": 1, "half_open_probes": 1, "seq": n }),
        )
    };
    let (c1, _) = trip(1);
    assert_eq!(c1, 502, "upstream 500 surfaces as a bad gateway");
    let (c2, r2) = trip(2);
    assert_eq!(c2, 502);
    assert_eq!(r2["state"], "open", "2 failures at threshold 2 -> tripped: {r2}");
    let hits_when_tripped = upstream_hits("down");
    assert_eq!(hits_when_tripped, 2, "one real request per attempt");

    // The circuit is open: the next call must be SHED — no upstream request.
    let (c3, r3) = trip(3);
    assert_eq!(c3, 503, "open circuit fails fast: {r3}");
    assert_eq!(r3["shed"], true);
    assert_eq!(r3["state"], "open");
    assert!(r3["attempts"].as_array().unwrap().is_empty(), "nothing was attempted");
    assert!(r3["retry_after_ms"].as_u64().unwrap() > 0, "counts down to the probe");
    assert_eq!(
        upstream_hits("down"),
        hits_when_tripped,
        "THE POINT: while open, the upstream is never dialled"
    );

    // A read of the circuit reports the same, and does not spend the probe.
    let (_, cv) = get(HOST, "/api/circuit/down");
    assert_eq!(cv["circuit"]["state"], "open");
    assert_eq!(cv["would_admit"], false);
    assert_eq!(cv["stats"]["shed"], 1);
    assert_eq!(cv["stats"]["trips"], 1);

    // ===== half-open: after the cooldown, a probe closes it ================
    std::thread::sleep(Duration::from_millis(1600));
    let (_, cv) = get(HOST, "/api/circuit/down");
    assert_eq!(cv["would_admit"], true, "cooldown elapsed: {cv}");
    // Probe a HEALTHY path this time (the upstream "recovered").
    let (code, r) = post(
        "/api/call",
        json!({ "key": "down", "path": "/upstream/hit?id=probe",
                "attempts": 1, "failure_threshold": 2, "open_ms": 1500, "success_threshold": 1 }),
    );
    assert_eq!(code, 200, "the probe got through: {r}");
    assert_eq!(r["state"], "closed", "one good probe closes it (success_threshold 1)");
    assert_eq!(upstream_hits("probe"), 1);

    // ===== slow is failed: an SLO breach despite a 200 ======================
    let (code, r) = post(
        "/api/call",
        json!({ "key": "slow", "path": "/upstream/hit?id=slow&delay=300",
                "attempts": 1, "slo_ms": 100, "failure_threshold": 1, "open_ms": 5000 }),
    );
    assert_eq!(code, 502, "slow counts as a failure: {r}");
    let t = &r["attempts"][0];
    assert_eq!(t["status"], 200, "the upstream did answer 200");
    assert_eq!(t["ok"], false, "...but too late to count");
    assert!(t["error"].as_str().unwrap().contains("slo"), "{t}");
    assert_eq!(r["state"], "open", "threshold 1 -> tripped by the SLO breach");

    // ===== unreachable is a failure; a missing route is NOT ================
    let (code, r) = post(
        "/api/call",
        json!({ "key": "dead", "path": "/dead/anything", "attempts": 1, "failure_threshold": 1 }),
    );
    assert_eq!(code, 502, "{r}");
    assert!(r["attempts"][0]["error"].as_str().unwrap().contains("unreachable"), "{r}");
    assert_eq!(r["state"], "open", "an unreachable upstream trips the breaker");

    let (code, r) = post(
        "/api/call",
        json!({ "key": "misconfigured", "path": "/nowhere/at/all", "attempts": 1, "failure_threshold": 1 }),
    );
    assert_eq!(code, 502, "{r}");
    assert!(r["error"].as_str().unwrap().contains("no route"), "{r}");
    let (_, cv) = get(HOST, "/api/circuit/misconfigured");
    assert_eq!(cv["circuit"]["state"], "closed", "OUR config bug must not trip a breaker: {cv}");
    assert_eq!(cv["stats"]["attempts"], 0);

    // ===== reset forgets a circuit =========================================
    post("/api/reset", json!({ "key": "slow" }));
    let (_, cv) = get(HOST, "/api/circuit/slow");
    assert_eq!(cv["circuit"]["state"], "closed");
    assert_eq!(cv["stats"]["failed"], 0);

    // The dashboard lists what is left.
    let (_, all) = get(HOST, "/api/circuits");
    let keys: Vec<&str> = all["circuits"].as_array().unwrap().iter().map(|c| c["key"].as_str().unwrap()).collect();
    assert!(keys.contains(&"down") && keys.contains(&"dead") && !keys.contains(&"slow"), "{keys:?}");
}
