//! Thin wrapper over `wasi:keyvalue/store` for the "auth" bucket.

use crate::bindings::exports::auth::identity::types::AuthError;
use crate::bindings::wasi::keyvalue::store;

const BUCKET: &str = "auth";

fn open() -> Result<store::Bucket, AuthError> {
    store::open(BUCKET).map_err(|e| AuthError::BackendUnavailable(format!("kv open: {e:?}")))
}

/// Get a UTF-8 string value, or None if the key is absent.
pub fn get(key: &str) -> Result<Option<String>, AuthError> {
    let bucket = open()?;
    match bucket.get(key) {
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
        .set(key, value.as_bytes())
        .map_err(|e| AuthError::BackendUnavailable(format!("kv set: {e:?}")))
}

pub fn delete(key: &str) -> Result<(), AuthError> {
    let bucket = open()?;
    bucket
        .delete(key)
        .map_err(|e| AuthError::BackendUnavailable(format!("kv delete: {e:?}")))
}
