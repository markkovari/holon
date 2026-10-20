//! Advisory lease mutex on wasi:keyvalue. Best-effort: keyvalue@0.2.0-draft has no CAS,
//! so two simultaneous acquires of a free key can both win.

#[allow(warnings)]
mod bindings;

use bindings::exports::lock::mutex::mutex::{Guest, Lease, LockError};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::store as kv;
use bindings::wasi::random::random::get_random_bytes;

struct Component;

const BUCKET: &str = "default";

fn now() -> u64 {
    wall_clock::now().seconds
}

fn new_token() -> String {
    get_random_bytes(8).iter().map(|x| format!("{x:02x}")).collect()
}

/// NATS-legal kv chars only.
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

// Tabs in an owner are replaced: they would corrupt the tab-delimited line.

fn serialize(l: &Lease) -> String {
    format!("{}\t{}\t{}\t{}", l.owner.replace('\t', " "), l.token, l.expires, l.fence,)
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
    Ok(Lease { key: key.to_string(), owner, token, expires, fence })
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

impl Guest for Component {
    fn acquire(key: String, owner: String, ttl_seconds: u64) -> Result<Lease, LockError> {
        if ttl_seconds == 0 {
            return Err(LockError::InvalidTtl);
        }
        let bucket = open()?;
        let now = now();
        let prior = load(&bucket, &key)?;
        // A live lease blocks even its own owner (keep-alive is `renew`); an expired one is taken over.
        let prior_fence = match &prior {
            Some(l) if l.expires > now => return Err(LockError::Held(l.clone())),
            Some(l) => l.fence,
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
                // Tombstone, not delete: `fence` must survive for the next holder; dropping tt_ makes the old token inert.
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
                // A lapsed lease cannot be renewed: it may already be held by another acquirer.
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
            Some(l) if l.expires > now() => Ok(Some(Lease {
                token: String::new(), // never reveal the secret on peek
                ..l
            })),
            _ => Ok(None),
        }
    }
}

bindings::export!(Component with_types_in bindings);
