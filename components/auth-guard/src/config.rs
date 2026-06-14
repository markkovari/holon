//! Deployment policy, read from `wasi:config/runtime` (set per-deployment in the
//! component's config — see infra/k8s/app.yaml). Every knob has a sane default
//! so the component runs with zero config; operators override as needed.
//!
//! Keys (all string-valued):
//!   session-ttl        seconds a session lives        (default 3600)
//!   password-min-len   minimum password length        (default 8)
//!   jwks-cache-ttl     seconds to cache OIDC JWKS      (default 3600)  [reserved]
//!   default-tenant     tenant when none in token/req   (default "")

use crate::bindings::wasi::config::runtime;

fn get_u64(key: &str, default: u64) -> u64 {
    match runtime::get(key) {
        Ok(Some(v)) => v.parse().unwrap_or(default),
        _ => default,
    }
}

fn get_str(key: &str, default: &str) -> String {
    match runtime::get(key) {
        Ok(Some(v)) => v,
        _ => default.to_string(),
    }
}

/// Session lifetime in seconds.
pub fn session_ttl() -> u64 {
    get_u64("session-ttl", 3600)
}

/// Minimum acceptable password length.
pub fn password_min_len() -> usize {
    get_u64("password-min-len", 8) as usize
}

/// Seconds to cache OIDC discovery + JWKS.
pub fn jwks_cache_ttl() -> u64 {
    get_u64("jwks-cache-ttl", 3600)
}

/// Tenant to assume when a token/request carries none.
pub fn default_tenant() -> String {
    get_str("default-tenant", "")
}
