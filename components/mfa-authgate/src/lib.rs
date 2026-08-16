//! mfa:app — TOTP 2FA enrollment + challenge-response login over composed
//! contracts.
//!
//! The challenge-response axis: three states over one account.
//!
//! 1. **enroll** — `otp::provision` mints a random shared secret + an
//!    `otpauth://` URI (the QR an authenticator app scans). The secret is
//!    sealed in `secrets:vault` (envelope-encrypted; only ciphertext hits the
//!    store) and the account is recorded `pending`.
//! 2. **activate** — the user types the first code from their app; we
//!    `vault::get` the secret and `otp::verify` it. Only a correct first code
//!    flips the account to `enrolled` and issues single-use **recovery codes**
//!    (we store their SHA-256 hashes, never the codes).
//! 3. **login** — a live TOTP code (or a not-yet-burned recovery code) is
//!    verified; on success `session::create` mints an opaque server-side
//!    session + CSRF token. The secret is write-once, read-only-to-verify.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use bindings::otp::totp::authenticator as otp;
use bindings::qr::encode::encoder as qr;
use bindings::records::store::store as records;
use bindings::secrets::vault::vault;
use bindings::session::store::store as session;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const ACCOUNTS: &str = "mfa_accounts";
const ISSUER: &str = "comp-authgate";
const PERIOD: u32 = 30;
const DIGITS: u8 = 6;
const SKEW: u32 = 1;
const SESSION_TTL: u64 = 900;
const RECOVERY_COUNT: u32 = 5;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage_json(),
            (Method::Post, ["api", "enroll"]) => enroll(&request),
            (Method::Post, ["api", "activate"]) => activate(&request),
            (Method::Post, ["api", "login"]) => login(&request),
            (Method::Get, ["api", "session", id]) => get_session(id),
            (Method::Post, ["api", "logout"]) => logout(&request),
            (Method::Get, ["api", "status", account]) => status(account),
            _ => Outcome::err(404, "not_found"),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
}
impl Outcome {
    fn err(code: u16, msg: &str) -> Outcome {
        Outcome::Json(code, json!({ "error": msg }).to_string())
    }
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn secret_name(account: &str) -> String {
    format!("totp/{account}")
}

/// Look up the enrollment record by account (a secondary index — the store keys
/// by its own minted id). Returns the store entry so callers can `update` by
/// `entry.id` at the correct revision.
fn find_account(account: &str) -> Result<Option<records::Entry>, records::StoreError> {
    let hits = records::find_by(ACCOUNTS, "account", &json!(account).to_string())?;
    Ok(hits.into_iter().next())
}

fn hash_code(code: &str) -> String {
    let mut h = Sha256::new();
    // normalize: recovery codes are compared case-insensitively, dashes ignored.
    h.update(code.to_lowercase().replace('-', "").as_bytes());
    let digest = h.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "authgate",
            "about": "TOTP 2FA enrollment + challenge-response login — the secret is sealed in a vault; you prove you hold it right now",
            "enroll": "POST /api/enroll {account} -> {uri, secret, qr_svg} (pending)",
            "activate": "POST /api/activate {account, code} -> {recovery_codes[]} (enrolled)",
            "login": "POST /api/login {account, code} -> {session, csrf} (TOTP or a recovery code)",
            "session": "GET /api/session/{id}",
            "logout": "POST /api/logout {session}",
            "status": "GET /api/status/{account}"
        })
        .to_string(),
    )
}

// ---- 1. enroll: provision + seal ---------------------------------------------

fn enroll(request: &IncomingRequest) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let account = body["account"].as_str().unwrap_or("").trim().to_string();
    if account.is_empty() {
        return Outcome::err(422, "account required");
    }
    // one active enrollment per account; re-enrolling replaces a pending one.
    let existing = match find_account(&account) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    if let Some(entry) = &existing {
        if let Ok(v) = serde_json::from_str::<Value>(&entry.data) {
            if v["state"] == "enrolled" {
                return Outcome::err(409, "already enrolled");
            }
        }
    }
    let prov = match otp::provision(ISSUER, &account) {
        Ok(p) => p,
        Err(e) => return otp_err(e),
    };
    // seal the secret — plaintext never touches the record store.
    if let Err(e) = vault::put(&secret_name(&account), prov.secret.as_bytes()) {
        return vault_err(e);
    }
    let rec = json!({"account": account, "state": "pending", "recovery": [], "at": now()});
    let write = match existing {
        Some(entry) => records::update(ACCOUNTS, &entry.id, &rec.to_string(), entry.revision),
        None => records::create(ACCOUNTS, &rec.to_string(), &["account".to_string()]),
    };
    if let Err(e) = write {
        return store_err(e);
    }
    // render the otpauth:// URI as a scannable QR so the authenticator app can
    // scan it instead of the user typing the secret. Fall back to just the URI
    // if the (bounded) input somehow doesn't fit a QR.
    let qr_svg = qr::svg(&prov.uri, qr::Ecc::Medium, 4).unwrap_or_default();
    Outcome::Json(
        201,
        json!({
            "account": account,
            "uri": prov.uri,
            "secret": prov.secret,
            "qr_svg": qr_svg,
            "state": "pending"
        })
        .to_string(),
    )
}

