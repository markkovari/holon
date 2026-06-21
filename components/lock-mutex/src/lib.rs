//! `lock-mutex` — reference implementation of `lock:mutex`.
//!
//! Lease-based advisory mutual exclusion, backed by `wasi:keyvalue`. A holder
//! claims `key` for a TTL and gets a secret `token` + a monotonic `fence`;
//! holders renew before expiry, and a crashed holder's lease simply lapses
//! (TTL is the dead-man's switch). Advisory: the component answers "may I
//! proceed?" and gates release/renew by token — it does not police the resource.
//!
//! Storage layout (BUCKET = "default"):
//!   lk_{key}    the current lease: `owner\ttoken\texpires\tfence` (or absent).
//!   tt_{token}  reverse pointer token -> key, so `renew(token)` finds the lock
//!               without a key-scan API. Removed on release / takeover.
//!
//! Acquire takes over an EXPIRED lease (fence bumped, old token pointer left to
//! rot — it no longer matches the lock's token, so it's inert). A live lease
//! held by another owner yields `err(held)`.
//!
//! Re-entrant acquire by the SAME owner+still-live lease: treated as a fresh
//! acquire that REPLACES the lease (new token, fence bumped) — the prior token
//! becomes not-holder. (A caller wanting keep-alive should `renew`, not
//! re-acquire.)
//!
//! RACE CAVEAT (best-effort, NOT a correctness guarantee): acquire is
//! read-then-write. `wasi:keyvalue@0.2.0-draft` exposes no compare-and-swap, so
//! two simultaneous acquires of a free key can both win. This reference impl
//! assumes the store provides effective serialization (the single-writer relay
//! topology, or a backend with external CAS). A true distributed mutex needs
//! CAS on `lk_{key}`.

#[allow(warnings)]
mod bindings;

use bindings::exports::lock::mutex::mutex::{Guest, Lease, LockError};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::store as kv;
use bindings::wasi::random::random::get_random_bytes;

struct Component;

const BUCKET: &str = "default";

// ---- helpers ------------------------------------------------------------

fn now() -> u64 {
    wall_clock::now().seconds
}

/// A fresh 16-hex token from 8 random bytes.
fn new_token() -> String {
    get_random_bytes(8)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// Sanitize an arbitrary string to NATS-legal kv chars, with a 3-char prefix.
fn key_with(prefix: &str, s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 3);
    out.push_str(prefix);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn lk_key(key: &str) -> String {
    key_with("lk_", key)
}
fn tt_key(token: &str) -> String {
    key_with("tt_", token)
}

fn open() -> Result<kv::Bucket, LockError> {
    kv::open(BUCKET).map_err(|e| LockError::BackendUnavailable(format!("open: {e:?}")))
}

// ---- lease (de)serialization --------------------------------------------
//
// One tab-delimited line: `owner\ttoken\texpires\tfence`. owner is base64-free
// here (it's an app id); a tab in an owner would corrupt the line, so we reject
// tabs by replacing them — owners are opaque ids, not arbitrary bytes.

fn serialize(l: &Lease) -> String {
    format!(
        "{}\t{}\t{}\t{}",
        l.owner.replace('\t', " "),
        l.token,
        l.expires,
        l.fence,
    )
}

fn parse(key: &str, s: &str) -> Result<Lease, LockError> {
    let mut p = s.splitn(4, '\t');
    let owner = p.next().unwrap_or("").to_string();
    let token = p.next().unwrap_or("").to_string();
    let expires = p
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| LockError::BackendUnavailable("corrupt lease: expires".into()))?;
    let fence = p
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| LockError::BackendUnavailable("corrupt lease: fence".into()))?;
    Ok(Lease {
        key: key.to_string(),
        owner,
        token,
        expires,
        fence,
    })
}

