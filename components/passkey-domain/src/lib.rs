//! `passkey-domain` — passwordless sign-in (docs/apps/PASSKEY.md) as ONE composed wasm HTTP
//! component. Exports `wasi:http`; imports only WIT contracts: `webauthn:verify`
//! (the ceremony verification), `records:store` (accounts + credentials),
//! `cache:store` (single-use challenges with a TTL), `session:store` (the session
//! a completed ceremony mints).
//!
//! The app's whole job is the bookkeeping around two ceremonies:
//!
//!   begin  -> mint a random challenge, remember what it was issued FOR
//!   finish -> spend the challenge (single use), verify, then persist
//!
//! Two rules that are easy to get wrong and are enforced here:
//!
//!   * the RP ID and origin come from `wasi:config`, never from the request — a
//!     client-supplied origin would make the anti-phishing check meaningless;
//!   * adding a passkey to an account that already has one requires a SESSION for
//!     that account, or anyone could enrol their own authenticator on your name.
//!
//! Config (wasi:config/store):
//!   rp-id        the RP ID credentials are scoped to (default `localhost`)
//!   origin       the exact origin the browser must report (default `http://localhost:3053`)
//!   require-uv   demand the UV flag (biometric/PIN), not just user presence (default false)

#[allow(warnings)]
mod bindings;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use serde_json::{json, Map, Value};

use bindings::cache::store::cache;
use bindings::records::store::store as records;
use bindings::session::store::store as sessions;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::config::store as config;
use bindings::wasi::random::random;
use bindings::webauthn::verify::verifier as wa;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const ACCOUNTS: &str = "accounts";
const CREDENTIALS: &str = "credentials";
/// A ceremony must be completed promptly; the browser's own timeout is 60s.
const CHALLENGE_TTL: u64 = 300;
const SESSION_TTL: u64 = 3600;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage(),
            (Method::Get, ["api", "config"]) => rp_config(),
            (Method::Post, ["api", "register", "begin"]) => register_begin(&request),
            (Method::Post, ["api", "register", "finish"]) => register_finish(&request),
            (Method::Post, ["api", "login", "begin"]) => login_begin(&request),
            (Method::Post, ["api", "login", "finish"]) => login_finish(&request),
            (Method::Get, ["api", "me"]) => me(&request),
            (Method::Post, ["api", "credentials", "delete"]) => credential_delete(&request),
            (Method::Post, ["api", "logout"]) => logout(&request),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
}

fn usage() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "passkey",
            "about": "passwordless sign-in with WebAuthn passkeys — the authenticator holds the private key, the server verifies a signature over a single-use challenge",
            "register": "POST /api/register/begin {username} then /api/register/finish {username, id, client_data_json, attestation_object}",
            "login": "POST /api/login/begin {username?} then /api/login/finish {id, client_data_json, authenticator_data, signature}",
            "session": "GET /api/me with `authorization: bearer <token>`",
            "credentials": "POST /api/credentials/delete {id}, POST /api/logout"
        })
        .to_string(),
    )
}

// ---- the relying party's identity (config, never the request) ---------------

fn cfg(key: &str, default: &str) -> String {
    config::get(key).ok().flatten().unwrap_or_else(|| default.to_string())
}

fn rp_id() -> String {
    cfg("rp-id", "localhost")
}
fn origin() -> String {
    cfg("origin", "http://localhost:3053")
}
fn require_uv() -> bool {
    cfg("require-uv", "false") == "true"
}

fn expectations(challenge: &str) -> wa::Expectations {
    wa::Expectations {
        rp_id: rp_id(),
        origin: origin(),
        challenge: challenge.to_string(),
        require_user_verification: require_uv(),
    }
}

fn rp_config() -> Outcome {
    Outcome::Json(
        200,
        json!({ "rp_id": rp_id(), "origin": origin(), "require_uv": require_uv() }).to_string(),
    )
}

// ---- challenges (unguessable, single-use, self-expiring) --------------------

