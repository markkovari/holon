//! Token id generation and key prefixes.

use crate::bindings::wasi::random::random::get_random_bytes;

/// Prefix marking an opaque, session-backed access token. The authorizer uses
/// this to route to a kv session lookup instead of JWS verification.
pub const ACCESS_PREFIX: &str = "sess_";
/// Prefix for refresh tokens (rotated on each `refresh`).
pub const REFRESH_PREFIX: &str = "ref_";

/// Generate a URL-safe random id with the given prefix (128 bits of entropy).
pub fn new_id(prefix: &str) -> String {
    let bytes = get_random_bytes(16);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}{hex}")
}
