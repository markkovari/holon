//! E2E for passwordless passkey sign-in (PASSKEY.md) as ONE composed wasm HTTP
//! component (passkey-domain + webauthn + records + cache + session-store) on the
//! native Rust host — driven by a **virtual authenticator**.
//!
//! The test holds a P-256 key and performs the real ceremonies: it builds the CBOR
//! attestation object, the COSE public key, and DER-encoded ECDSA signatures over
//! `authData || sha256(clientDataJSON)`. The server cannot tell it from Touch ID,
//! which is the point — it also lets the test produce ceremonies a real
//! authenticator never would, and check that each one is refused, by reason:
//!
//!   * a replayed challenge (single-use, enforced by spending the cache entry)
//!   * a phishing origin
//!   * a credential minted for another RP ID
//!   * a signature from the wrong key, and tampered authData
//!   * a signature counter that went backwards (a cloned authenticator)
//!   * enrolling a second passkey on someone else's account without a session
//!
//! (The fixture below duplicates a little of the component's own unit-test
//! authenticator — the component tests live inside the component crate and can't
//! be shared with an external e2e crate.)

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const ADDR: &str = "127.0.0.1:3053";
const RP: &str = "localhost";
const ORIGIN: &str = "http://localhost:3053";

// authData flags (WebAuthn §6.1).
const UP: u8 = 1 << 0;
const UV: u8 = 1 << 2;
const AT: u8 = 1 << 6;

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// ---- http -------------------------------------------------------------------

fn post(path: &str, body: Value) -> (u16, Value) {
    send(ureq::post(&format!("http://{ADDR}{path}")), body)
}
fn post_as(path: &str, body: Value, token: &str) -> (u16, Value) {
    send(
        ureq::post(&format!("http://{ADDR}{path}")).set("authorization", &format!("bearer {token}")),
        body,
    )
}
fn send(req: ureq::Request, body: Value) -> (u16, Value) {
    match req.set("content-type", "application/json").send_string(&body.to_string()) {
        Ok(r) => (r.status(), json_of(r)),
        Err(ureq::Error::Status(s, r)) => (s, json_of(r)),
        Err(e) => panic!("request failed: {e}"),
    }
}
fn get_as(path: &str, token: &str) -> (u16, Value) {
    match ureq::get(&format!("http://{ADDR}{path}")).set("authorization", &format!("bearer {token}")).call() {
        Ok(r) => (r.status(), json_of(r)),
        Err(ureq::Error::Status(s, r)) => (s, json_of(r)),
        Err(e) => panic!("GET {path}: {e}"),
    }
}
fn json_of(r: ureq::Response) -> Value {
    serde_json::from_str(&r.into_string().unwrap_or_default()).unwrap_or(Value::Null)
}

