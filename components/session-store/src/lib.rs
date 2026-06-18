//! `session-store` — reference implementation of `session:store`.
//!
//! Generic server-side session + CSRF capability, backed by `wasi:keyvalue`.
//! The client only ever carries the opaque session id (a cookie); the payload
//! and the CSRF token live here, server-side.
//!
//! State per session: `sess_{id}` -> one line, four colon-separated fields:
//!   {created}:{expires}:{csrf}:{base64url data}
//! `created`/`expires` are unix seconds; `csrf` is already url-safe text; the
//! app-defined `data` blob is base64url-encoded (URL_SAFE_NO_PAD) so it never
//! collides with the field delimiter.
//!
//! Config (wasi:config/runtime):
//!   default-ttl   session lifetime, seconds (default 86400). The per-call
//!                 `ttl-seconds` overrides this when non-zero.

#[allow(warnings)]
mod bindings;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;

use bindings::exports::session::store::store::{Guest, Session, SessionError};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::config::runtime as config;
use bindings::wasi::keyvalue::store as kv;
use bindings::wasi::random::random::get_random_bytes;

struct Component;

const BUCKET: &str = "default";

// ---- config -------------------------------------------------------------

fn default_ttl() -> u64 {
    match config::get("default-ttl") {
        Ok(Some(v)) => v.parse().unwrap_or(86400),
        _ => 86400,
    }
}

fn ttl_or_default(ttl_seconds: u64) -> u64 {
    if ttl_seconds == 0 {
        default_ttl()
    } else {
        ttl_seconds
    }
}

// ---- record state -------------------------------------------------------

struct Record {
    created: u64,
    expires: u64,
    csrf: String,
    data: Vec<u8>,
}

fn now() -> u64 {
    wall_clock::now().seconds
}

/// A fresh 256-bit (32-byte) unguessable token, base64url (no padding).
fn token() -> String {
    B64.encode(get_random_bytes(32))
}

/// Sanitize an opaque session id to NATS-legal kv chars (same scheme as the
/// idempotency-guard's `id_key` / rate-limiter's `rl_key`), prefixed `sess_`.
fn sess_key(id: &str) -> String {
    let mut out = String::with_capacity(id.len() + 5);
    out.push_str("sess_");
    for b in id.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn open() -> Result<kv::Bucket, SessionError> {
    kv::open(BUCKET).map_err(|e| SessionError::BackendUnavailable(format!("open: {e:?}")))
}

/// Load a record by sanitized key; `Ok(None)` if absent.
fn load(bucket: &kv::Bucket, key: &str) -> Result<Option<Record>, SessionError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => {
            let s = String::from_utf8(bytes)
                .map_err(|_| SessionError::BackendUnavailable("value not utf-8".into()))?;
            Ok(Some(parse(&s)?))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(SessionError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

fn parse(s: &str) -> Result<Record, SessionError> {
    // `{created}:{expires}:{csrf}:{b64url data}`
    let mut parts = s.splitn(4, ':');
    let created = parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| SessionError::BackendUnavailable("corrupt record: created".into()))?;
    let expires = parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| SessionError::BackendUnavailable("corrupt record: expires".into()))?;
    let csrf = parts
        .next()
        .ok_or_else(|| SessionError::BackendUnavailable("corrupt record: csrf".into()))?
        .to_string();
    let b64 = parts
        .next()
        .ok_or_else(|| SessionError::BackendUnavailable("corrupt record: data".into()))?;
    let data = B64
        .decode(b64)
        .map_err(|_| SessionError::BackendUnavailable("corrupt record: data".into()))?;
    Ok(Record {
        created,
        expires,
        csrf,
        data,
    })
}

fn serialize(r: &Record) -> String {
    format!(
        "{}:{}:{}:{}",
        r.created,
        r.expires,
        r.csrf,
        B64.encode(&r.data)
    )
}

fn set(bucket: &kv::Bucket, key: &str, body: &str) -> Result<(), SessionError> {
    bucket
        .set(key, body.as_bytes())
        .map_err(|e| SessionError::BackendUnavailable(format!("set: {e:?}")))
}

/// Load a record only if it exists AND is not expired; else `err(not-found)`.
fn load_live(bucket: &kv::Bucket, key: &str) -> Result<Record, SessionError> {
    match load(bucket, key)? {
        Some(r) if now() < r.expires => Ok(r),
        _ => Err(SessionError::NotFound),
    }
}

fn to_session(id: &str, r: &Record) -> Session {
    Session {
        id: id.to_string(),
        data: r.data.clone(),
        created: r.created,
        expires: r.expires,
        csrf_token: r.csrf.clone(),
    }
}

/// Constant-time byte compare: XOR-accumulate over equal-length inputs.
/// Differing lengths fail immediately (the length is not itself secret here).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

impl Guest for Component {
    fn create(data: Vec<u8>, ttl_seconds: u64) -> Result<Session, SessionError> {
        let ttl = ttl_or_default(ttl_seconds);
        let bucket = open()?;
        let now = now();
        // 256-bit opaque id, separate 256-bit csrf token.
        let id = token();
        let record = Record {
            created: now,
            expires: now + ttl,
            csrf: token(),
            data,
        };
        set(&bucket, &sess_key(&id), &serialize(&record))?;
        Ok(to_session(&id, &record))
    }

    fn get(id: String) -> Result<Session, SessionError> {
        let bucket = open()?;
        // load_live applies the `now >= expires => not-found` rule; does NOT
        // extend expiry.
        let record = load_live(&bucket, &sess_key(&id))?;
        Ok(to_session(&id, &record))
    }

    fn update_data(id: String, data: Vec<u8>) -> Result<(), SessionError> {
        let bucket = open()?;
        let k = sess_key(&id);
        let mut record = load_live(&bucket, &k)?;
        // rewrite payload, keep id/csrf/created/expires.
        record.data = data;
        set(&bucket, &k, &serialize(&record))
    }

    fn refresh(id: String, ttl_seconds: u64) -> Result<Session, SessionError> {
        let ttl = ttl_or_default(ttl_seconds);
        let bucket = open()?;
        let k = sess_key(&id);
        let mut record = load_live(&bucket, &k)?;
        // slide the expiry forward to now + ttl.
        record.expires = now() + ttl;
        set(&bucket, &k, &serialize(&record))?;
        Ok(to_session(&id, &record))
    }

    fn verify_csrf(id: String, token: String) -> Result<(), SessionError> {
        let bucket = open()?;
        let record = load_live(&bucket, &sess_key(&id))?;
        if constant_time_eq(token.as_bytes(), record.csrf.as_bytes()) {
            Ok(())
        } else {
            Err(SessionError::CsrfMismatch)
        }
    }

    fn revoke(id: String) -> Result<(), SessionError> {
        let bucket = open()?;
        // idempotent: deleting an unknown key succeeds.
        bucket
            .delete(&sess_key(&id))
            .map_err(|e| SessionError::BackendUnavailable(format!("delete: {e:?}")))
    }
}

bindings::export!(Component with_types_in bindings);
