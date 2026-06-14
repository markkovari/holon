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

/// Expected JWT issuer (`iss`). When set, tokens whose `iss` differs are
/// rejected. Empty (default) disables the check — convenient for local/dev,
/// but operators SHOULD set this in production to prevent cross-issuer reuse.
pub fn expected_issuer() -> String {
    get_str("expected-issuer", "")
}

/// Expected JWT audience (`aud`). When set, a token is accepted only if this
/// value appears in its `aud`. Empty (default) disables the check.
pub fn expected_audience() -> String {
    get_str("expected-audience", "")
}

/// Comma-separated allow-list of JWS algorithms. Pinning the algorithm prevents
/// algorithm-confusion attacks (e.g. a token forged with HS256 against a public
/// RSA key). Default allows the asymmetric algs only; add `HS256` explicitly to
/// enable shared-secret tokens (dev/test).
pub fn allowed_algs() -> Vec<String> {
    get_str("allowed-algs", "RS256,ES256")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Clock-skew tolerance (seconds) applied to `exp` / `nbf` checks. Default 60.
pub fn clock_skew() -> u64 {
    get_u64("clock-skew", 60)
}

/// Whether to emit structured (JSON) audit events for auth decisions to stderr
/// (host-captured / scrapable by OTel collectors). Default on. Set "false" off.
pub fn audit_enabled() -> bool {
    get_str("audit-enabled", "true") != "false"
}
