//! E2E for real-estate-escrow: property transactions behind a login; register grants `agent`.
//!
//! Self-contained: spawns comp-host on the composed artifact apps/real-estate-escrow.toml names,
//! on a free port, with in-memory kv. Run with `cargo xtask e2e real-estate-escrow`.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{json, Value};

/// Kills the host when the test ends, pass or fail.
struct HostGuard(Child, String);
impl Drop for HostGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// A port nothing is listening on right now, so parallel suites do not collide.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Spawn comp-host on the composed artifact `apps/real-estate-escrow.toml` names.
fn start_host(extra: &[&str]) -> HostGuard {
    let bin = root().join("host/target/release/comp-host");
    let component = root().join("components/target/real-estate-escrow.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `cargo xtask e2e real-estate-escrow`)");
    assert!(component.exists(), "composed wasm missing: {component:?} (run `cargo xtask compose real-estate-escrow`)");
    let addr = format!("127.0.0.1:{}", free_port());
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", &addr, "--kv", "memory"])
        .args(["--app", "real-estate-escrow", "--tenant", "real-estate-escrow"])
        .args(["--config-file", concat!(env!("CARGO_MANIFEST_DIR"), "/../defaults.conf")])
        .args(["--config", "default-tenant=real-estate-escrow"])
        .args(extra)
        .spawn()
        .expect("spawn comp-host");
    let guard = HostGuard(child, format!("http://{addr}"));
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&guard.1).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("comp-host did not start on {addr}");
}

fn req(base: &str, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, String) {
    let mut r = ureq::request(method, &format!("{base}{path}"));
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
    (status, resp.into_string().unwrap_or_default())
}

fn jreq(base: &str, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, Value) {
    let (s, b) = req(base, method, path, token, body);
    (s, serde_json::from_str(&b).unwrap_or(Value::Null))
}

/// Register + log in; returns (subject, access token).
fn account(base: &str, email: &str) -> (String, String) {
    let creds = json!({ "email": email, "password": "correct-horse-9" });
    let (s, reg) = jreq(base, "POST", "/api/register", None, Some(creds.clone()));
    assert_eq!(s, 201, "register {email}: {reg}");
    let subject = reg["subject"].as_str().expect("subject").to_string();
    let (s, login) = jreq(base, "POST", "/api/login", None, Some(creds));
    assert_eq!(s, 200, "login {email}: {login}");
    let token = login["access_token"].as_str().expect("access_token").to_string();
    (subject, token)
}

/// The auth surface every one of these apps shares: the page, register/login,
/// bad credentials, me, logout revoking the session, and an unknown route.
fn auth_surface(base: &str, title: &str) {
    let (s, page) = req(base, "GET", "/", None, None);
    assert_eq!(s, 200);
    assert!(page.contains(title), "index page should name the app ({title}): {page:.200}");

    let (subject, token) = account(base, "auth@example.com");

    let (s, _) = jreq(
        base,
        "POST",
        "/api/register",
        None,
        Some(json!({ "email": "auth@example.com", "password": "correct-horse-9" })),
    );
    assert_ne!(s, 201, "a duplicate email must not register twice");

    let (s, _) = jreq(
        base,
        "POST",
        "/api/login",
        None,
        Some(json!({ "email": "auth@example.com", "password": "wrong-password" })),
    );
    assert_eq!(s, 401, "wrong password is refused");

    let (s, me) = jreq(base, "GET", "/api/me", Some(&token), None);
    assert_eq!(s, 200, "me: {me}");
    assert_eq!(me["subject"], json!(subject));

    let (s, _) = jreq(base, "GET", "/api/me", None, None);
    assert_eq!(s, 401, "me without a bearer is refused");

    let (s, _) = jreq(base, "POST", "/api/logout", Some(&token), None);
    assert_eq!(s, 200);
    let (s, _) = jreq(base, "GET", "/api/me", Some(&token), None);
    assert_eq!(s, 401, "a revoked session no longer authenticates");

    let (s, _) = jreq(base, "GET", "/api/nope", None, None);
    assert_eq!(s, 404);
}

/// Register grants `agent`, so a new account can open a transaction; the list is
/// owner-scoped, so a second account sees none of it.
#[test]
fn e2e() {
    let host = start_host(&[]);
    let base = host.1.as_str();
    auth_surface(base, "Real Estate Escrow");

    let (s, _) = req(base, "GET", "/app.js", None, None);
    assert_eq!(s, 200, "the UI's script is served");

    let (alice, a) = account(base, "alice@example.com");
    let (_, b) = account(base, "bob@example.com");

    let (s, _) = jreq(base, "GET", "/api/items", None, None);
    assert_eq!(s, 401);

    let (s, t) = jreq(base, "POST", "/api/items", Some(&a), Some(json!({ "name": "12 Elm St" })));
    assert_eq!(s, 201, "an agent opens a transaction: {t}");
    assert_eq!(t["name"], json!("12 Elm St"));
    assert_eq!(t["owner"], json!(alice));
    let id = t["id"].as_str().expect("id").to_string();

    let (s, list) = jreq(base, "GET", "/api/items", Some(&a), None);
    assert_eq!(s, 200);
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "alice sees her transaction: {list}");
    assert_eq!(items[0]["id"], json!(id));

    let (_, list) = jreq(base, "GET", "/api/items", Some(&b), None);
    assert_eq!(list["items"], json!([]), "bob sees none of alice's transactions");
}
