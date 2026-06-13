//! Stateless JWS (JWT) verification: RS256, ES256, HS256.
//!
//! Signing key material is resolved from the issuer's JWKS (cached in kv by
//! `oidc_client`). HS256 uses a shared secret supplied via the "hs256-secret"
//! kv key (set out-of-band by config), enabling local/test tokens.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::bindings::exports::auth::identity::types::{AuthError, Claims};
use crate::bindings::wasi::clocks::wall_clock;
use crate::{kv, oidc_client};

#[derive(Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

#[derive(Deserialize)]
struct Payload {
    iss: String,
    sub: String,
    #[serde(default)]
    aud: Aud,
    exp: u64,
    #[serde(default)]
    iat: u64,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    scp: Option<Vec<String>>,
    #[serde(flatten)]
    rest: std::collections::BTreeMap<String, serde_json::Value>,
}

/// `aud` may be a string or an array of strings per RFC 7519.
#[derive(Deserialize, Default)]
#[serde(untagged)]
enum Aud {
    #[default]
    None,
    One(String),
    Many(Vec<String>),
}

impl Aud {
    fn into_vec(self) -> Vec<String> {
        match self {
            Aud::None => Vec::new(),
            Aud::One(s) => vec![s],
            Aud::Many(v) => v,
        }
    }
}

fn b64(part: &str) -> Result<Vec<u8>, AuthError> {
    URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| AuthError::Malformed("jwt segment not base64url".into()))
}

pub fn verify(token: &str) -> Result<Claims, AuthError> {
    let mut parts = token.split('.');
    let (h, p, s) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s), None) => (h, p, s),
        _ => return Err(AuthError::Malformed("jwt must have 3 segments".into())),
    };

    let header: Header = serde_json::from_slice(&b64(h)?)
        .map_err(|e| AuthError::Malformed(format!("jwt header: {e}")))?;
    let payload: Payload = serde_json::from_slice(&b64(p)?)
        .map_err(|e| AuthError::Malformed(format!("jwt payload: {e}")))?;
    let signature = b64(s)?;

    let signing_input = format!("{h}.{p}");

    match header.alg.as_str() {
        "RS256" => verify_rs256(&payload.iss, header.kid.as_deref(), &signing_input, &signature)?,
        "ES256" => verify_es256(&payload.iss, header.kid.as_deref(), &signing_input, &signature)?,
        "HS256" => verify_hs256(&signing_input, &signature)?,
        other => return Err(AuthError::InvalidToken(format!("unsupported alg {other}"))),
    }

    // Expiry check.
    let now = wall_clock::now().seconds;
    if payload.exp != 0 && payload.exp < now {
        return Err(AuthError::Expired);
    }

    Ok(payload_to_claims(payload))
}

fn payload_to_claims(p: Payload) -> Claims {
    // Scopes: prefer OAuth `scope` (space-delimited) then `scp` (array).
    let scopes = if let Some(scope) = &p.scope {
        scope.split_whitespace().map(str::to_string).collect()
    } else {
        p.scp.clone().unwrap_or_default()
    };
    let raw = p
        .rest
        .iter()
        .map(|(k, v)| {
            let val = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (k.clone(), val)
        })
        .collect();
    Claims {
        iss: p.iss,
        sub: p.sub,
        aud: p.aud.into_vec(),
        exp: p.exp,
        iat: p.iat,
        scopes,
        raw,
    }
}

// ---- signature verification --------------------------------------------

fn verify_rs256(
    issuer: &str,
    kid: Option<&str>,
    signing_input: &str,
    signature: &[u8],
) -> Result<(), AuthError> {
    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::signature::Verifier;
    use rsa::{BigUint, RsaPublicKey};

    let (n, e) = oidc_client::jwks_rsa_key(issuer, kid)?;
    let key = RsaPublicKey::new(BigUint::from_bytes_be(&n), BigUint::from_bytes_be(&e))
        .map_err(|e| AuthError::Internal(format!("rsa key: {e}")))?;
    let vk = VerifyingKey::<Sha256>::new(key);
    let sig = Signature::try_from(signature)
        .map_err(|_| AuthError::InvalidToken("bad rs256 signature".into()))?;
    vk.verify(signing_input.as_bytes(), &sig)
        .map_err(|_| AuthError::InvalidToken("rs256 verify failed".into()))
}

fn verify_es256(
    issuer: &str,
    kid: Option<&str>,
    signing_input: &str,
    signature: &[u8],
) -> Result<(), AuthError> {
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::{Signature, VerifyingKey};

    let (x, y) = oidc_client::jwks_ec_key(issuer, kid)?;
    let mut sec1 = Vec::with_capacity(65);
    sec1.push(0x04); // uncompressed point
    sec1.extend_from_slice(&x);
    sec1.extend_from_slice(&y);
    let vk = VerifyingKey::from_sec1_bytes(&sec1)
        .map_err(|e| AuthError::Internal(format!("ec key: {e}")))?;
    let sig = Signature::from_slice(signature)
        .map_err(|_| AuthError::InvalidToken("bad es256 signature".into()))?;
    vk.verify(signing_input.as_bytes(), &sig)
        .map_err(|_| AuthError::InvalidToken("es256 verify failed".into()))
}

/// HS256 via a shared secret in kv. Constant-time-ish compare via fixed digest.
fn verify_hs256(signing_input: &str, signature: &[u8]) -> Result<(), AuthError> {
    let secret = kv::get("hs256-secret")?
        .ok_or_else(|| AuthError::InvalidToken("no hs256 secret configured".into()))?;
    // Minimal HMAC-SHA256 (RFC 2104) using sha2 directly.
    let mac = hmac_sha256(secret.as_bytes(), signing_input.as_bytes());
    if mac.len() == signature.len() && constant_eq(&mac, signature) {
        Ok(())
    } else {
        Err(AuthError::InvalidToken("hs256 verify failed".into()))
    }
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 64;
    let mut k = if key.len() > BLOCK {
        Sha256::digest(key).to_vec()
    } else {
        key.to_vec()
    };
    k.resize(BLOCK, 0);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::new().chain_update(&ipad).chain_update(msg).finalize();
    Sha256::new().chain_update(&opad).chain_update(inner).finalize().to_vec()
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
