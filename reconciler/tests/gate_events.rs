//! The `events` gate, as a Rust integration test rather than a bash script.
//!
//! A PROTOTYPE, and an argument with a number attached. `components/events-domain/
//! e2e-events.sh` asserts exactly what this does; the difference is what a machine
//! must have installed before it can run.
//!
//! ## Why this exists
//!
//! The gates are being distributed across machines and scheduled between actors, and
//! every external tool a gate needs is a way one worker can differ from another. A
//! gate run today needs THIRTEEN of them — `curl` (207 references), `python3` (156),
//! `grep`, `cargo`, `sed`, `MailHog`, `mktemp`, `go`, `wasm-tools`, `awk`, `date`,
//! `base64`, `docker` — each at a compatible version, on every worker, or a gate
//! fails on one machine and passes on another for a reason that is not the code.
//!
//! This needs `comp-host` and a composed `.wasm`. Both are artifacts THIS REPOSITORY
//! BUILDS, which is the difference that matters for scheduling: they can be shipped
//! to a worker, and `cargo test --no-run` turns this file into one more binary that
//! can be shipped beside them.
//!
//! ## What is deliberately unchanged
//!
//! Every failure message, verbatim. ADR-0088 says a gate's output IS the next prompt,
//! and these sentences were written carefully — the comment in the bash version about
//! the `?state=open` assertion records a round where a gate GUESSED a cause, reported
//! the guess as a finding, and sent a repair to fix a query that was working. A port
//! that rewords them is a port that changes what the loop reads.
//!
//! ## What it costs
//!
//! One host process for the whole gate rather than one plus thirty-one short-lived
//! ones. Instrumented, `e2e-events.sh` spawns 26 `curl` and 5 `python3`; this spawns
//! `comp-host` and nothing else.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

/// A port nothing else in the suite uses. The bash gate randomises in 20000..40000;
/// a fixed one is fine here because `cargo test` runs one integration binary at a
/// time per target and this is the only test in it.
const ADDR: &str = "127.0.0.1:38121";

struct HostGuard(Child);
impl Drop for HostGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}
fn base() -> String {
    format!("http://{ADDR}")
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().unwrap()
}

/// (status, body-as-text). A non-2xx is a value, never a panic: most of this gate is
/// asserting that a particular request is REFUSED with a particular code.
fn send(method: &str, path: &str, token: Option<&str>, body: Option<(&str, Vec<u8>)>) -> (u16, String) {
    let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
    let mut r = client().request(m, format!("{}{}", base(), path));
    if let Some(t) = token {
        r = r.header("authorization", format!("Bearer {t}"));
    }
    if let Some((ct, bytes)) = body {
        r = r.header("content-type", ct).body(bytes);
    }
    let resp = r.send().unwrap_or_else(|e| panic!("{method} {path}: transport error: {e}"));
    let status = resp.status().as_u16();
    (status, resp.text().unwrap_or_default())
}

fn json_req(method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, String) {
    send(method, path, token, body.map(|b| ("application/json", b.to_string().into_bytes())))
}

