//! `webhook-ingest` — verify an inbound webhook's HMAC signature, then dedup a redelivery
//! so `webhook-relay` uses `webhook:sign` + `idempotency:guard` instead.

#[allow(warnings)]
mod bindings;

use hmac::{Hmac, Mac};
use sha2::Sha256;

use bindings::exports::webhook::ingest::verifier::{Guest, IngestError, Verdict};
use bindings::idempotency::guard::store as idem;
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";
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
            Ok(None) => {
                let _ = idem::complete(&delivery_id, 200, &[]);
                Ok(Verdict { accepted: true, replay: false })
            }
            Ok(Some(_)) => Ok(Verdict { accepted: false, replay: true }),
            // A concurrent duplicate mid-flight is also a replay here.
            Err(idem::IdemError::InProgress) => Ok(Verdict { accepted: false, replay: true }),
            Err(idem::IdemError::BackendUnavailable(m)) => {
                Err(IngestError::BackendUnavailable(format!("idempotency: {m}")))
            }
        }
    }
}

bindings::export!(Component with_types_in bindings);
