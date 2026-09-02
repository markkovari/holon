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
//! ## Nothing imports this any more, and that is on purpose
//!
//! `webhook-relay` was the only consumer and has moved to `webhook:sign/signer::verify`
//! plus `idempotency:guard`. Not because either half of this component is wrong — the
//! HMAC check and the dedup both work — but because `ingest` does them in ONE call,
//! and that shape cannot express what a relay needs: everything that can still fail
//! happens after the mark, so a refused delivery left its id burnt and the sender's
//! retry came back `200 {"replay": true}` for an event that was never queued.
//!
//! The mechanism is visible below: `ingest` calls `idem::begin` and then
//! `idem::complete` immediately, on the next line. `idempotency:guard` is a
//! three-step protocol — reserve, then commit or release — and collapsing it into one
//! call is what removes the caller's ability to release. The capability was always
//! there; the shape of this interface hid it.
//!
//! The same collapse shows up once more: `Err(in-progress)` is reported to the caller
//! as `replay: true`. A concurrent duplicate is told "already handled" while the other
//! request may still fail. `webhook-relay` now answers 409 for that case, which is
//! what `idempotency:guard`'s own contract names it.
//!
//! So this stays as a worked example of chaining two capabilities, and as the right
//! answer for a caller whose accept path cannot fail after the mark. It is the wrong
//! answer for one whose can, and a caller cannot tell from the signature which it is —
//! which is the part worth remembering if this grows a second consumer.

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

/// Decode lowercase/uppercase hex into bytes.
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
        // 1. signature gate — reject before touching the dedup store.
        let secret = fetch_secret(&secret_ref)?;
        if !verify_hmac(&payload, &signature_hex, &secret)? {
            return Err(IngestError::BadSignature);
        }

        // 2. dedup on delivery-id via the composed idempotency capability.
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
