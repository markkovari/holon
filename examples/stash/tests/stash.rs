//! E2E for the stash note app (docs/apps/STASH.md) as ONE composed wasm HTTP component
//! (stash-domain + auth-guard + records + zip + csv) on the native Rust host.
//! Proves the export: notes CRUD, then `GET /api/export.zip` returns a VALID ZIP
//! (PK local + central-directory + end-of-central-directory records) whose entry
//! count and names match the notes plus `index.csv` and `manifest.json`.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3046";

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
    let component = root.join("components/target/stash_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-stash`)");
    assert!(component.exists(), "composed wasm missing (just compose-stash)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "stash")
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
    panic!("stash host did not start");
}

/// Total entries from a ZIP's end-of-central-directory record.
fn zip_entry_count(zip: &[u8]) -> u16 {
    let eocd = zip.windows(4).rposition(|w| w == [0x50, 0x4b, 0x05, 0x06]).expect("EOCD signature");
    u16::from_le_bytes([zip[eocd + 10], zip[eocd + 11]])
}

fn contains(zip: &[u8], needle: &[u8]) -> bool {
    zip.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn notes_crud_and_zip_export() {
    let _host = start_host();
    let tok = signup("keep@acme.io");

    // ===== seeded notes + CRUD ============================================
    let (_, r) = req("GET", "/api/notes", Some(&tok), None);
    assert_eq!(r["items"].as_array().unwrap().len(), 3, "seeded notes");

    let (s, n) = req("POST", "/api/notes", Some(&tok), Some(json!({ "title": "Fourth", "body": "hello" })));
    assert_eq!(s, 201, "{n}");
    let id = n["id"].as_str().unwrap().to_string();
    let (s, _) = req("PATCH", &format!("/api/notes/{id}"), Some(&tok), Some(json!({ "body": "edited" })));
    assert_eq!(s, 200);
    // another account can't touch it.
    let other = signup("nope@acme.io");
    assert_eq!(req("PATCH", &format!("/api/notes/{id}"), Some(&other), Some(json!({ "body": "x" }))).0, 403);

    // ===== the export is a VALID ZIP ======================================
    let resp = ureq::get(&format!("{}/api/export.zip", base()))
        .set("authorization", &format!("Bearer {tok}"))
        .call()
        .expect("export.zip");
    assert_eq!(resp.status(), 200);
    assert!(resp.header("content-type").unwrap_or("").starts_with("application/zip"), "content-type");
    let mut zip = Vec::new();
    resp.into_reader().read_to_end(&mut zip).unwrap();

    // PK local-file-header at the very start, and the EOCD record present.
    assert!(zip.starts_with(&[0x50, 0x4b, 0x03, 0x04]), "ZIP local header");
    // 4 notes + index.csv + manifest.json = 6 entries.
    assert_eq!(zip_entry_count(&zip), 6, "one .md per note + index + manifest");
    assert!(contains(&zip, b"index.csv"), "index.csv entry");
    assert!(contains(&zip, b"manifest.json"), "manifest.json entry");
    assert!(contains(&zip, b"notes/fourth-"), "the note's slugged filename");
    // the manifest is real JSON with the right count (it's stored, so readable in-place).
    assert!(contains(&zip, br#""count":4"#) && contains(&zip, br#""app":"stash""#), "manifest contents");

    // deleting a note shrinks the next export by one entry.
    assert_eq!(req("DELETE", &format!("/api/notes/{id}"), Some(&tok), None).0, 200);
    let resp = ureq::get(&format!("{}/api/export.zip", base())).set("authorization", &format!("Bearer {tok}")).call().unwrap();
    let mut zip2 = Vec::new();
    resp.into_reader().read_to_end(&mut zip2).unwrap();
    assert_eq!(zip_entry_count(&zip2), 5, "3 notes + index + manifest after delete");
}