fn b64u(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn random_b64u(n: u64) -> String {
    b64u(&random::get_random_bytes(n))
}

/// Mint a challenge and remember what it was issued for. Keyed BY the challenge,
/// so spending it is a delete — no way to use one twice even for a login where we
/// don't yet know the user.
fn issue_challenge(purpose: &str, username: &str, user_handle: &str) -> String {
    let challenge = random_b64u(32);
    let meta = json!({ "purpose": purpose, "username": username, "user_handle": user_handle }).to_string();
    let _ = cache::set(&format!("chal:{challenge}"), meta.as_bytes(), CHALLENGE_TTL);
    challenge
}

/// Spend a challenge: read it, delete it, and require it to have been issued for
/// this purpose. A second attempt with the same challenge finds nothing.
fn spend_challenge(challenge: &str, purpose: &str) -> Result<Value, Outcome> {
    let key = format!("chal:{challenge}");
    let raw = cache::get(&key)
        .ok()
        .flatten()
        .ok_or_else(|| Outcome::Err(400, "unknown or expired challenge".into()))?;
    let _ = cache::delete(&key);
    let meta: Value = serde_json::from_slice(&raw).unwrap_or_else(|_| json!({}));
    if meta["purpose"].as_str() != Some(purpose) {
        return Err(Outcome::Err(400, "challenge was issued for another ceremony".into()));
    }
    Ok(meta)
}

// ---- accounts + credentials -------------------------------------------------

fn now_secs() -> u64 {
    wall_clock::now().seconds
}

fn find_one(coll: &str, field: &str, value: &str) -> Option<(String, u64, Value)> {
    records::find_by(coll, field, &json!(value).to_string())
        .ok()?
        .into_iter()
        .next()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok().map(|v| (e.id, e.revision, v)))
}

fn account_of(username: &str) -> Option<Value> {
    find_one(ACCOUNTS, "username", username).map(|(_, _, v)| v)
}

fn credentials_of(username: &str) -> Vec<Value> {
    records::find_by(CREDENTIALS, "username", &json!(username).to_string())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .collect()
}

/// A credential as the WIT record, rebuilt from what we stored.
fn stored_credential(v: &Value) -> Option<wa::Credential> {
    Some(wa::Credential {
        id: v["id"].as_str()?.to_string(),
        public_key: STANDARD.decode(v["public_key"].as_str()?).ok()?,
        alg: v["alg"].as_i64()? as i32,
        sign_count: v["sign_count"].as_u64().unwrap_or(0) as u32,
        aaguid: v["aaguid"].as_str().unwrap_or_default().to_string(),
        user_verified: v["user_verified"].as_bool().unwrap_or(false),
        backup_eligible: v["backup_eligible"].as_bool().unwrap_or(false),
        backed_up: v["backed_up"].as_bool().unwrap_or(false),
        attestation_format: v["attestation_format"].as_str().unwrap_or_default().to_string(),
    })
}

/// What the UI shows about a passkey — never the key material.
fn credential_view(v: &Value) -> Value {
    json!({
        "id": v["id"], "aaguid": v["aaguid"], "alg": v["alg"],
        "sign_count": v["sign_count"], "created": v["created"], "last_used": v["last_used"],
        "user_verified": v["user_verified"], "backed_up": v["backed_up"],
        "backup_eligible": v["backup_eligible"], "attestation_format": v["attestation_format"],
        "synced": v["backed_up"].as_bool().unwrap_or(false)
    })
}

// ---- sessions ---------------------------------------------------------------

fn bearer(request: &IncomingRequest) -> Option<String> {
    let h = request.headers().get(&"authorization".to_string());
    let raw = String::from_utf8(h.into_iter().next()?).ok()?;
    raw.strip_prefix("bearer ")
        .or_else(|| raw.strip_prefix("Bearer "))
        .map(|t| t.trim().to_string())
}

/// The account this request is authenticated as, if any.
fn session_user(request: &IncomingRequest) -> Option<String> {
    let token = bearer(request)?;
    let s = sessions::get(&token).ok()?;
    String::from_utf8(s.data).ok()
}

fn mint_session(username: &str) -> Result<Value, Outcome> {
    sessions::create(username.as_bytes(), SESSION_TTL)
        .map(|s| json!({ "token": s.id, "expires": s.expires, "username": username }))
        .map_err(|e| Outcome::Err(500, format!("session: {e:?}")))
}

