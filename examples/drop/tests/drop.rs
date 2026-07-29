//! E2E for the drop-box (DROP.md) as ONE composed wasm HTTP component on the
//! native Rust host. The presigned-ticket axis: a ticket is the policy answer,
//! the client PUTs bytes against it, and a signed link round-trips the object
//! back out while a tampered signature is refused.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3031";

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

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/comp-host");
    let component = root.join("components/target/upload_drop.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-drop`)");
    assert!(component.exists(), "composed wasm missing (just compose-drop)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "drop")
        .env("CFG_ALLOWED_TYPES", "text/plain,image/png")
        .env("CFG_MAX_SIZE", "1048576")
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
    panic!("drop host did not start");
}

#[test]
fn ticket_upload_and_signed_download() {
    let _host = start_host();

    // a blocked content-type is refused at TICKET time — the policy answer, no
    // bytes involved.
    let (status, _) = json_req("POST", "/api/tickets", Some(json!({"content-type": "application/x-evil", "size": 10})));
    assert_eq!(status, 415, "disallowed type must be rejected at ticket time");

    // an oversized request is refused at ticket time too.
    let (status, _) = json_req("POST", "/api/tickets", Some(json!({"content-type": "text/plain", "size": 99_000_000u64})));
    assert_eq!(status, 413, "oversize must be rejected at ticket time");

    // an allowed type mints a ticket.
    let payload = b"hello, presigned world";
    let (status, t) = json_req("POST", "/api/tickets", Some(json!({"content-type": "text/plain", "size": payload.len()})));
    assert_eq!(status, 201, "ticket: {t}");
    let token = t["token"].as_str().expect("token").to_string();
    assert!(!token.is_empty());

    // redeem the ticket by PUTting the bytes straight to storage.
    let put_url = format!("{}/api/blob/{token}", base());
    let put = ureq::put(&put_url).set("content-type", "text/plain").send_bytes(payload);
    let put = match put {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => panic!("PUT: {e}"),
    };
    assert_eq!(put.status(), 201, "upload should store the bytes");
    let up: Value = serde_json::from_str(&put.into_string().unwrap()).unwrap();
    let id = up["id"].as_str().expect("id").to_string();

    // the object shows up in the listing.
    let (_, list) = json_req("GET", "/api/objects", None);
    assert!(list["objects"].as_array().unwrap().iter().any(|o| o["id"] == id), "object must be listed");

    // get a signed download link.
    let (status, meta) = json_req("GET", &format!("/api/object/{id}"), None);
    assert_eq!(status, 200, "object meta: {meta}");
    let link = meta["download"].as_str().expect("download link").to_string();

    // the signed link round-trips the exact bytes.
    let dl = ureq::get(&format!("{}{link}", base())).call().expect("download");
    assert_eq!(dl.status(), 200);
    let mut got = Vec::new();
    dl.into_reader().read_to_end(&mut got).unwrap();
    assert_eq!(got, payload, "downloaded bytes must match uploaded");

    // a tampered signature is refused.
    let tampered = link.replace("sig=", "sig=x");
    let bad = ureq::get(&format!("{}{tampered}", base())).call();
    let code = match bad {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(c, _)) => c,
        Err(e) => panic!("tampered dl: {e}"),
    };
    assert_eq!(code, 403, "tampered signature must be refused");

    // stats reflect the one stored object.
    let (_, st) = json_req("GET", "/api/stats", None);
    assert_eq!(st["objects"].as_u64().unwrap(), 1, "one object stored: {st}");
    assert_eq!(st["total_bytes"].as_u64().unwrap(), payload.len() as u64, "bytes tallied: {st}");
}

use std::io::Read;
