//! `upload-policy` — reference implementation of `upload:policy`.
//!
//! File-upload gatekeeping: validate a declared content-type + size against a
//! config allow-policy, then mint a short-lived, HMAC-signed upload ticket the
//! client presents back. A stand-in for a presigned URL — the *signing* is the
//! reusable part; the real storage URL is the deploy's concern (pairs with
//! `blob:store`). Also verifies a returned ticket (signature + expiry).
//!
//! Ticket wire format: `{base64url-nopad payload}.{checksum-hex}` where
//!   payload  = `{b64url object-key}:{b64url content-type}:{size}:{expires}`
//!   checksum = first 8 bytes of HMAC-SHA256(secret, payload), hex.
//! `redeem` recomputes the HMAC over the decoded payload and constant-time
//! compares it to the presented checksum, so a tampered payload or a wrong key
//! fails closed as `invalid-ticket`.
//!
//! Config (wasi:config/runtime):
//!   allowed-types   comma-separated content-type allow-list; empty/unset = all
//!   max-size        bytes, default 10485760 (10 MiB)
//!   ticket-ttl      seconds, default 300
//!   ticket-secret   HMAC key; default "upload-default-secret"
//!                   — production MUST set this to a real secret.

#[allow(warnings)]
mod bindings;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use bindings::exports::upload::policy::gate::{Grant, Guest, PolicyError, Ticket};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::config::runtime as config;
use bindings::wasi::random::random::get_random_bytes;

struct Component;

type HmacSha256 = Hmac<Sha256>;

// ---- config -------------------------------------------------------------

const DEFAULT_MAX_SIZE: u64 = 10_485_760; // 10 MiB
const DEFAULT_TTL: u64 = 300;
const DEFAULT_SECRET: &str = "upload-default-secret";

/// The configured content-type allow-list (comma-separated). Empty = allow all.
fn allowed_types() -> Vec<String> {
    match config::get("allowed-types") {
        Ok(Some(v)) => v
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn max_size() -> u64 {
    match config::get("max-size") {
        Ok(Some(v)) => v.parse().unwrap_or(DEFAULT_MAX_SIZE),
        _ => DEFAULT_MAX_SIZE,
    }
}

fn ticket_ttl() -> u64 {
    match config::get("ticket-ttl") {
        Ok(Some(v)) => v.parse().unwrap_or(DEFAULT_TTL),
        _ => DEFAULT_TTL,
    }
}

fn ticket_secret() -> String {
    match config::get("ticket-secret") {
        Ok(Some(v)) if !v.is_empty() => v,
        _ => DEFAULT_SECRET.to_string(),
    }
}

// ---- helpers ------------------------------------------------------------

fn now() -> u64 {
    wall_clock::now().seconds
}

/// 16 hex chars from 8 random bytes — the ticket / object id.
fn random16hex() -> String {
    get_random_bytes(8)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// Sanitize a tenant into key-legal chars (same byte scheme as the other
/// components): keep `[A-Za-z0-9-/=]`, escape anything else as `_HH`.
fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

/// First 8 bytes of HMAC-SHA256(secret, payload), lowercase hex.
fn checksum(secret: &str, payload: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(payload.as_bytes());
    let tag = mac.finalize().into_bytes();
    tag[..8].iter().map(|x| format!("{x:02x}")).collect()
}

/// Build the signed payload string for an authorized upload.
fn payload(object_key: &str, content_type: &str, size: u64, expires: u64) -> String {
    format!(
        "{}:{}:{}:{}",
        B64.encode(object_key),
        B64.encode(content_type),
        size,
        expires
    )
}

// ---- guest --------------------------------------------------------------

impl Guest for Component {
    fn check(content_type: String, size: u64) -> Result<(), PolicyError> {
        let allowed = allowed_types();
        if !allowed.is_empty() && !allowed.iter().any(|t| t == &content_type) {
            return Err(PolicyError::TypeNotAllowed(content_type));
        }
        let max = max_size();
        if size > max {
            return Err(PolicyError::TooLarge(max));
        }
        Ok(())
    }

    fn authorize(
        tenant: String,
        content_type: String,
        size: u64,
        ttl_seconds: u64,
    ) -> Result<Ticket, PolicyError> {
        Self::check(content_type.clone(), size)?;

        let ttl = if ttl_seconds > 0 {
            ttl_seconds
        } else {
            ticket_ttl()
        };
        let now = now();
        let expires = now.saturating_add(ttl);

        let object_key = format!("{}/{}", sanitize(&tenant), random16hex());

        let payload = payload(&object_key, &content_type, size, expires);
        let checksum = checksum(&ticket_secret(), &payload);
        let token = format!("{}.{}", B64.encode(payload.as_bytes()), checksum);

        Ok(Ticket {
            token,
            object_key,
            expires,
        })
    }

    fn redeem(token: String) -> Result<Grant, PolicyError> {
        // token = {base64url-nopad payload}.{checksum-hex}
        let (b64_payload, presented) = token
            .split_once('.')
            .ok_or(PolicyError::InvalidTicket)?;

        let payload_bytes = B64
            .decode(b64_payload)
            .map_err(|_| PolicyError::InvalidTicket)?;
        let payload =
            String::from_utf8(payload_bytes).map_err(|_| PolicyError::InvalidTicket)?;

        // Verify signature (constant-time over the hex strings).
        let expected = checksum(&ticket_secret(), &payload);
        if !ct_eq(expected.as_bytes(), presented.as_bytes()) {
            return Err(PolicyError::InvalidTicket);
        }

        // payload = {b64 object-key}:{b64 content-type}:{size}:{expires}
        let mut parts = payload.split(':');
        let object_key = parts
            .next()
            .and_then(|s| B64.decode(s).ok())
            .and_then(|b| String::from_utf8(b).ok())
            .ok_or(PolicyError::InvalidTicket)?;
        let content_type = parts
            .next()
            .and_then(|s| B64.decode(s).ok())
            .and_then(|b| String::from_utf8(b).ok())
            .ok_or(PolicyError::InvalidTicket)?;
        let size: u64 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or(PolicyError::InvalidTicket)?;
        let expires: u64 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or(PolicyError::InvalidTicket)?;

        if expires <= now() {
            return Err(PolicyError::InvalidTicket);
        }

        Ok(Grant {
            object_key,
            content_type,
            max_size: size,
        })
    }
}

/// Length-checked constant-time byte comparison (no early-out on first diff).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

bindings::export!(Component with_types_in bindings);
