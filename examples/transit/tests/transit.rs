//! E2E for the transit ticketing app (TRANSIT.md) as ONE composed wasm HTTP
//! component (transit-domain + auth-guard + records + qr + lock-mutex) on the
//! native Rust host. Proves the capability model: a rider buys fares; a
//! validator validates — a single ticket is consumed by ONE scan (a second is
//! rejected); a duration ticket activates with a remaining window; CONCURRENT
//! scans of one single ticket accept exactly once (the lock:mutex single-use
//! guarantee); a fabricated code is rejected; and a ticket renders a valid QR.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3042";

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

fn get_text(path: &str, token: &str) -> (u16, String, String) {
    let r = ureq::get(&format!("{}{}", base(), path)).set("authorization", &format!("Bearer {token}")).call();
    match r {
        Ok(resp) => {
            let ct = resp.header("content-type").unwrap_or("").to_string();
            (200, ct, resp.into_string().unwrap_or_default())
        }
        Err(ureq::Error::Status(s, resp)) => (s, String::new(), resp.into_string().unwrap_or_default()),
        Err(e) => panic!("GET {path}: {e}"),
    }
}

fn signup(email: &str, role: &str) -> String {
    let (s, _) = req("POST", "/api/register", None, Some(json!({ "email": email, "password": "pw12345678", "role": role })));
    assert!(s == 201 || s == 409, "register {email}: {s}");
    let (s, l) = req("POST", "/api/login", None, Some(json!({ "email": email, "password": "pw12345678" })));
    assert_eq!(s, 200, "login {email}: {l}");
    l["access_token"].as_str().unwrap().to_string()
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/vet-host");
    let component = root.join("components/target/transit_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-transit`)");
    assert!(component.exists(), "composed wasm missing (just compose-transit)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "transit")
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
    panic!("transit host did not start");
}

fn buy(tok: &str, fare: &str) -> String {
    let (s, r) = req("POST", "/api/tickets", Some(tok), Some(json!({ "fare": fare })));
    assert_eq!(s, 201, "buy {fare}: {r}");
    r["id"].as_str().unwrap().to_string()
}
fn validate(tok: &str, code: &str) -> Value {
    let (s, r) = req("POST", "/api/validate", Some(tok), Some(json!({ "code": code })));
    assert_eq!(s, 200, "validate: {r}");
    r
}

#[test]
fn ticketing_capability_and_single_use() {
    let _host = start_host();
    let rider = signup("rider@acme.io", "rider");
    let validator = signup("insp@acme.io", "validator");

    // ===== the fare catalog is seeded ======================================
    let (s, r) = req("GET", "/api/fares", Some(&rider), None);
    assert_eq!(s, 200);
    let keys: Vec<&str> = r["items"].as_array().unwrap().iter().map(|f| f["key"].as_str().unwrap()).collect();
    assert!(keys.contains(&"single") && keys.contains(&"t60") && keys.contains(&"month"), "{keys:?}");

    // a rider cannot validate; a validator can.
    assert_eq!(req("POST", "/api/validate", Some(&rider), Some(json!({ "code": "x" }))).0, 403);

    // ===== single ticket: one scan accepts, the next rejects ===============
    let single = buy(&rider, "single");
    let a = validate(&validator, &single);
    assert_eq!(a["result"], "accept", "{a}");
    assert!(a["reason"].as_str().unwrap().contains("single ride"));
    let b = validate(&validator, &single);
    assert_eq!(b["result"], "reject", "second scan rejected: {b}");
    assert_eq!(b["reason"], "already used");

    // ===== duration ticket: activates with a remaining window ==============
    let t60 = buy(&rider, "t60");
    let d = validate(&validator, &t60);
    assert_eq!(d["result"], "accept");
    assert_eq!(d["remaining_min"], 60, "fresh 60-min ticket");
    assert!(d["valid_until"].as_u64().unwrap() > 0);
    // a re-scan within the window still accepts (unlimited rides).
    assert_eq!(validate(&validator, &t60)["result"], "accept");

    // ===== a fabricated code is rejected ===================================
    assert_eq!(validate(&validator, "NOT-A-REAL-TICKET")["reason"], "unknown ticket");

    // ===== a ticket renders a valid QR SVG (qr:encode) =====================
    let (s, ct, svg) = get_text(&format!("/api/tickets/{single}/qr.svg"), &rider);
    assert_eq!(s, 200);
    assert!(ct.starts_with("image/svg+xml"), "content-type: {ct}");
    assert!(svg.starts_with("<svg") && svg.contains("</svg>"), "an SVG document");

    // ===== SINGLE-USE UNDER CONCURRENCY: many validators, one ticket =======
    // 8 validators scan the same fresh single ticket at once — exactly one must
    // accept (the lock:mutex critical section serializes activate/consume).
    let contested = buy(&rider, "single");
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let tok = validator.clone();
            let code = contested.clone();
            std::thread::spawn(move || validate(&tok, &code)["result"].as_str().unwrap().to_string())
        })
        .collect();
    let results: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let accepts = results.iter().filter(|r| *r == "accept").count();
    assert_eq!(accepts, 1, "exactly one concurrent scan accepts a single ticket: {results:?}");

    // ground truth: the ticket is now "used" with exactly one validation.
    let (_, t) = req("GET", &format!("/api/tickets/{contested}"), Some(&validator), None);
    assert_eq!(t["status"], "used");
    assert_eq!(t["uses"], 1, "one validation recorded, not more");
}