// ---- registration ----------------------------------------------------------

fn username_of(b: &Value) -> Result<String, Outcome> {
    let name = b["username"].as_str().unwrap_or_default().trim().to_lowercase();
    if name.is_empty() || name.len() > 64 {
        return Err(Outcome::Err(422, "username must be 1..64 characters".into()));
    }
    Ok(name)
}

fn register_begin(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let username = match username_of(&b) {
        Ok(u) => u,
        Err(o) => return o,
    };

    // Enrolling a passkey on an EXISTING account requires being signed in to it —
    // otherwise adding a credential is a complete account takeover.
    let existing = account_of(&username);
    if existing.is_some() && session_user(request).as_deref() != Some(username.as_str()) {
        return Outcome::Err(
            401,
            "account exists — sign in with an existing passkey to add another".into(),
        );
    }

    let user_handle = existing
        .as_ref()
        .and_then(|a| a["user_handle"].as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| random_b64u(16));
    // The handle rides along with the challenge, so the account we create on
    // finish stores exactly the handle the authenticator was told to remember.
    let challenge = issue_challenge("register", &username, &user_handle);

    // Exactly the `publicKey` options the browser needs, ready to pass to
    // navigator.credentials.create() once the SPA decodes the base64url fields.
    Outcome::Json(
        200,
        json!({
            "challenge": challenge,
            "rp": { "id": rp_id(), "name": "passkey" },
            "user": { "id": user_handle, "name": username, "displayName": username },
            // ES256 first (what nearly every authenticator picks), RS256 as a
            // fallback — the two `webauthn:verify` verifies.
            "pubKeyCredParams": [
                { "type": "public-key", "alg": -7 },
                { "type": "public-key", "alg": -257 }
            ],
            "authenticatorSelection": {
                "residentKey": "preferred",
                "userVerification": if require_uv() { "required" } else { "preferred" }
            },
            // Don't let one authenticator enrol twice on the same account.
            "excludeCredentials": credentials_of(&username)
                .iter()
                .map(|c| json!({ "type": "public-key", "id": c["id"] }))
                .collect::<Vec<_>>(),
            "attestation": "none",
            "timeout": 60000
        })
        .to_string(),
    )
}

fn register_finish(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let username = match username_of(&b) {
        Ok(u) => u,
        Err(o) => return o,
    };
    let (client_data, attestation) = match (field_bytes(&b, "client_data_json"), field_bytes(&b, "attestation_object")) {
        (Ok(c), Ok(a)) => (c, a),
        (Err(o), _) | (_, Err(o)) => return o,
    };
    let challenge = match b["challenge"].as_str() {
        Some(c) => c.to_string(),
        // The challenge is inside clientDataJSON too; requiring it explicitly
        // keeps `spend_challenge` in charge of single use.
        None => match challenge_from_client_data(&client_data) {
            Some(c) => c,
            None => return Outcome::Err(400, "no challenge in clientDataJSON".into()),
        },
    };
    let meta = match spend_challenge(&challenge, "register") {
        Ok(m) => m,
        Err(o) => return o,
    };
    if meta["username"].as_str() != Some(username.as_str()) {
        return Outcome::Err(400, "challenge was issued for another account".into());
    }

    let cred = match wa::register(&expectations(&challenge), &client_data, &attestation) {
        Ok(c) => c,
        Err(e) => return ceremony_error(e),
    };
    if find_one(CREDENTIALS, "id", &cred.id).is_some() {
        return Outcome::Err(409, "credential already registered".into());
    }

    // Create the account on first passkey (the account IS its credentials).
    if account_of(&username).is_none() {
        let handle = meta["user_handle"].as_str().unwrap_or_default();
        let acct = json!({ "username": username, "user_handle": handle, "created": now_secs() });
        if records::create(ACCOUNTS, &acct.to_string(), &["username".to_string()]).is_err() {
            return Outcome::Err(500, "could not create account".into());
        }
    }

    let doc = json!({
        "id": cred.id, "username": username,
        "public_key": STANDARD.encode(&cred.public_key), "alg": cred.alg,
        "sign_count": cred.sign_count, "aaguid": cred.aaguid,
        "user_verified": cred.user_verified, "backup_eligible": cred.backup_eligible,
        "backed_up": cred.backed_up, "attestation_format": cred.attestation_format,
        "created": now_secs(), "last_used": Value::Null
    });
    if records::create(CREDENTIALS, &doc.to_string(), &["id".to_string(), "username".to_string()]).is_err() {
        return Outcome::Err(500, "could not store credential".into());
    }

    match mint_session(&username) {
        Ok(mut s) => {
            s["credential"] = credential_view(&doc);
            Outcome::Json(201, s.to_string())
        }
        Err(o) => o,
    }
}

