//! E2E for the 2FA authgate (docs/apps/AUTHGATE.md) as ONE composed wasm HTTP component
//! on the native Rust host. The challenge-response axis: enroll mints a TOTP
//! secret sealed in the vault; activation requires a first correct code; login
//! verifies a live code (or burns a single-use recovery code) and mints a
//! session. The test derives real RFC-6238 codes from the returned secret,
//! exactly as an authenticator app would.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use totp_lite::{totp_custom, Sha1};

const ADDR: &str = "127.0.0.1:3034";
// same 32-byte base64 master key the host recipe uses.
const MASTER_KEY: &str = "bWZhLWRlbW8tbWFzdGVyLWtleS0zMi1ieXRlcyEhISE=";

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

fn req(method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
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

/// Derive the current 6-digit TOTP code from a base32 secret (RFC 6238, SHA1,
/// period 30) — what the authenticator app shows.
fn code_for(secret_b32: &str) -> String {
    let key = data_encoding::BASE32_NOPAD
        .decode(secret_b32.as_bytes())
        .expect("valid base32 secret");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    totp_custom::<Sha1>(30, 6, &key, now)
}

fn start_host() -> HostGuard {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/comp-host");
    let component = root.join("components/target/mfa_authgate.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-authgate`)");
    assert!(component.exists(), "composed wasm missing (just compose-authgate)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "authgate")
        .env("CFG_MASTER_KEY", MASTER_KEY)
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
    panic!("authgate host did not start");
}

#[test]
fn enroll_activate_login_and_recovery() {
    let _host = start_host();
    let account = "alice@example.com";

    // 1. enroll: provision a secret (pending). The secret is returned once so
    // the app can render the QR; it is sealed in the vault server-side.
    let (s, enr) = req("POST", "/api/enroll", Some(json!({"account": account})));
    assert_eq!(s, 201, "enroll: {enr}");
    assert_eq!(enr["state"], "pending");
    let secret = enr["secret"].as_str().expect("secret").to_string();
    assert!(enr["uri"].as_str().unwrap().starts_with("otpauth://totp/"), "otpauth uri");
    // the otpauth URI is rendered as a scannable QR (qr:encode) so the user
    // doesn't have to type the secret.
    let qr = enr["qr_svg"].as_str().expect("qr_svg");
    assert!(qr.starts_with("<svg") && qr.contains("<path"), "qr svg rendered: {}", &qr[..qr.len().min(40)]);

    // status shows pending, not yet usable.
    let (_, st) = req("GET", &format!("/api/status/{account}"), None);
    assert_eq!(st["state"], "pending");

    // a wrong first code does NOT activate.
    let (s, _) = req("POST", "/api/activate", Some(json!({"account": account, "code": "000000"})));
    assert_eq!(s, 401, "wrong first code must be rejected");

    // 2. activate with the real code -> enrolled + recovery codes (returned once).
    let (s, act) = req("POST", "/api/activate", Some(json!({"account": account, "code": code_for(&secret)})));
    assert_eq!(s, 200, "activate: {act}");
    assert_eq!(act["state"], "enrolled");
    let recovery: Vec<String> = act["recovery_codes"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
    assert_eq!(recovery.len(), 5, "five recovery codes issued");

    // re-activating is refused.
    let (s, _) = req("POST", "/api/activate", Some(json!({"account": account, "code": code_for(&secret)})));
    assert_eq!(s, 409, "already active");

    // 3. login with a live TOTP code -> a session.
    let (s, login) = req("POST", "/api/login", Some(json!({"account": account, "code": code_for(&secret)})));
    assert_eq!(s, 200, "login: {login}");
    assert_eq!(login["via"], "totp");
    let session = login["session"].as_str().expect("session id").to_string();
    assert!(!login["csrf"].as_str().unwrap().is_empty(), "csrf token issued");

    // the session looks up and carries the account.
    let (s, sess) = req("GET", &format!("/api/session/{session}"), None);
    assert_eq!(s, 200);
    assert_eq!(sess["data"]["account"], account);
    assert_eq!(sess["data"]["mfa"], true);

    // a wrong login code is rejected.
    let (s, _) = req("POST", "/api/login", Some(json!({"account": account, "code": "111111"})));
    assert_eq!(s, 401, "wrong login code rejected");

    // a recovery code logs in (via=recovery) ...
    let (s, rlogin) = req("POST", "/api/login", Some(json!({"account": account, "code": recovery[0]})));
    assert_eq!(s, 200, "recovery login: {rlogin}");
    assert_eq!(rlogin["via"], "recovery");

    // ... and is single-use: reusing the same recovery code fails.
    let (s, _) = req("POST", "/api/login", Some(json!({"account": account, "code": recovery[0]})));
    assert_eq!(s, 401, "recovery code is single-use");

    // status now shows one recovery code burned (4 remaining).
    let (_, st) = req("GET", &format!("/api/status/{account}"), None);
    assert_eq!(st["recovery_remaining"].as_u64().unwrap(), 4, "one recovery code burned: {st}");

    // logout revokes the session.
    let (s, _) = req("POST", "/api/logout", Some(json!({"session": session})));
    assert_eq!(s, 200);
    let (s, _) = req("GET", &format!("/api/session/{session}"), None);
    assert_eq!(s, 404, "revoked session is gone");
}
