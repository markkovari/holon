//! `webhook-ingest` — verify an inbound webhook's HMAC signature, then dedup a redelivery
//!
//! Two composed capabilities behind one call:
//!   1. HMAC-SHA256 signature verification (vetted `hmac` crate, constant-time
//!      `verify_slice` — same pattern as auth-guard's HS256 path).
//!   2. Replay dedup via the imported `idempotency:guard/store` (a SEPARATE
//!      component plugged in with wac).
//!
//! The signing secret is read from a generic kv store at `secret-ref`.
//!
//! Nothing imports this any more, on purpose: `webhook-relay` moved to
//! `webhook:sign/signer::verify` plus `idempotency:guard`, because `ingest` does verify and dedup
//! in ONE call, and a relay needs everything that can still fail to happen after the mark.
//! Collapsing the reserve/commit/release protocol (`idem::begin` then `idem::complete` on the
//! next line) removes the caller's ability to release, so a refused delivery left its id burnt
//! and the retry came back `200 {"replay": true}`. For the same reason `Err(in-progress)` is
//! reported as `replay: true`; `webhook-relay` answers 409, as `idempotency:guard`'s contract
//! names it.
//! Kept as a worked example of chaining two capabilities, and right for a caller whose accept
//! path cannot fail after the mark.

#[allow(warnings)]
mod bindings;

use hmac::{Hmac, Mac};
use sha2::Sha256;

use bindings::exports::webhook::ingest::verifier::{Guest, IngestError, Verdict};
use bindings::idempotency::guard::store as idem;
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";
/// Dedup reservation lifetime — a delivery-id seen within this window is a replay.
const DEDUP_TTL: u64 = 86400;

fn kv_err(ctx: &str) -> IngestError {
    IngestError::BackendUnavailable(ctx.to_string())
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

fn fetch_secret(secret_ref: &str) -> Result<Vec<u8>, IngestError> {
    let bucket = kv::open(BUCKET).map_err(|e| kv_err(&format!("open: {e:?}")))?;
    match bucket.get(secret_ref) {
        Ok(Some(bytes)) => Ok(bytes),
        Ok(None) => Err(kv_err("secret not found")),
        Err(e) => Err(kv_err(&format!("get: {e:?}"))),
    }
}

/// Constant-time HMAC-SHA256 check of `payload` against `signature-hex`.
fn verify_hmac(payload: &[u8], signature_hex: &str, secret: &[u8]) -> Result<bool, IngestError> {
    let expected = match from_hex(signature_hex) {
        Some(b) => b,
        None => return Ok(false), // malformed signature -> reject (not an error)
    };
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret).map_err(|e| kv_err(&format!("hmac key: {e}")))?;
    mac.update(payload);
    Ok(mac.verify_slice(&expected).is_ok())
}

impl Guest for Component {
    fn ingest(
        payload: Vec<u8>,
        signature_hex: String,
        secret_ref: String,
        delivery_id: String,
    ) -> Result<Verdict, IngestError> {
        let secret = fetch_secret(&secret_ref)?;
        if !verify_hmac(&payload, &signature_hex, &secret)? {
            return Err(IngestError::BadSignature);
        }

        match idem::begin(&delivery_id, DEDUP_TTL) {
            // first time: reserve, mark complete, accept.
            Ok(None) => {
                let _ = idem::complete(&delivery_id, 200, &[]);
                Ok(Verdict { accepted: true, replay: false })
            }
            // already completed -> replay.
            Ok(Some(_)) => Ok(Verdict { accepted: false, replay: true }),
            // a concurrent duplicate is mid-flight -> also a replay for our purposes.
            Err(idem::IdemError::InProgress) => Ok(Verdict { accepted: false, replay: true }),
            Err(idem::IdemError::BackendUnavailable(m)) => {
                Err(IngestError::BackendUnavailable(format!("idempotency: {m}")))
            }
        }
    }
}

bindings::export!(Component with_types_in bindings);