// ---- login -----------------------------------------------------------------

fn login_begin(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    // No username -> a DISCOVERABLE ceremony: the authenticator offers whichever
    // passkey it holds for this RP, and we learn who it is from the credential id.
    let username = b["username"].as_str().unwrap_or_default().trim().to_lowercase();
    let allow: Vec<Value> = if username.is_empty() {
        Vec::new()
    } else {
        credentials_of(&username)
            .iter()
            .map(|c| json!({ "type": "public-key", "id": c["id"] }))
            .collect()
    };
    if !username.is_empty() && allow.is_empty() {
        // Don't leak whether the account exists: answer with an empty list and
        // let the ceremony fail in the browser like any other unknown passkey.
        return Outcome::Json(
            200,
            json!({ "challenge": issue_challenge("login", "", ""), "rpId": rp_id(),
                    "allowCredentials": [], "userVerification": "preferred", "timeout": 60000 })
            .to_string(),
        );
    }
    Outcome::Json(
        200,
        json!({
            "challenge": issue_challenge("login", &username, ""),
            "rpId": rp_id(),
            "allowCredentials": allow,
            "userVerification": if require_uv() { "required" } else { "preferred" },
            "timeout": 60000
        })
        .to_string(),
    )
}

fn login_finish(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let id = b["id"].as_str().unwrap_or_default().to_string();
    let (client_data, auth_data, signature) = match (
        field_bytes(&b, "client_data_json"),
        field_bytes(&b, "authenticator_data"),
        field_bytes(&b, "signature"),
    ) {
        (Ok(c), Ok(a), Ok(s)) => (c, a, s),
        (Err(o), _, _) | (_, Err(o), _) | (_, _, Err(o)) => return o,
    };
    let challenge = match b["challenge"].as_str().map(|s| s.to_string()).or_else(|| challenge_from_client_data(&client_data)) {
        Some(c) => c,
        None => return Outcome::Err(400, "no challenge in clientDataJSON".into()),
    };
    let issued_for = match spend_challenge(&challenge, "login") {
        Ok(m) => m["username"].as_str().unwrap_or_default().to_string(),
        Err(o) => return o,
    };

    let (rec_id, revision, doc) = match find_one(CREDENTIALS, "id", &id) {
        Some(found) => found,
        None => return Outcome::Err(401, "unknown credential".into()),
    };
    let username = doc["username"].as_str().unwrap_or_default().to_string();
    // A challenge issued for a named account must not be used with someone
    // else's passkey. (A discoverable ceremony has no name to bind to.)
    if !issued_for.is_empty() && issued_for != username {
        return Outcome::Err(401, "credential does not belong to that account".into());
    }
    let cred = match stored_credential(&doc) {
        Some(c) => c,
        None => return Outcome::Err(500, "stored credential is corrupt".into()),
    };

    let assertion = match wa::authenticate(&expectations(&challenge), &cred, &client_data, &auth_data, &signature) {
        Ok(a) => a,
        Err(e) => return ceremony_error(e),
    };

    // Persist the new counter (and the backup state, which can change when a
    // passkey syncs to a second device).
    let mut next = doc.clone();
    next["sign_count"] = json!(assertion.sign_count);
    next["backed_up"] = json!(assertion.backed_up);
    next["last_used"] = json!(now_secs());
    let _ = records::update(CREDENTIALS, &rec_id, &next.to_string(), revision);

    match mint_session(&username) {
        Ok(mut s) => {
            s["user_verified"] = json!(assertion.user_verified);
            s["credential"] = credential_view(&next);
            Outcome::Json(200, s.to_string())
        }
        Err(o) => o,
    }
}