// ---- 2. activate: first correct code -> enrolled + recovery codes ------------

fn activate(request: &IncomingRequest) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let account = body["account"].as_str().unwrap_or("").trim().to_string();
    let code = body["code"].as_str().unwrap_or("").trim().to_string();
    if account.is_empty() || code.is_empty() {
        return Outcome::err(422, "account and code required");
    }
    let entry = match find_account(&account) {
        Ok(Some(e)) => e,
        Ok(None) => return Outcome::err(404, "not enrolled"),
        Err(e) => return store_err(e),
    };
    let mut rec: Value = serde_json::from_str(&entry.data).unwrap_or_else(|_| json!({}));
    if rec["state"] == "enrolled" {
        return Outcome::err(409, "already active");
    }
    let secret = match vault::get(&secret_name(&account)) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        Err(e) => return vault_err(e),
    };
    match otp::verify(&secret, &code, PERIOD, DIGITS, SKEW) {
        Ok(true) => {}
        Ok(false) => return Outcome::err(401, "code did not verify"),
        Err(e) => return otp_err(e),
    }
    // first code proven — issue recovery codes, store only their hashes.
    let codes = otp::recovery_codes(RECOVERY_COUNT);
    let hashes: Vec<String> = codes.iter().map(|c| hash_code(c)).collect();
    rec["state"] = json!("enrolled");
    rec["recovery"] = json!(hashes);
    rec["activated_at"] = json!(now());
    if let Err(e) = records::update(ACCOUNTS, &entry.id, &rec.to_string(), entry.revision) {
        return store_err(e);
    }
    // the plaintext recovery codes are returned ONCE, here, and never stored.
    Outcome::Json(200, json!({"account": account, "state": "enrolled", "recovery_codes": codes}).to_string())
}

// ---- 3. login: challenge -> session ------------------------------------------

fn login(request: &IncomingRequest) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let account = body["account"].as_str().unwrap_or("").trim().to_string();
    let code = body["code"].as_str().unwrap_or("").trim().to_string();
    if account.is_empty() || code.is_empty() {
        return Outcome::err(422, "account and code required");
    }
    let entry = match find_account(&account) {
        Ok(Some(e)) => e,
        Ok(None) => return Outcome::err(404, "not enrolled"),
        Err(e) => return store_err(e),
    };
    let mut rec: Value = serde_json::from_str(&entry.data).unwrap_or_else(|_| json!({}));
    if rec["state"] != "enrolled" {
        return Outcome::err(403, "enrollment not activated");
    }

    let mut used_recovery = false;
    let secret = match vault::get(&secret_name(&account)) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        Err(e) => return vault_err(e),
    };
    // try a live TOTP code first; fall back to burning a recovery code.
    let totp_ok = matches!(otp::verify(&secret, &code, PERIOD, DIGITS, SKEW), Ok(true));
    if !totp_ok {
        let h = hash_code(&code);
        let mut remaining: Vec<String> = rec["recovery"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        if let Some(pos) = remaining.iter().position(|x| *x == h) {
            remaining.remove(pos); // single-use: burn it
            rec["recovery"] = json!(remaining);
            used_recovery = true;
        } else {
            return Outcome::err(401, "invalid code");
        }
    }
    if used_recovery {
        // persist the burned recovery code.
        if let Err(e) = records::update(ACCOUNTS, &entry.id, &rec.to_string(), entry.revision) {
            return store_err(e);
        }
    }
    // challenge passed — mint a server-side session carrying the account.
    let data = json!({"account": account, "mfa": true}).to_string();
    match session::create(data.as_bytes(), SESSION_TTL) {
        Ok(s) => Outcome::Json(
            200,
            json!({"session": s.id, "csrf": s.csrf_token, "expires": s.expires, "via": if used_recovery {"recovery"} else {"totp"}}).to_string(),
        ),
        Err(e) => session_err(e),
    }
}

