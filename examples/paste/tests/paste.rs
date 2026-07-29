//! E2E for the paste bin (PASTE.md) as ONE composed wasm HTTP component on the
//! native Rust host. The pure-compute pipeline axis: validate -> redact -> store
//! -> slug, then render on read. The headline property is that PII is masked
//! BEFORE storage — the raw email/card never lands in the record store — and
//! that Markdown renders to sanitized HTML.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3035";

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

fn json_req(method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
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

fn get_text(path: &str) -> (u16, String) {
    let url = format!("{}{}", base(), path);
    let resp = match ureq::get(&url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => panic!("GET {path}: {e}"),
    };
    (resp.status(), resp.into_string().unwrap_or_default())
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/comp-host");
    let component = root.join("components/target/paste_bin.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-paste`)");
    assert!(component.exists(), "composed wasm missing (just compose-paste)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "paste")
        .spawn()
        .expect("spawn comp-host");
    let guard = HostGuard(child);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&base()).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("paste host did not start");
}

#[test]
fn validate_redact_render_and_slug() {
    let _host = start_host();

    // validate: an empty body is rejected (pure-compute validate:schema).
    let (s, v) = json_req("POST", "/api/paste", Some(json!({"body": ""})));
    assert_eq!(s, 422, "empty body must fail validation: {v}");
    assert_eq!(v["error"], "validation_failed");

    // create a paste containing PII + Markdown + a raw <script>.
    let raw = "# Title\n\nReach me at alice@example.com or card 4111 1111 1111 1111.\n\n<script>alert('xss')</script>\n\n**bold** and _em_";
    let (s, p) = json_req("POST", "/api/paste", Some(json!({"title": "My Notes", "body": raw})));
    assert_eq!(s, 201, "create: {p}");
    // two PII findings masked at ingest (email + card).
    assert_eq!(p["redacted"].as_u64().unwrap(), 2, "email + card detected: {p}");
    let id = p["id"].as_str().unwrap().to_string();
    assert_eq!(p["slug"], "my-notes");

    // the RAW stored body has the PII masked — the plaintext email/card never
    // landed in the store.
    let (_, raw_out) = get_text(&format!("/api/raw/{id}"));
    assert!(!raw_out.contains("alice@example.com"), "raw email must be masked, got: {raw_out}");
    assert!(!raw_out.contains("4111 1111 1111 1111"), "raw card must be masked, got: {raw_out}");

    // the rendered view escapes the <script> (safe Markdown -> HTML) and still
    // renders real formatting.
    let (_, view) = json_req("GET", &format!("/api/paste/{id}"), None);
    let html = view["html"].as_str().unwrap();
    assert!(html.contains("&lt;script&gt;"), "raw <script> must be escaped: {html}");
    assert!(!html.contains("<script>"), "no executable <script> in output");
    assert!(html.contains("<h1>"), "markdown heading rendered: {html}");

    // duplicate titles get distinct slugs (slug::uniquify).
    let (_, p2) = json_req("POST", "/api/paste", Some(json!({"title": "My Notes", "body": "second"})));
    assert_eq!(p2["slug"], "my-notes-2", "duplicate title must get a unique slug: {p2}");

    // listing shows both, most-recent visible.
    let (_, list) = json_req("GET", "/api/pastes", None);
    assert!(list["pastes"].as_array().unwrap().len() >= 2, "both pastes listed: {list}");
}
