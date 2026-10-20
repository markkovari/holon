//! Every knob has a default, so the component runs with zero config.

use crate::bindings::wasi::config::store;

fn get_u64(key: &str, default: u64) -> u64 {
    match store::get(key) {
        Ok(Some(v)) => v.parse().unwrap_or(default),
        _ => default,
    }
}

fn get_str(key: &str, default: &str) -> String {
    match store::get(key) {
        Ok(Some(v)) => v,
        _ => default.to_string(),
    }
}

pub fn session_ttl() -> u64 {
    get_u64("session-ttl", 3600)
}

pub fn password_min_len() -> usize {
    get_u64("password-min-len", 8) as usize
}

pub fn jwks_cache_ttl() -> u64 {
    get_u64("jwks-cache-ttl", 3600)
}

pub fn default_tenant() -> String {
    get_str("default-tenant", "")
}

/// Empty disables the check; production should set it to prevent cross-issuer reuse.
pub fn expected_issuer() -> String {
    get_str("expected-issuer", "")
}

/// Empty disables the check.
pub fn expected_audience() -> String {
    get_str("expected-audience", "")
}

/// Pinned against algorithm confusion; asymmetric only by default, add `HS256` for dev/test.
pub fn allowed_algs() -> Vec<String> {
    get_str("allowed-algs", "RS256,ES256")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn clock_skew() -> u64 {
    get_u64("clock-skew", 60)
}

pub fn audit_enabled() -> bool {
    get_str("audit-enabled", "true") != "false"
}
