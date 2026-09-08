//! Shared-secret auth for the ADR-0095 daemons' loopback HTTP.
//!
//! Loopback binding alone is not a boundary. The egress allow-list
//! (`host/src/tenant.rs`) gates what a wasm component's OWN sandbox may dial —
//! it says nothing about who may reach a daemon that's already listening.
//! Any other local process — another tenant's component whose own manifest
//! happens to allow the same port, or anything else on the box with loopback
//! access at all — could otherwise ask `comp-docker` for the container list,
//! `comp-clipboard` for the clipboard's contents, `comp-cron` for the full
//! crontab, with no caller-identity check of any kind. Fixed, documented port
//! numbers (8000-8011) make this scan-free.
//!
//! `--token` closes it: the wasm component reads `<name>-token` from
//! `wasi:config` and sends it as `Authorization: Bearer <token>`; the daemon
//! refuses anything else. A daemon started with no `--token` still answers
//! every request — same shape as an empty `--allow-path`, but inverted:
//! there, absence is the safe default (refuses everything); here, absence is
//! convenience over security (a single-user dev box has no other local
//! process to defend against), so it stays permissive but says so loudly
//! rather than silently.

use axum::body::Body;
use axum::extract::{Extension, Request};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

/// A daemon comparing an operator-issued token against what a request sent is
/// exactly the place a timing side-channel would matter — this is the one
/// comparison across these daemons worth being constant-time about.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Axum middleware: refuses any request whose `Authorization: Bearer <token>`
/// doesn't match, when a token was configured. Install with
/// `.layer(axum::middleware::from_fn(require_token)).layer(Extension(Arc::new(token)))`
/// — the `Extension` carries the expected token in, independent of whatever
/// state type a daemon's own routes already use.
pub async fn require_token(
    Extension(expected): Extension<Arc<Option<String>>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if let Some(want) = expected.as_deref() {
        let ok = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(|got| constant_time_eq(got.as_bytes(), want.as_bytes()))
            .unwrap_or(false);
        if !ok {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }
    next.run(req).await
}

/// `--token <value>` on a command line is `ps`-readable by any local user —
/// the exact reason `comp-host`'s own unit reads its config through
/// `LoadCredential` rather than argv (see `render_unit`'s doc). `--token-file`
/// is the same fix here: a systemd `LoadCredential` path the unit points at,
/// read once at startup rather than passed as a value. `--token` still exists
/// for a bare `cargo run` during local development, where there is no `ps`
/// argv boundary worth protecting on a single-user box.
pub fn resolve_token(direct: Option<String>, file: Option<std::path::PathBuf>) -> Option<String> {
    if let Some(path) = file {
        return std::fs::read_to_string(&path)
            .map(|s| s.trim().to_string())
            .inspect_err(|e| eprintln!("--token-file {}: {e}", path.display()))
            .ok();
    }
    direct
}

/// The startup warning every daemon prints when run without `--token` — one
/// place to word it so all twelve say the same thing.
pub fn warn_if_unauthenticated(bin: &str, token: &Option<String>) {
    if token.is_none() {
        eprintln!(
            "{bin}: no --token given — any local process reaching 127.0.0.1 can use this \
             daemon with no check at all. Fine for a single-user dev box; set one otherwise."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_tokens_match_and_different_lengths_never_do() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"wrong!"));
        assert!(!constant_time_eq(b"short", b"muchlonger"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn a_token_file_wins_over_a_direct_token_and_is_trimmed() {
        // Test scaffolding, not a security-sensitive file: torn down at the
        // end of this test. Same pattern as fs-watcher's daemon test.
        let path = std::env::temp_dir().join(format!("daemon-auth-test-{}.token", std::process::id())); // nosemgrep: rust.lang.security.temp-dir.temp-dir
        std::fs::write(&path, "  from-file\n").expect("write");
        assert_eq!(
            resolve_token(Some("from-argv".into()), Some(path.clone())),
            Some("from-file".into())
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_missing_token_file_falls_back_to_none_not_the_direct_value() {
        // A configured file that cannot be read is a misconfiguration worth
        // failing loudly on, not silently falling back to a weaker value.
        let path = std::env::temp_dir().join("daemon-auth-test-does-not-exist.token"); // nosemgrep: rust.lang.security.temp-dir.temp-dir
        assert_eq!(resolve_token(Some("from-argv".into()), Some(path)), None);
    }

    #[test]
    fn no_file_means_the_direct_value_is_used() {
        assert_eq!(resolve_token(Some("from-argv".into()), None), Some("from-argv".into()));
        assert_eq!(resolve_token(None, None), None);
    }
}
