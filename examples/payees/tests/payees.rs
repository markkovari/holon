//! E2E for the payees book (docs/apps/PAYEES.md) as ONE composed wasm HTTP component
//! (payees-domain + auth-guard + records + iban) on the native Rust host. Proves
//! IBAN validation end to end: `/verify` parses a valid IBAN and flags a bad one;
//! adding a payee stores a valid IBAN (normalized + country) and rejects a
//! bad-checksum / wrong-length / bad-country one with the reason; and one account
//! can't delete another's payee.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3047";

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

fn req(method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, Value) {
    let url = format!("{}{}", base(), path);
    let mut r = ureq::request(method, &url);
    if let Some(t) = token {
        r = r.set("authorization", &format!("Bearer {t}"));
    }
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

fn signup(email: &str) -> String {
    let (s, _) = req("POST", "/api/register", None, Some(json!({ "email": email, "password": "pw12345678" })));
    assert!(s == 201 || s == 409, "register {email}: {s}");
    let (s, l) = req("POST", "/api/login", None, Some(json!({ "email": email, "password": "pw12345678" })));
    assert_eq!(s, 200, "login {email}: {l}");
    l["access_token"].as_str().unwrap().to_string()
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/comp-host");
    let component = root.join("components/target/payees_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-payees`)");
    assert!(component.exists(), "composed wasm missing (just compose-payees)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "payees")
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
    panic!("payees host did not start");
}

#[test]
fn iban_validation_and_payee_book() {
    let _host = start_host();
    let tok = signup("pay@acme.io");

    // ===== seeded payees ==================================================
    let (_, r) = req("GET", "/api/payees", Some(&tok), None);
    assert_eq!(r["items"].as_array().unwrap().len(), 3, "seeded payees");

    // ===== /verify parses a valid IBAN and flags a bad one ================
    let (_, v) = req("POST", "/api/verify", Some(&tok), Some(json!({ "iban": "NL91 ABNA 0417 1643 00" })));
    assert_eq!(v["valid"], true);
    assert_eq!(v["country"], "NL");
    assert_eq!(v["formatted"], "NL91 ABNA 0417 1643 00");

    let (_, v) = req("POST", "/api/verify", Some(&tok), Some(json!({ "iban": "NL91 ABNA 0417 1643 01" })));
    assert_eq!(v["valid"], false);
    assert!(v["error"].as_str().unwrap().contains("checksum"), "{v}");

    // ===== adding a payee stores a valid IBAN; rejects bad ones ===========
    let (s, p) = req("POST", "/api/payees", Some(&tok), Some(json!({ "name": "Dutch Vendor", "iban": "nl91 abna 0417 1643 00" })));
    assert_eq!(s, 201, "{p}");
    assert_eq!(p["iban"], "NL91ABNA0417164300", "stored normalized");
    assert_eq!(p["country"], "NL");
    let id = p["id"].as_str().unwrap().to_string();

    // bad checksum
    let (s, e) = req("POST", "/api/payees", Some(&tok), Some(json!({ "name": "Typo", "iban": "DE89370400440532013001" })));
    assert_eq!(s, 422);
    assert!(e["error"].as_str().unwrap().contains("checksum"), "{e}");
    // wrong length for the country
    let (s, e) = req("POST", "/api/payees", Some(&tok), Some(json!({ "name": "Short", "iban": "DE8937040044" })));
    assert_eq!(s, 422);
    assert!(e["error"].as_str().unwrap().contains("length"), "{e}");
    // not a country code
    let (s, _) = req("POST", "/api/payees", Some(&tok), Some(json!({ "name": "Nope", "iban": "12 3456 7890" })));
    assert_eq!(s, 422);

    // ===== ownership ======================================================
    let other = signup("other@acme.io");
    assert_eq!(req("DELETE", &format!("/api/payees/{id}"), Some(&other), None).0, 403, "not your payee");
    assert_eq!(req("DELETE", &format!("/api/payees/{id}"), Some(&tok), None).0, 200, "owner deletes");
}