fn load(bucket: &kv::Bucket, key: &str) -> Result<Option<Lease>, LockError> {
    match bucket.get(&lk_key(key)) {
        Ok(Some(bytes)) => {
            let s = String::from_utf8(bytes)
                .map_err(|_| LockError::BackendUnavailable("lease not utf-8".into()))?;
            Ok(Some(parse(key, &s)?))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(LockError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

fn store(bucket: &kv::Bucket, l: &Lease) -> Result<(), LockError> {
    bucket
        .set(&lk_key(&l.key), serialize(l).as_bytes())
        .map_err(|e| LockError::BackendUnavailable(format!("set: {e:?}")))?;
    // reverse pointer token -> key, for renew().
    bucket
        .set(&tt_key(&l.token), l.key.as_bytes())
        .map_err(|e| LockError::BackendUnavailable(format!("set tt: {e:?}")))
}

fn key_for_token(bucket: &kv::Bucket, token: &str) -> Result<Option<String>, LockError> {
    match bucket.get(&tt_key(token)) {
        Ok(Some(bytes)) => Ok(Some(
            String::from_utf8(bytes)
                .map_err(|_| LockError::BackendUnavailable("tt not utf-8".into()))?,
        )),
        Ok(None) => Ok(None),
        Err(e) => Err(LockError::BackendUnavailable(format!("get tt: {e:?}"))),
    }
}

// ---- guest --------------------------------------------------------------

impl Guest for Component {
    fn acquire(key: String, owner: String, ttl_seconds: u64) -> Result<Lease, LockError> {
        if ttl_seconds == 0 {
            return Err(LockError::InvalidTtl);
        }
        let bucket = open()?;
        let now = now();
        let prior = load(&bucket, &key)?;
        // A live lease held by ANYONE blocks (incl. the same owner — keep-alive
        // is `renew`, not re-acquire). An expired lease is taken over.
        let prior_fence = match &prior {
            Some(l) if l.expires > now => return Err(LockError::Held(l.clone())),
            Some(l) => l.fence, // expired -> take over, bump fence
            None => 0,
        };
        let lease = Lease {
            key: key.clone(),
            owner,
            token: new_token(),
            expires: now.saturating_add(ttl_seconds),
            fence: prior_fence.saturating_add(1),
        };
        store(&bucket, &lease)?;
        Ok(lease)
    }

    fn release(key: String, token: String) -> Result<(), LockError> {
        let bucket = open()?;
        match load(&bucket, &key)? {
            Some(mut l) if l.token == token => {
                // Tombstone: keep the record (so `fence` survives for the next
                // holder) but mark it free by zeroing the expiry and blanking
                // the token. Drop the token->key pointer so the old token is
                // inert. `holder` reads expires<=now as free; `acquire` takes
                // it over and bumps the retained fence.
                let old_token = l.token.clone();
                l.expires = 0;
                l.token = String::new();
                bucket
                    .set(&lk_key(&key), serialize(&l).as_bytes())
                    .map_err(|e| LockError::BackendUnavailable(format!("set tombstone: {e:?}")))?;
                bucket
                    .delete(&tt_key(&old_token))
                    .map_err(|e| LockError::BackendUnavailable(format!("delete tt: {e:?}")))
            }
            _ => Err(LockError::NotHolder),
        }
    }

    fn renew(token: String, ttl_seconds: u64) -> Result<Lease, LockError> {
        if ttl_seconds == 0 {
            return Err(LockError::InvalidTtl);
        }
        let bucket = open()?;
        let key = key_for_token(&bucket, &token)?.ok_or(LockError::NotHolder)?;
        match load(&bucket, &key)? {
            Some(mut l) if l.token == token => {
                // A lapsed lease cannot be renewed — it may already be held by
                // another acquirer.
                if l.expires <= now() {
                    return Err(LockError::NotHolder);
                }
                l.expires = now().saturating_add(ttl_seconds);
                store(&bucket, &l)?;
                Ok(l)
            }
            _ => Err(LockError::NotHolder),
        }
    }

    fn holder(key: String) -> Result<Option<Lease>, LockError> {
        let bucket = open()?;
        match load(&bucket, &key)? {
            // a lapsed lease reads as free.
            Some(l) if l.expires > now() => Ok(Some(Lease {
                token: String::new(), // never reveal the secret on peek
                ..l
            })),
            _ => Ok(None),
        }
    }
}

bindings::export!(Component with_types_in bindings);
