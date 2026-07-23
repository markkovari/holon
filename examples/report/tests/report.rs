//! E2E for the CSV import/report tool (REPORT.md) as ONE composed wasm HTTP
//! component on the native Rust host. The batch-ingest axis: import a CSV with a
//! mix of valid + invalid rows and prove typed validation splits them with
//! per-field errors, page the clean set through the opaque cursor, and export
//! it back to CSV through the same codec (round-trip).

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::Value;

const ADDR: &str = "127.0.0.1:3032";

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

fn get(path: &str) -> (u16, String) {
    let url = format!("{}{}", base(), path);
    let resp = match ureq::get(&url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => panic!("GET {path}: {e}"),
    };
    let status = resp.status();
    (status, resp.into_string().unwrap_or_default())
}

fn get_json(path: &str) -> (u16, Value) {
    let (s, body) = get(path);
    (s, serde_json::from_str(&body).unwrap_or(Value::Null))
}

fn post_csv(path: &str, csv: &str) -> (u16, Value) {
    let url = format!("{}{}", base(), path);
    let resp = match ureq::post(&url).set("content-type", "text/csv").send_string(csv) {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => panic!("POST {path}: {e}"),
    };
    let status = resp.status();
    (status, serde_json::from_str(&resp.into_string().unwrap_or_default()).unwrap_or(Value::Null))
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/vet-host");
    let component = root.join("components/target/csv_report.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-report`)");
    assert!(component.exists(), "composed wasm missing (just compose-report)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "report")
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
    panic!("report host did not start");
}

#[test]
fn import_validate_page_and_export() {
    let _host = start_host();

    // a CSV with 3 good rows and 2 bad: one bad email, one age over the ceiling
    // + bad role.
    let csv = "name,email,age,role\n\
        Ada Lovelace,ada@example.com,36,admin\n\
        Alan Turing,alan@example.com,41,user\n\
        Grace Hopper,grace@example.com,45,guest\n\
        Bad Email,not-an-email,30,user\n\
        Old Wizard,wiz@example.com,999,overlord\n";

    let (status, r) = post_csv("/api/import", csv);
    assert_eq!(status, 200, "import: {r}");
    assert_eq!(r["imported"].as_u64().unwrap(), 3, "3 valid rows import: {r}");
    assert_eq!(r["rejected"].as_u64().unwrap(), 2, "2 rows rejected: {r}");

    // per-field errors are surfaced, not just a count.
    let rejects = r["rejects"].as_array().unwrap();
    let bad_email = rejects.iter().find(|x| x["row"]["email"] == "not-an-email").expect("bad-email reject");
    assert!(bad_email["errors"].as_array().unwrap().iter().any(|e| e["field"] == "email"), "email error surfaced: {bad_email}");
    let old = rejects.iter().find(|x| x["row"]["age"] == 999).expect("old reject");
    let fields: Vec<&str> = old["errors"].as_array().unwrap().iter().filter_map(|e| e["field"].as_str()).collect();
    assert!(fields.contains(&"age"), "age range error: {old}");
    assert!(fields.contains(&"role"), "role one-of error: {old}");

    // page the clean set: limit 2 -> a cursor -> the rest.
    let (_, p1) = get_json("/api/rows?limit=2");
    assert_eq!(p1["rows"].as_array().unwrap().len(), 2, "first page has 2: {p1}");
    let cursor = p1["next"].as_str().expect("a next cursor").to_string();
    let (_, p2) = get_json(&format!("/api/rows?limit=2&after={cursor}"));
    assert_eq!(p2["rows"].as_array().unwrap().len(), 1, "second page has the last 1: {p2}");

    // an invalid cursor is rejected, not silently ignored.
    let (bad_status, _) = get_json("/api/rows?after=not-a-real-cursor");
    assert_eq!(bad_status, 400, "garbage cursor must 400");

    // export re-serializes the clean set through the SAME codec (round-trip):
    // header + 3 data rows, none of the rejected ones.
    let (status, body) = get("/api/export");
    assert_eq!(status, 200);
    let lines: Vec<&str> = body.trim().lines().collect();
    assert_eq!(lines[0], "name,email,age,role", "export header: {body}");
    assert_eq!(lines.len(), 4, "header + 3 clean rows: {body}");
    assert!(body.contains("ada@example.com"));
    assert!(!body.contains("not-an-email"), "rejected rows must NOT be in the export");

    let (_, st) = get_json("/api/stats");
    assert_eq!(st["rows"].as_u64().unwrap(), 3, "3 rows stored: {st}");
}