fn start_host() -> Kill {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let bin = root.join("host/target/release/comp-host");
    let component = root.join("components/target/passkey_domain.composed.wasm");
    assert!(bin.exists(), "host not built: {bin:?} (run `just e2e-passkey`)");
    assert!(component.exists(), "composed wasm missing (just compose-passkey)");
    let child = Command::new(&bin)
        .args(["--component", component.to_str().unwrap(), "--addr", ADDR, "--kv", "memory"])
        .env("VET_TENANT", "passkey")
        // The RP identity is CONFIG, never request data — that is what makes the
        // origin check worth anything.
        .env("CFG_RP_ID", RP)
        .env("CFG_ORIGIN", ORIGIN)
        .spawn()
        .expect("spawn comp-host");
    let guard = Kill(child);
    for _ in 0..200 {
        if let Ok(r) = ureq::get(&format!("http://{ADDR}/")).call() {
            if r.status() == 200 {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("passkey host did not start");
}

// ---- the virtual authenticator ----------------------------------------------

fn b64u(b: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(b)
}

fn client_data(ty: &str, challenge: &str, origin: &str) -> Vec<u8> {
    format!(r#"{{"type":"{ty}","challenge":"{challenge}","origin":"{origin}","crossOrigin":false}}"#).into_bytes()
}

/// CBOR head byte(s) for a major type + argument (canonical, shortest form).
fn head(major: u8, arg: usize) -> Vec<u8> {
    let m = major << 5;
    match arg {
        0..=23 => vec![m | arg as u8],
        24..=0xff => vec![m | 24, arg as u8],
        0x100..=0xffff => vec![m | 25, (arg >> 8) as u8, arg as u8],
        _ => unreachable!(),
    }
}

/// The COSE_Key an authenticator reports for an ES256 credential:
/// `{1: 2, 3: -7, -1: 1, -2: x, -3: y}`.
fn cose_key(sk: &SigningKey) -> Vec<u8> {
    let p = sk.verifying_key().to_encoded_point(false);
    let mut out = vec![0xa5];
    out.extend([0x01, 0x02]); // kty: EC2
    out.extend([0x03, 0x26]); // alg: -7 (ES256)
    out.extend([0x20, 0x01]); // crv (-1): P-256
    out.extend([0x21]); // x (-2)
    out.extend(head(2, 32));
    out.extend(&p.x().unwrap()[..]);
    out.extend([0x22]); // y (-3)
    out.extend(head(2, 32));
    out.extend(&p.y().unwrap()[..]);
    out
}

fn auth_data(rp: &str, flags: u8, count: u32, cred: Option<(&[u8], &[u8])>) -> Vec<u8> {
    let mut ad = Sha256::digest(rp.as_bytes()).to_vec();
    ad.push(flags);
    ad.extend(count.to_be_bytes());
    if let Some((id, cose)) = cred {
        ad.extend([0u8; 16]); // aaguid: model not disclosed
        ad.extend((id.len() as u16).to_be_bytes());
        ad.extend(id);
        ad.extend(cose);
    }
    ad
}

/// `{"fmt": "none", "attStmt": {}, "authData": <bytes>}`
fn attestation_object(ad: &[u8]) -> Vec<u8> {
    let mut out = vec![0xa3];
    out.extend(b"\x63fmt\x64none\x67attStmt\xa0\x68authData");
    out.extend(head(2, ad.len()));
    out.extend(ad);
    out
}

/// A `navigator.credentials.create()` response for this key.
fn create_response(sk: &SigningKey, cred_id: &[u8], challenge: &str, rp: &str, origin: &str) -> (Vec<u8>, Vec<u8>) {
    let cose = cose_key(sk);
    let ad = auth_data(rp, UP | UV | AT, 0, Some((cred_id, &cose)));
    (client_data("webauthn.create", challenge, origin), attestation_object(&ad))
}

/// A `navigator.credentials.get()` response: authData + a DER ECDSA signature
/// over `authData || sha256(clientDataJSON)`.
fn get_response(sk: &SigningKey, challenge: &str, rp: &str, origin: &str, count: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let cd = client_data("webauthn.get", challenge, origin);
    let ad = auth_data(rp, UP | UV, count, None);
    let mut signed = ad.clone();
    signed.extend(Sha256::digest(&cd));
    let sig: Signature = sk.sign(&signed);
    (cd, ad, sig.to_der().as_bytes().to_vec())
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32].into()).unwrap()
}

// ---- ceremonies as the SPA performs them ------------------------------------

fn register(username: &str, sk: &SigningKey, cred_id: &[u8], token: Option<&str>) -> (u16, Value) {
    let (code, opts) = match token {
        Some(t) => post_as("/api/register/begin", json!({ "username": username }), t),
        None => post("/api/register/begin", json!({ "username": username })),
    };
    if code != 200 {
        return (code, opts);
    }
    let challenge = opts["challenge"].as_str().unwrap();
    let (cd, att) = create_response(sk, cred_id, challenge, RP, ORIGIN);
    post(
        "/api/register/finish",
        json!({ "username": username, "id": b64u(cred_id),
                "client_data_json": b64u(&cd), "attestation_object": b64u(&att) }),
    )
}

fn login_begin(username: Option<&str>) -> Value {
    let body = match username {
        Some(u) => json!({ "username": u }),
        None => json!({}),
    };
    let (code, v) = post("/api/login/begin", body);
    assert_eq!(code, 200, "login/begin: {v}");
    v
}

fn login_finish(cred_id: &[u8], cd: &[u8], ad: &[u8], sig: &[u8]) -> (u16, Value) {
    post(
        "/api/login/finish",
        json!({ "id": b64u(cred_id), "client_data_json": b64u(cd),
                "authenticator_data": b64u(ad), "signature": b64u(sig) }),
    )
}

#[test]
fn passkey_ceremonies() {
    let _host = start_host();

    // The RP identity the browser will be held to — from config, not the request.
    let cfg = json_of(ureq::get(&format!("http://{ADDR}/api/config")).call().unwrap());
    assert_eq!(cfg["rp_id"], RP);
    assert_eq!(cfg["origin"], ORIGIN);

    // ===== registration: no password anywhere in this flow =================
    let ada = key(11);
    let ada_id = b"ada-touchid";
    let (code, reg) = register("ada", &ada, ada_id, None);
    assert_eq!(code, 201, "registration: {reg}");
    let token = reg["token"].as_str().unwrap().to_string();
    assert_eq!(reg["credential"]["alg"], -7, "ES256");
    assert_eq!(reg["credential"]["aaguid"], "0".repeat(32));

    let (code, me) = get_as("/api/me", &token);
    assert_eq!(code, 200, "{me}");
    assert_eq!(me["username"], "ada");
    assert_eq!(me["credentials"].as_array().unwrap().len(), 1);

    // ===== login ==========================================================
    let opts = login_begin(Some("ada"));
    let allow = opts["allowCredentials"].as_array().unwrap();
    assert_eq!(allow[0]["id"], b64u(ada_id), "the server tells the browser which passkey");
    let challenge = opts["challenge"].as_str().unwrap().to_string();
    let (cd, ad, sig) = get_response(&ada, &challenge, RP, ORIGIN, 1);
    let (code, login) = login_finish(ada_id, &cd, &ad, &sig);
    assert_eq!(code, 200, "login: {login}");
    let session = login["token"].as_str().unwrap().to_string();
    assert_eq!(login["credential"]["sign_count"], 1, "the counter was persisted");

    // ===== a challenge is single-use ======================================
    let (code, replay) = login_finish(ada_id, &cd, &ad, &sig);
    assert_eq!(code, 400, "the very same ceremony, replayed: {replay}");
    assert!(replay["error"].as_str().unwrap().contains("challenge"), "{replay}");

    // ===== every check bites ==============================================
    // a phishing origin
    let c = login_begin(Some("ada"))["challenge"].as_str().unwrap().to_string();
    let (cd, ad, sig) = get_response(&ada, &c, RP, "http://evil.example", 2);
    let (code, r) = login_finish(ada_id, &cd, &ad, &sig);
    assert_eq!(code, 401);
    assert_eq!(r["error"], "origin_mismatch", "{r}");
    assert_eq!(r["detail"], "http://evil.example");

    // a credential minted for another relying party
    let c = login_begin(Some("ada"))["challenge"].as_str().unwrap().to_string();
    let (cd, ad, sig) = get_response(&ada, &c, "evil.example", ORIGIN, 2);
    let (code, r) = login_finish(ada_id, &cd, &ad, &sig);
    assert_eq!((code, r["error"].as_str().unwrap()), (401, "rp_id_mismatch"));

    // somebody else's key
    let c = login_begin(Some("ada"))["challenge"].as_str().unwrap().to_string();
    let (cd, ad, sig) = get_response(&key(99), &c, RP, ORIGIN, 2);
    let (code, r) = login_finish(ada_id, &cd, &ad, &sig);
    assert_eq!((code, r["error"].as_str().unwrap()), (401, "bad_signature"));

    // tampered authData (flip the counter after signing)
    let c = login_begin(Some("ada"))["challenge"].as_str().unwrap().to_string();
    let (cd, mut ad, sig) = get_response(&ada, &c, RP, ORIGIN, 7);
    ad[36] ^= 0xff;
    let (code, r) = login_finish(ada_id, &cd, &ad, &sig);
    assert_eq!((code, r["error"].as_str().unwrap()), (401, "bad_signature"));

    // a cloned authenticator: the counter did not move past the stored 1
    let c = login_begin(Some("ada"))["challenge"].as_str().unwrap().to_string();
    let (cd, ad, sig) = get_response(&ada, &c, RP, ORIGIN, 1);
    let (code, r) = login_finish(ada_id, &cd, &ad, &sig);
    assert_eq!((code, r["error"].as_str().unwrap()), (401, "counter_regressed"), "{r}");

    // ===== you cannot enrol your authenticator on someone else's account ===
    let attacker = key(66);
    let (code, r) = register("ada", &attacker, b"attacker-key", None);
    assert_eq!(code, 401, "adding a passkey to an existing account needs a session: {r}");

    // ...but the account owner can add a second device.
    let phone = key(22);
    let (code, r) = register("ada", &phone, b"ada-phone", Some(&session));
    assert_eq!(code, 201, "second passkey: {r}");
    let (_, me) = get_as("/api/me", &session);
    assert_eq!(me["credentials"].as_array().unwrap().len(), 2);

    // ===== a discoverable ("usernameless") login ===========================
    let opts = login_begin(None);
    assert!(opts["allowCredentials"].as_array().unwrap().is_empty(), "the authenticator chooses");
    let c = opts["challenge"].as_str().unwrap().to_string();
    let (cd, ad, sig) = get_response(&phone, &c, RP, ORIGIN, 3);
    let (code, r) = login_finish(b"ada-phone", &cd, &ad, &sig);
    assert_eq!(code, 200, "usernameless login: {r}");
    assert_eq!(r["username"], "ada", "the credential id identified the account");

    // ===== managing passkeys ==============================================
    let (code, _) = post_as("/api/credentials/delete", json!({ "id": b64u(b"ada-phone") }), &session);
    assert_eq!(code, 200);
    let (code, r) = post_as("/api/credentials/delete", json!({ "id": b64u(ada_id) }), &session);
    assert_eq!(code, 409, "never delete the last passkey — there is no password to fall back on: {r}");

    // ===== logout ends the session ========================================
    let (code, _) = post_as("/api/logout", json!({}), &session);
    assert_eq!(code, 200);
    let (code, _) = get_as("/api/me", &session);
    assert_eq!(code, 401, "the session is gone");

    // ===== an unknown username does not leak whether it exists ============
    let opts = login_begin(Some("nobody"));
    assert!(opts["allowCredentials"].as_array().unwrap().is_empty());
    assert!(opts["challenge"].as_str().is_some(), "same shape as a real account");
}