fn get_session(id: &str) -> Outcome {
    match session::get(id) {
        Ok(s) => {
            let data: Value = serde_json::from_slice(&s.data).unwrap_or_else(|_| json!({}));
            Outcome::Json(200, json!({"id": s.id, "data": data, "expires": s.expires}).to_string())
        }
        Err(session::SessionError::NotFound) => Outcome::err(404, "no live session"),
        Err(e) => session_err(e),
    }
}

fn logout(request: &IncomingRequest) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let id = body["session"].as_str().unwrap_or("").trim().to_string();
    if id.is_empty() {
        return Outcome::err(422, "session required");
    }
    match session::revoke(&id) {
        Ok(_) => Outcome::Json(200, json!({"revoked": true}).to_string()),
        Err(e) => session_err(e),
    }
}

fn status(account: &str) -> Outcome {
    match find_account(account) {
        Ok(Some(entry)) => {
            let rec: Value = serde_json::from_str(&entry.data).unwrap_or_else(|_| json!({}));
            let remaining = rec["recovery"].as_array().map(|a| a.len()).unwrap_or(0);
            Outcome::Json(
                200,
                json!({"account": account, "state": rec["state"], "recovery_remaining": remaining}).to_string(),
            )
        }
        Ok(None) => Outcome::Json(200, json!({"account": account, "state": "none", "recovery_remaining": 0}).to_string()),
        Err(e) => store_err(e),
    }
}

// ---- error mapping -----------------------------------------------------------

fn otp_err(e: otp::OtpError) -> Outcome {
    match e {
        otp::OtpError::BadSecret => Outcome::err(500, "bad secret"),
        otp::OtpError::BadDigits => Outcome::err(500, "bad digits"),
    }
}

fn vault_err(e: vault::VaultError) -> Outcome {
    match e {
        vault::VaultError::NotFound => Outcome::err(404, "secret not found"),
        vault::VaultError::Crypto(m) => Outcome::Json(500, json!({"error": format!("crypto: {m}")}).to_string()),
        vault::VaultError::BackendUnavailable(m) => Outcome::Json(503, json!({"error": m}).to_string()),
    }
}

fn session_err(e: session::SessionError) -> Outcome {
    match e {
        session::SessionError::NotFound => Outcome::err(404, "no live session"),
        session::SessionError::CsrfMismatch => Outcome::err(403, "csrf mismatch"),
        session::SessionError::BackendUnavailable(m) => Outcome::Json(503, json!({"error": m}).to_string()),
    }
}

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::err(404, "not_found"),
        records::StoreError::InvalidJson(m) => Outcome::Json(422, json!({"error": m}).to_string()),
        records::StoreError::RevisionConflict(_) => Outcome::err(409, "conflict"),
        records::StoreError::BackendUnavailable(m) => Outcome::Json(503, json!({"error": m}).to_string()),
    }
}

// ---- http plumbing -----------------------------------------------------------

fn parse_body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let body = read_body(request).map_err(|_| Outcome::err(400, "could not read body"))?;
    if body.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(&body).map_err(|e| Outcome::Json(400, json!({"error": format!("bad json: {e}")}).to_string()))
}

/// The most a request body may be, before the component stops reading it.
///
/// There was no ceiling anywhere: 148 of 150 components accumulated whatever
/// arrived until the guest hit wasmtime's 64 MiB per-store memory cap and TRAPPED,
/// which reaches the caller as a closed connection saying nothing about a size.
/// A component that answers JSON has no business reading sixteen megabytes, and
/// the ones that legitimately handle uploads police it themselves with a 413 and a
/// granted max-size — those are left alone.
///
/// Generous on purpose. This is a backstop against an unbounded read, not a
/// content policy; an API that needs a real limit should state its own and say 413.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let body = request.consume().map_err(|_| ())?;
    let stream = body.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                // A ceiling, not a policy: past this the read stops and the caller
                // is told, rather than growing until the store's memory cap traps
                // the component and the connection just closes.
                if buf.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(());
                }
                buf.extend_from_slice(&chunk);
            }
            // `Closed` is how wasi:io says end-of-body; `LastOperationFailed` is a
            // read that went wrong. Collapsing both into `break` returns a TRUNCATED
            // body as if it were complete — the same silent truncation that, on the
            // write side, took four runs to find.
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(_) => return Err(()),
        }
    }
    Ok(buf)
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond(response_out, code, body.as_bytes()),
    }
}

fn respond(response_out: ResponseOutparam, status: u16, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    let _ = headers.set(&"access-control-allow-origin".to_string(), &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in body.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
