//! Thin wrapper over `wasi:keyvalue/store` for the "auth" bucket.

use crate::bindings::exports::auth::identity::types::AuthError;
use crate::bindings::wasi::keyvalue::store;

// The keyvalue-nats provider registers its store under the LINK NAME and
// resolves `store.open(<link-name>)` to the JS bucket from link config. Our
// keyvalue link is named `default`, so we open "default".
const BUCKET: &str = "default";

fn open() -> Result<store::Bucket, AuthError> {
    store::open(BUCKET).map_err(|e| AuthError::BackendUnavailable(format!("kv open: {e:?}")))
}

/// NATS JetStream KV keys may only contain `[-/_=A-Za-z0-9]` and must not be
/// empty or start/end with `.`. Our logical keys use `:`, `@`, `.` (emails,
/// namespacing), so map any disallowed byte to a `_XX` hex escape. Deterministic
/// and collision-free (the escape char `_` is itself escaped).
fn safe(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for b in key.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

/// Get a UTF-8 string value, or None if the key is absent.
pub fn get(key: &str) -> Result<Option<String>, AuthError> {
    let bucket = open()?;
    match bucket.get(&safe(key)) {
        Ok(Some(bytes)) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| AuthError::Internal("kv value not utf-8".into())),
        Ok(None) => Ok(None),
        Err(e) => Err(AuthError::BackendUnavailable(format!("kv get: {e:?}"))),
    }
}

pub fn set(key: &str, value: &str) -> Result<(), AuthError> {
    let bucket = open()?;
    bucket
        .set(&safe(key), value.as_bytes())
        .map_err(|e| AuthError::BackendUnavailable(format!("kv set: {e:?}")))
}

pub fn delete(key: &str) -> Result<(), AuthError> {
    let bucket = open()?;
    bucket
        .delete(&safe(key))
        .map_err(|e| AuthError::BackendUnavailable(format!("kv delete: {e:?}")))
}
