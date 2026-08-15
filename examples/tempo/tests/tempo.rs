//! E2E for the tempo worktime logger (docs/apps/TEMPO.md) as ONE composed wasm HTTP
//! component (tempo-domain + auth-guard + records + pdf) on the native Rust host.
//! Proves the capability model: admin creates projects/categories + assigns
//! per-project membership; a user logs only against projects they belong to; a
//! project LEAD sees that project's whole distribution (a member can't); owners
//! edit/delete their own entries; and the pomodoro timer produces an entry.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3040";

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

fn signup(email: &str, role: &str) -> String {
    let (s, _) = req("POST", "/api/register", None, Some(json!({ "email": email, "password": "pw12345678", "role": role })));
    assert!(s == 201 || s == 409, "register {email}: {s}");
    let (s, l) = req("POST", "/api/login", None, Some(json!({ "email": email, "password": "pw12345678" })));
    assert_eq!(s, 200, "login {email}: {l}");
    l["access_token"].as_str().unwrap().to_string()
}
fn total(v: &Value) -> u64 {
    v["total_minutes"].as_u64().unwrap()
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/comp-host");
    let component = root.join("components/target/tempo_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-tempo`)");
    assert!(component.exists(), "composed wasm missing (just compose-tempo)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "tempo")
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
    panic!("tempo host did not start");
}

#[test]
fn membership_capability_model() {
    let _host = start_host();

    let admin = signup("admin@acme.io", "admin");
    let ada = signup("ada@acme.io", "member");
    let bo = signup("bo@acme.io", "member");
    let boss = signup("boss@acme.io", "member"); // becomes a LEAD via membership

    // ===== admin creates projects + categories ==============================
    let (s, apollo) = req("POST", "/api/projects", Some(&admin), Some(json!({ "key": "APOLLO", "name": "Apollo" })));
    assert_eq!(s, 201, "{apollo}");
    let apollo = apollo["id"].as_str().unwrap().to_string();
    let (_, orion) = req("POST", "/api/projects", Some(&admin), Some(json!({ "key": "ORION", "name": "Orion" })));
    let orion = orion["id"].as_str().unwrap().to_string();
    let (_, eng) = req("POST", "/api/categories", Some(&admin), Some(json!({ "name": "engineering" })));
    let eng = eng["id"].as_str().unwrap().to_string();

    // a member cannot create projects
    let (s, _) = req("POST", "/api/projects", Some(&ada), Some(json!({ "key": "X", "name": "X" })));
    assert_eq!(s, 403);

    // ===== membership: ada+bo members of Apollo, boss its lead ==============
    for (email, role) in [("ada@acme.io", "member"), ("bo@acme.io", "member"), ("boss@acme.io", "lead")] {
        let (s, _) = req("POST", &format!("/api/projects/{apollo}/members"), Some(&admin), Some(json!({ "email": email, "role": role })));
        assert_eq!(s, 200, "add {email}");
    }
    // ada is NOT a member of Orion.

    // list_projects reflects membership
    let (_, mine) = req("GET", "/api/projects", Some(&ada), None);
    let ids: Vec<&str> = mine["items"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec![apollo.as_str()], "ada only sees Apollo");
    let (_, all) = req("GET", "/api/projects", Some(&admin), None);
    assert_eq!(all["items"].as_array().unwrap().len(), 2, "admin sees all projects");

    // ===== logging is gated by membership ===================================
    let log = |tok: &str, proj: &str, mins: u64, day: &str| {
        req("POST", "/api/entries", Some(tok), Some(json!({ "project": proj, "category": eng, "minutes": mins, "day": day })))
    };
    let (s, e1) = log(&ada, &apollo, 120, "2026-07-20");
    assert_eq!(s, 201, "{e1}");
    let entry_id = e1["id"].as_str().unwrap().to_string();
    let (s, _) = log(&bo, &apollo, 90, "2026-07-21");
    assert_eq!(s, 201);
    // ada can't log against Orion — she's not a member
    let (s, _) = log(&ada, &orion, 60, "2026-07-20");
    assert_eq!(s, 403, "non-member cannot log against the project");

    // ===== owner edit/delete ================================================
    let (s, _) = req("PATCH", &format!("/api/entries/{entry_id}"), Some(&ada), Some(json!({ "minutes": 150 })));
    assert_eq!(s, 200, "owner edits own entry");
    let (s, _) = req("PATCH", &format!("/api/entries/{entry_id}"), Some(&bo), Some(json!({ "minutes": 5 })));
    assert_eq!(s, 403, "a stranger cannot edit it");
    let (_, r) = req("GET", "/api/report?from=2026-07-01&to=2026-07-31", Some(&ada), None);
    assert_eq!(total(&r), 150, "edit took effect");
    let (s, _) = req("DELETE", &format!("/api/entries/{entry_id}"), Some(&ada), None);
    assert_eq!(s, 200, "owner deletes own entry");
    let (_, r) = req("GET", "/api/report?from=2026-07-01&to=2026-07-31", Some(&ada), None);
    assert_eq!(total(&r), 0, "gone after delete");
    // re-log for the lead-view check
    log(&ada, &apollo, 120, "2026-07-20");

    // ===== a project LEAD sees the project's whole distribution =============
    let (s, r) = req("GET", "/api/report?from=2026-07-01&to=2026-07-31&scope=all", Some(&boss), None);
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["scope"], "all");
    assert_eq!(r["can_see_all"], true, "boss leads Apollo");
    assert_eq!(total(&r), 210, "sees ada(120) + bo(90) on Apollo");
    let users: Vec<&str> = r["by_user"].as_array().unwrap().iter().map(|u| u["key"].as_str().unwrap()).collect();
    assert!(users.contains(&"ada@acme.io") && users.contains(&"bo@acme.io"), "grouped by person: {users:?}");

    // a plain member can't widen scope
    let (_, r) = req("GET", "/api/report?from=2026-07-01&to=2026-07-31&scope=all", Some(&bo), None);
    assert_eq!(r["scope"], "me");
    assert_eq!(r["can_see_all"], false);

    // ===== calendar: a scheduled entry carries its time-of-day =============
    let (s, sched) = req("POST", "/api/entries", Some(&ada),
        Some(json!({ "project": apollo, "category": eng, "minutes": 45, "day": "2026-07-23", "start": 9 * 60 })));
    assert_eq!(s, 201, "{sched}");
    assert_eq!(sched["start"], 540, "start (09:00) round-trips for the calendar grid");
    let (_, list) = req("GET", "/api/entries?from=2026-07-23&to=2026-07-23", Some(&ada), None);
    assert_eq!(list["entries"].as_array().unwrap()[0]["start"], 540);

    // ===== the pomodoro timer ===============================================
    let (s, _) = req("POST", "/api/timer/start", Some(&ada), Some(json!({ "project": apollo, "category": eng, "day": "2026-07-22" })));
    assert_eq!(s, 200);
    let (s, entry) = req("POST", "/api/timer/stop", Some(&ada), None);
    assert_eq!(s, 201, "{entry}");
    assert!(entry["minutes"].as_u64().unwrap() >= 1);

    // ===== the report exports as a real PDF (pdf:codec) =====================
    let resp = ureq::get(&format!("{}/api/report.pdf?from=2026-07-01&to=2026-07-31&scope=all", base()))
        .set("authorization", &format!("Bearer {boss}"))
        .call()
        .expect("report.pdf");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.content_type(), "application/pdf");
    let mut pdf = Vec::new();
    resp.into_reader().read_to_end(&mut pdf).unwrap();
    assert!(pdf.starts_with(b"%PDF-1.4"), "PDF header");
    assert!(pdf.ends_with(b"%%EOF"), "PDF trailer");
}
