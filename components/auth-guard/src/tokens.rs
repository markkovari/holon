use crate::bindings::wasi::random::random::get_random_bytes;

/// Routes the authorizer to a kv session lookup instead of JWS verification.
pub const ACCESS_PREFIX: &str = "sess_";
pub const REFRESH_PREFIX: &str = "ref_";

pub fn new_id(prefix: &str) -> String {
    let bytes = get_random_bytes(16);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}{hex}")
}
