//! E2E for the scribe collaborative editor (SCRIBE.md) as ONE composed wasm HTTP
//! component (scribe-domain + crdt + record-store + id-generate) on the native
//! Rust host. The subject is CRDT convergence over HTTP:
//!   - concurrent edits to DIFFERENT fields both survive (lwwmap merge),
//!   - concurrent edits to the SAME field resolve by (ts, replica) — and a
//!     late-arriving OLDER edit never clobbers a newer one (the thing a naive
//!     "last request to the server wins" gets wrong),
//!   - a held-open SSE connection sees the merged document live.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3037";

struct HostGuard(Child);
impl Drop for HostGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn base() -> String {
    format!("http://{ADDR}")
}

fn req(method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let url = format!("{}{}", base(), path);
    let r = ureq::request(method, &url);
    let result = match &body {
        Some(b) => r.set("content-type", "application/json").send_string(&b.to_string()),
        None => r.call(),
    };
    let resp = match result {
        Ok(resp) => resp,
        Err(ureq::Error::Status(_, resp)) => resp,
        Err(e) => panic!("{method} {path}: {e}"),
    };
    let status = resp.status();
    (status, serde_json::from_str(&resp.into_string().unwrap_or_default()).unwrap_or(Value::Null))
}

fn op(doc: &str, field: &str, value: &str, ts: u64, replica: &str) -> (u16, Value) {
    req(
        "POST",
        &format!("/api/docs/{doc}/ops"),
        Some(json!({ "field": field, "value": value, "ts": ts, "replica": replica })),
    )
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/vet-host");
    let component = root.join("components/target/scribe_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-scribe`)");
    assert!(component.exists(), "composed wasm missing (just compose-scribe)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "scribe")
        .spawn()
        .expect("spawn vet-host");
    let guard = HostGuard(child);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&base()).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("scribe host did not start");
}

#[test]
fn concurrent_edits_merge_and_stream_live() {
    let _host = start_host();

    // ===== different fields: both edits survive the merge =====================
    let (s, _) = op("readme", "title", "Design spec", 100, "alice");
    assert_eq!(s, 200);
    let (s, d) = op("readme", "body", "Hello world", 100, "bob");
    assert_eq!(s, 200, "{d}");
    let (_, doc) = req("GET", "/api/docs/readme", None);
    assert_eq!(doc["fields"]["title"], "Design spec", "title survived: {doc}");
    assert_eq!(doc["fields"]["body"], "Hello world", "body survived: {doc}");

    // ===== same field: higher (ts, replica) wins, even arriving LATER =========
    op("readme", "title", "Design proposal", 200, "bob"); // newer
    op("readme", "title", "Stale rename", 150, "carol"); // older ts, sent AFTER
    let (_, doc) = req("GET", "/api/docs/readme", None);
    assert_eq!(
        doc["fields"]["title"], "Design proposal",
        "the newer edit must win regardless of arrival order: {doc}"
    );

    // a brand-new doc is empty
    let (_, doc) = req("GET", "/api/docs/empty", None);
    assert_eq!(doc["fields"], json!({}), "unedited doc is empty: {doc}");

    // ===== live SSE: a held-open connection sees a merged edit ===============
    let found = Arc::new(AtomicBool::new(false));
    let f = found.clone();
    let url = format!("{}/api/docs/live/events", base());
    let reader = std::thread::spawn(move || {
        let agent = ureq::AgentBuilder::new().timeout_read(Duration::from_secs(2)).build();
        let Ok(resp) = agent.get(&url).call() else { return };
        let mut buf = BufReader::new(resp.into_reader());
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut line = String::new();
        while Instant::now() < deadline {
            line.clear();
            match buf.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if line.starts_with("data:") && line.contains("live-edit") {
                        f.store(true, Ordering::SeqCst);
                        return;
                    }
                }
                Err(_) => {} // read-timeout tick; ": ping" heartbeats keep us moving
            }
        }
    });

    std::thread::sleep(Duration::from_millis(900));
    let (s, _) = op("live", "title", "live-edit", 300, "dave");
    assert_eq!(s, 200);

    reader.join().unwrap();
    assert!(
        found.load(Ordering::SeqCst),
        "the live SSE connection should have received the merged document as a data: frame"
    );
}