/// `field` in the bash gate: one top-level key, empty when absent.
fn field(body: &str, key: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get(key).cloned())
        .map(|v| match v {
            Value::String(s) => s,
            Value::Null => String::new(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

fn stored(kind: &str, id: &str) -> String {
    send("GET", &format!("/test/{kind}/{id}"), None, None).1
}

fn start_host() -> HostGuard {
    let root = root();
    let bin = root.join("host/target/release/comp-host");
    let component = root.join("components/target/events_domain.composed.wasm");
    assert!(bin.exists(), "no comp-host at {bin:?} — the gate cannot run what it built");
    assert!(
        component.exists(),
        "no composed events-domain at {component:?} — run `just compose-events`"
    );

    let child = Command::new(&bin)
        .args([
            "--app", "events",
            "--config", "default-tenant=events",
            "--config", "allow-test-routes=true",
            "--config", "allowed-types=image/png,image/jpeg,image/webp",
            "--config", "max-size=2097152",
            "--component", component.to_str().unwrap(),
            "--addr", ADDR,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn comp-host");
    let guard = HostGuard(child);
    for _ in 0..200 {
        if client().get(format!("{}/health", base())).send().is_ok() {
            return guard;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("comp-host never answered /health on {ADDR}");
}

/// A real PNG, byte for byte — the same bytes the bash gate writes with `printf`.
/// The round trip is the point: the router used to read every body through
/// `from_utf8_lossy`, which turns each byte that is not valid UTF-8 into U+FFFD, so
/// the upload succeeds and stores something that is not an image.
const PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D', b'R',
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, b'I', b'D', b'A', b'T', b'x', 0x9c, b'c', 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xae,
    b'B', b'`', 0x82,
];

#[test]
fn events_created_read_amended_and_cancelled() {
    // A loud skip rather than a failure, matching the rest of this suite: the gate
    // needs an artifact a build produces, and a checkout that has not built one has
    // not broken anything.
    let composed = root().join("components/target/events_domain.composed.wasm");
    if !composed.exists() || !root().join("host/target/release/comp-host").exists() {
        eprintln!(
            "SKIPPED: needs `just compose-events` and a built comp-host — \
             the two artifacts this repository produces, and the only two things \
             this gate needs on a machine"
        );
        return;
    }
    let _host = start_host();

    // --- the fixture -------------------------------------------------------------
    let (_, raw) = json_req("POST", "/test/seed", None, Some(json!({})));
    let seed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let tok = |who: &str| seed["tokens"][who]["token"].as_str().unwrap_or_default().to_string();
    let (organizer, attendee) = (tok("organizer"), tok("attendee"));
    assert!(
        !seed["event_id"].as_str().unwrap_or_default().is_empty()
            && !organizer.is_empty()
            && !attendee.is_empty()
            && !tok("other").is_empty(),
        "the fixture did not come back with an event and three tokens: {raw}"
    );

    // --- anonymous callers are refused --------------------------------------------
    let (code, _) = json_req("POST", "/api/events", None,
        Some(json!({"title":"x","starts_at":"2026-09-01T18:00:00Z","capacity":5})));
    assert!(code == 401 || code == 403, "an unauthenticated POST /api/events must be refused (got {code})");

    // --- an organizer creates one --------------------------------------------------
    let (_, new) = json_req("POST", "/api/events", Some(&organizer),
        Some(json!({"title":"Wasm Night","starts_at":"2026-10-01T18:00:00Z","capacity":50})));
    let new_id = field(&new, "id");
    assert!(!new_id.is_empty(), "POST /api/events returned no id: {new}");

    let doc = stored("events", &new_id);
    for want in ["\"title\"", "\"starts_at\"", "\"capacity\"", "\"organizer\"", "\"state\""] {
        assert!(doc.contains(want), "the stored event is missing {want} — CONTRACT.md fixes the shape: {doc}");
    }
    assert!(
        doc.contains("\"state\":\"open\"") || doc.contains("\"state\": \"open\""),
        "a new event must be state=open: {doc}"
    );

    // --- an attendee may not create ------------------------------------------------
    let (code, _) = json_req("POST", "/api/events", Some(&attendee),
        Some(json!({"title":"nope","starts_at":"2026-10-01T18:00:00Z","capacity":5})));
    assert_eq!(code, 403, "an attendee has no event:write and must be refused");

    // --- validation ----------------------------------------------------------------
    let (code, _) = json_req("POST", "/api/events", Some(&organizer),
        Some(json!({"starts_at":"2026-10-01T18:00:00Z","capacity":5})));
    assert_eq!(code, 400, "an event with no title is a 400");
    let (code, _) = json_req("POST", "/api/events", Some(&organizer),
        Some(json!({"title":"x","starts_at":"2026-10-01T18:00:00Z","capacity":0})));
    assert_eq!(code, 400, "capacity below 1 is a 400");

    // --- reading it back ------------------------------------------------------------
    let (_, one) = json_req("GET", &format!("/api/events/{new_id}"), Some(&attendee), None);
    for want in ["\"claimed\"", "\"remaining\""] {
        assert!(one.contains(want), "GET /api/events/{{id}} must report {want} from quota:meter's peek: {one}");
    }
    assert_eq!(field(&one, "remaining"), "50", "a brand-new event with capacity 50 has 50 remaining");

    // Two separate claims with two separate messages ON PURPOSE — see the note in
    // e2e-events.sh: an earlier version asserted only that the id appeared, and when
    // it did not it reported a GUESS about the cause as a finding. It was wrong, and
    // a repair round was sent to fix a query that worked.
    let (_, list) = json_req("GET", "/api/events?state=open", Some(&attendee), None);
    assert!(
        list.contains("\"Wasm Night, moved\"") || list.contains("\"Wasm Night\""),
        "?state=open did not return the open event just created. If other open events came back \
         but not this one, the filter is matching the wrong value — record-store indexes the \
         SERIALISED form, so \"open\" with quotes. Body: {list}"
    );
    assert!(
        list.contains(&new_id),
        "the events list came back without any id on its entries, so nothing in it can be fetched \
         or amended — CONTRACT.md says every entry carries its id. Body: {list}"
    );

    let (code, _) = json_req("GET", "/api/events/does-not-exist", Some(&attendee), None);
    assert_eq!(code, 404, "an unknown event is a 404");

    // --- only the owning organizer may amend ----------------------------------------
    let (code, _) = json_req("PATCH", &format!("/api/events/{new_id}"), Some(&organizer),
        Some(json!({"title":"Wasm Night, moved"})));
    assert_eq!(code, 200, "the organizer who created the event must be able to PATCH it");

    // --- cancelling is soft ----------------------------------------------------------
    let (code, _) = json_req("DELETE", &format!("/api/events/{new_id}"), Some(&organizer), None);
    assert_eq!(code, 204, "DELETE must answer 204");
    let doc = stored("events", &new_id);
    assert!(
        doc.contains("\"state\":\"cancelled\"") || doc.contains("\"state\": \"cancelled\""),
        "DELETE is a SOFT delete — the document stays and state becomes cancelled: {doc}"
    );

    // --- an optional description ------------------------------------------------------
    let (_, with) = json_req("POST", "/api/events", Some(&organizer), Some(json!({
        "title":"Described","starts_at":"2026-10-02T18:00:00Z","capacity":9,
        "description":"An evening about nothing in particular."})));
    let wid = field(&with, "id");
    assert!(
        stored("events", &wid).contains("An evening about nothing in particular."),
        "description was dropped on create: {}", stored("events", &wid)
    );
    // Absent, not empty, when it is not given — a caller reading "" cannot tell
    // "nobody wrote one" from "somebody cleared it".
    assert!(
        !stored("events", &new_id).contains("\"description\""),
        "an event created without a description must not carry the key"
    );
    let (code, _) = json_req("PATCH", &format!("/api/events/{wid}"), Some(&organizer),
        Some(json!({"description": Value::Null})));
    assert_eq!(code, 200, "clearing a description is a PATCH with null");
    assert!(
        !stored("events", &wid).contains("\"description\""),
        "PATCH null must REMOVE the key, not blank it: {}", stored("events", &wid)
    );

    // --- an optional poster -------------------------------------------------------------
    let (code, _) = send("POST", &format!("/api/events/{wid}/image"), Some(&organizer),
        Some(("image/png", PNG.to_vec())));
    assert_eq!(code, 201, "uploading a PNG poster must be 201");

    let got = client()
        .get(format!("{}/api/events/{wid}/image", base()))
        .send()
        .expect("fetch the poster");
    let content_type =
        got.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let bytes = got.bytes().expect("read the poster").to_vec();
    assert_eq!(
        bytes, PNG,
        "the poster did not survive the round trip byte for byte — a body read as a lossy string \
         is not an image"
    );
    assert!(content_type.starts_with("image/png"), "the poster came back as '{content_type}', not image/png");

    // What may be uploaded is upload-policy's answer, not this component's.
    let (code, _) = send("POST", &format!("/api/events/{wid}/image"), Some(&organizer),
        Some(("text/plain", b"not an image".to_vec())));
    assert_eq!(code, 415, "a text/plain poster must be refused by upload:policy");

    let (code, _) = json_req("GET", &format!("/api/events/{new_id}/image"), Some(&attendee), None);
    assert_eq!(code, 404, "an event with no poster is a 404, not an empty 200");
}
