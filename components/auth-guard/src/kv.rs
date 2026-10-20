use crate::bindings::exports::auth::identity::types::AuthError;
use crate::bindings::wasi::keyvalue::store;

// keyvalue-nats resolves `open(<link-name>)`, and our link is named `default`.
const BUCKET: &str = "default";

fn open() -> Result<store::Bucket, AuthError> {
    store::open(BUCKET).map_err(|e| AuthError::BackendUnavailable(format!("kv open: {e:?}")))
}

/// JetStream KV keys allow only `[-/_=A-Za-z0-9]`; other bytes become `_XX` (`_` itself escaped).
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

#[cfg(test)]
mod tests {
    use super::safe;

    #[test]
    fn passes_through_allowed_chars() {
        assert_eq!(safe("abcXYZ012-/="), "abcXYZ012-/=");
    }

    #[test]
    fn escapes_nats_illegal_chars() {
        assert_eq!(safe("user:acme:a@b.com"), "user_3Aacme_3Aa_40b_2Ecom");
    }

    #[test]
    fn escape_char_itself_is_escaped_so_mapping_is_injective() {
        assert_ne!(safe("a_3A"), safe("a:"));
        assert_eq!(safe("_"), "_5F");
    }

    #[test]
    fn distinct_inputs_stay_distinct() {
        assert_ne!(safe("a:b"), safe("a_b"));
    }
}
