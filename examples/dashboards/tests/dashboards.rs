//! E2E for the dashboards app (DASHBOARDS.md) as ONE composed wasm HTTP
//! component (dashboards-domain + auth-guard + records + svg-chart) on the native
//! Rust host. Proves: a fresh account is seeded a demo dashboard; every panel
//! renders to a valid SVG per kind (bar/line/donut/sparkline) via svg:chart; a
//! new panel round-trips and renders; and one account cannot read another's.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:3043";

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

fn signup(email: &str) -> String {
    let (s, _) = req("POST", "/api/register", None, Some(json!({ "email": email, "password": "pw12345678" })));
    assert!(s == 201 || s == 409, "register {email}: {s}");
    let (s, l) = req("POST", "/api/login", None, Some(json!({ "email": email, "password": "pw12345678" })));
    assert_eq!(s, 200, "login {email}: {l}");
    l["access_token"].as_str().unwrap().to_string()
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/vet-host");
    let component = root.join("components/target/dashboards_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-dashboards`)");
    assert!(component.exists(), "composed wasm missing (just compose-dashboards)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "dashboards")
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
    panic!("dashboards host did not start");
}

#[test]
fn dashboards_seed_render_and_ownership() {
    let _host = start_host();
    let alice = signup("alice@acme.io");

    // ===== a fresh account is seeded a demo dashboard =====================
    let (s, r) = req("GET", "/api/dashboards", Some(&alice), None);
    assert_eq!(s, 200);
    let dashes = r["items"].as_array().unwrap();
    assert_eq!(dashes.len(), 1, "one seeded dashboard");
    let did = dashes[0]["id"].as_str().unwrap().to_string();

    // it has the four demo panels, one of each kind.
    let (_, d) = req("GET", &format!("/api/dashboards/{did}"), Some(&alice), None);
    let panels = d["panels"].as_array().unwrap();
    assert_eq!(panels.len(), 4, "four seeded panels");
    let kinds: std::collections::HashSet<&str> = panels.iter().map(|p| p["kind"].as_str().unwrap()).collect();
    for k in ["bar", "line", "donut", "sparkline"] {
        assert!(kinds.contains(k), "seeded kinds include {k}: {kinds:?}");
    }

    // ===== every panel renders to a valid SVG (svg:chart) =================
    for p in panels {
        let id = p["id"].as_str().unwrap();
        let (s, ct, svg) = get_text(&format!("/api/panels/{id}/chart.svg"), &alice);
        assert_eq!(s, 200);
        assert!(ct.starts_with("image/svg+xml"), "content-type: {ct}");
        assert!(svg.starts_with("<svg") && svg.contains("viewBox=") && svg.trim_end().ends_with("</svg>"), "valid svg for {}", p["kind"]);
    }

    // ===== a new panel round-trips and renders ============================
    let (s, np) = req("POST", &format!("/api/dashboards/{did}/panels"), Some(&alice),
        Some(json!({ "title": "Pets", "kind": "donut", "data": [{"label":"Cats","value":12},{"label":"Dogs","value":9}] })));
    assert_eq!(s, 201, "{np}");
    let npid = np["id"].as_str().unwrap();
    let (_, _, svg) = get_text(&format!("/api/panels/{npid}/chart.svg"), &alice);
    assert!(svg.contains("Pets") && svg.contains("Cats"), "new panel renders with its data");
    // it's now listed.
    let (_, d) = req("GET", &format!("/api/dashboards/{did}"), Some(&alice), None);
    assert_eq!(d["panels"].as_array().unwrap().len(), 5);

    // an empty-data panel is rejected.
    assert_eq!(req("POST", &format!("/api/dashboards/{did}/panels"), Some(&alice),
        Some(json!({ "title": "x", "kind": "bar", "data": [] }))).0, 422);

    // ===== ownership: another account can't read or render alice's =========
    let bob = signup("bob@acme.io");
    assert_eq!(req("GET", &format!("/api/dashboards/{did}"), Some(&bob), None).0, 404, "bob can't read alice's dashboard");
    assert_eq!(get_text(&format!("/api/panels/{npid}/chart.svg"), &bob).0, 404, "bob can't render alice's panel");
    // bob has his own seeded dashboard, distinct from alice's.
    let (_, rb) = req("GET", "/api/dashboards", Some(&bob), None);
    let bob_did = rb["items"][0]["id"].as_str().unwrap();
    assert_ne!(bob_did, did, "separate dashboards per account");
}
