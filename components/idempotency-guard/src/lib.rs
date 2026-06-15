//! `idempotency-guard` — reference implementation of `idempotency:guard`.
//!
//! Store-and-replay request deduplication, backed by `wasi:keyvalue`.
//! State per key: `id_{key}` -> one line, two shapes:
//!   pending:{created}                       reserved, operation in flight
//!   done:{created}:{status}:{base64 body}   operation completed, result stored
//! A `pending` record older than its ttl is treated as expired and reclaimed,
//! so a crashed in-flight request never wedges a key forever.
//!
//! Config (wasi:config/runtime):
//!   default-ttl   reservation / record lifetime, seconds (default 86400)
//!                 the per-call `ttl-seconds` overrides this when non-zero.

#[allow(warnings)]
mod bindings;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use bindings::exports::idempotency::guard::store::{CachedResponse, Guest, IdemError};
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

enum Record {
    Pending { created: u64, nonce: String },
    Done { status: u16, body: Vec<u8> },
}

fn now() -> u64 {
    wall_clock::now().seconds
}

/// A fresh 16-hex reservation nonce — used to detect a racing first-caller.
fn nonce() -> String {
    get_random_bytes(8)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// Sanitize an opaque key to NATS-legal kv chars (same scheme as the
/// rate-limiter's `rl_key` / auth-guard's `kv::safe`), prefixed with `id_`.
fn id_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 3);
    out.push_str("id_");
    for b in key.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn open() -> Result<kv::Bucket, IdemError> {
    kv::open(BUCKET).map_err(|e| IdemError::BackendUnavailable(format!("open: {e:?}")))
}

fn load(bucket: &kv::Bucket, key: &str) -> Result<Option<Record>, IdemError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => {
            let s = String::from_utf8(bytes)
                .map_err(|_| IdemError::BackendUnavailable("value not utf-8".into()))?;
            Ok(Some(parse(&s)?))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(IdemError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

fn parse(s: &str) -> Result<Record, IdemError> {
    // `pending:{created}:{nonce}` | `done:{created}:{status}:{b64}`
    if let Some(rest) = s.strip_prefix("pending:") {
        let (created_str, nonce) = rest.split_once(':').unwrap_or((rest, ""));
        let created = created_str.parse().unwrap_or(0);
        return Ok(Record::Pending {
            created,
            nonce: nonce.to_string(),
        });
    }
    if let Some(rest) = s.strip_prefix("done:") {
        // rest = {created}:{status}:{b64}
        let mut parts = rest.splitn(3, ':');
        let _created = parts.next();
        let status = parts
            .next()
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| IdemError::BackendUnavailable("corrupt record: status".into()))?;
        let b64 = parts.next().unwrap_or("");
        let body = B64
            .decode(b64)
            .map_err(|_| IdemError::BackendUnavailable("corrupt record: body".into()))?;
        return Ok(Record::Done { status, body });
    }
    Err(IdemError::BackendUnavailable("corrupt record".into()))
}

fn set(bucket: &kv::Bucket, key: &str, body: &str) -> Result<(), IdemError> {
    bucket
        .set(key, body.as_bytes())
        .map_err(|e| IdemError::BackendUnavailable(format!("set: {e:?}")))
}

impl Guest for Component {
    fn begin(key: String, ttl_seconds: u64) -> Result<Option<CachedResponse>, IdemError> {
        let ttl = ttl_or_default(ttl_seconds);
        let bucket = open()?;
        let k = id_key(&key);
        let now = now();
        match load(&bucket, &k)? {
            // already completed -> replay the stored response.
            Some(Record::Done { status, body }) => Ok(Some(CachedResponse { status, body })),
            // in flight and still within ttl -> concurrent duplicate.
            Some(Record::Pending { created, .. }) if now.saturating_sub(created) < ttl => {
                Err(IdemError::InProgress)
            }
            // absent, or a stale pending reservation -> try to reserve.
            //
            // BEST-EFFORT race mitigation (NOT a correctness guarantee): write
            // our pending record with a unique nonce, then re-read. If the
            // stored nonce is not ours, a concurrent first-caller wrote after us
            // and we yield to them with `in-progress`. This catches the common
            // interleaving but a tight set/set/read/read ordering can still let
            // two callers both proceed. A true fix needs compare-and-swap, which
            // wasi:keyvalue@0.2.0-draft does not expose (only `increment`).
            _ => {
                let mine = nonce();
                set(&bucket, &k, &format!("pending:{now}:{mine}"))?;
                match load(&bucket, &k)? {
                    Some(Record::Pending { nonce, .. }) if nonce == mine => Ok(None),
                    // someone overwrote our reservation -> they win.
                    Some(Record::Pending { .. }) => Err(IdemError::InProgress),
                    // raced with a completer -> replay theirs.
                    Some(Record::Done { status, body }) => {
                        Ok(Some(CachedResponse { status, body }))
                    }
                    // disappeared (forget) -> we proceed.
                    None => Ok(None),
                }
            }
        }
    }

    fn complete(key: String, status: u16, body: Vec<u8>) -> Result<(), IdemError> {
        let bucket = open()?;
        let k = id_key(&key);
        let line = format!("done:{}:{}:{}", now(), status, B64.encode(&body));
        set(&bucket, &k, &line)
    }

    fn forget(key: String) -> Result<(), IdemError> {
        let bucket = open()?;
        bucket
            .delete(&id_key(&key))
            .map_err(|e| IdemError::BackendUnavailable(format!("delete: {e:?}")))
    }
}

bindings::export!(Component with_types_in bindings);