/// Map a verification failure onto HTTP. Everything is a 401 — a failed ceremony
/// is a failed authentication — but the reason is reported, because "wrong
/// origin" and "bad signature" mean very different things to an operator.
fn ceremony_error(e: wa::VerifyError) -> Outcome {
    let (reason, detail) = match e {
        wa::VerifyError::BadEncoding(m) => ("bad_encoding", m),
        wa::VerifyError::BadType(t) => ("bad_type", t),
        wa::VerifyError::ChallengeMismatch => ("challenge_mismatch", String::new()),
        wa::VerifyError::OriginMismatch(o) => ("origin_mismatch", o),
        wa::VerifyError::RpIdMismatch => ("rp_id_mismatch", String::new()),
        wa::VerifyError::UserNotPresent => ("user_not_present", String::new()),
        wa::VerifyError::UserNotVerified => ("user_not_verified", String::new()),
        wa::VerifyError::UnsupportedAlgorithm(a) => ("unsupported_algorithm", a.to_string()),
        wa::VerifyError::BadSignature => ("bad_signature", String::new()),
        wa::VerifyError::CounterRegressed(c) => ("counter_regressed", c.to_string()),
        wa::VerifyError::Malformed(m) => ("malformed", m),
    };
    Outcome::Json(401, json!({ "error": reason, "detail": detail }).to_string())
}

// ---- session-scoped reads --------------------------------------------------

fn me(request: &IncomingRequest) -> Outcome {
    let username = match session_user(request) {
        Some(u) => u,
        None => return Outcome::Err(401, "no session".into()),
    };
    let creds: Vec<Value> = credentials_of(&username).iter().map(credential_view).collect();
    Outcome::Json(200, json!({ "username": username, "credentials": creds }).to_string())
}

fn credential_delete(request: &IncomingRequest) -> Outcome {
    let username = match session_user(request) {
        Some(u) => u,
        None => return Outcome::Err(401, "no session".into()),
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let id = b["id"].as_str().unwrap_or_default();
    match find_one(CREDENTIALS, "id", id) {
        // Only your own passkeys, and never your last one (that would lock the
        // account out — there is no password to fall back to).
        Some((rec_id, _, doc)) if doc["username"].as_str() == Some(username.as_str()) => {
            if credentials_of(&username).len() <= 1 {
                return Outcome::Err(409, "that is your only passkey — enrol another first".into());
            }
            let _ = records::delete(CREDENTIALS, &rec_id);
            Outcome::Json(200, json!({ "ok": true, "id": id }).to_string())
        }
        _ => Outcome::Err(404, "not_found".into()),
    }
}

fn logout(request: &IncomingRequest) -> Outcome {
    if let Some(token) = bearer(request) {
        let _ = sessions::revoke(&token);
    }
    Outcome::Json(200, json!({ "ok": true }).to_string())
}

// ---- http plumbing ---------------------------------------------------------

/// A base64url (or base64) field the browser sent as an ArrayBuffer.
fn field_bytes(b: &Value, key: &str) -> Result<Vec<u8>, Outcome> {
    let s = b[key]
        .as_str()
        .ok_or_else(|| Outcome::Err(422, format!("{key} required (base64url)")))?;
    URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .or_else(|_| STANDARD.decode(s))
        .map_err(|e| Outcome::Err(400, format!("{key}: bad base64 ({e})")))
}

fn challenge_from_client_data(client_data: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(client_data).ok()?;
    v["challenge"].as_str().map(|s| s.to_string())
}

fn body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let raw = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
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
    let b = request.consume().map_err(|_| ())?;
    let stream = b.stream().map_err(|_| ())?;
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
    let (code, body) = match result {
        Outcome::Json(c, b) => (c, b),
        Outcome::Err(c, m) => (c, json!({ "error": m }).to_string()),
    };
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(code);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    let bytes = body.as_bytes();
    if !bytes.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in bytes.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
