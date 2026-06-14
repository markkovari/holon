//! Stateless JWS (JWT) verification: RS256, ES256, HS256.
//!
//! Signing key material is resolved from the issuer's JWKS (cached in kv by
//! `oidc_client`). HS256 uses a shared secret supplied via the "hs256-secret"
//! kv key (set out-of-band by config), enabling local/test tokens.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;
use sha2::Sha256;

use crate::bindings::exports::auth::identity::types::{AuthError, Claims};
use crate::bindings::wasi::clocks::wall_clock;
use crate::{config, kv, oidc_client};

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
    /// Not-before: token is invalid until this time.
    #[serde(default)]
    nbf: u64,
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

    fn as_slice(&self) -> &[String] {
        match self {
            Aud::None => &[],
            Aud::One(s) => std::slice::from_ref(s),
            Aud::Many(v) => v,
        }
    }
}

fn b64(part: &str) -> Result<Vec<u8>, AuthError> {
    URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| AuthError::Malformed("jwt segment not base64url".into()))
}

/// Claim-validation policy. Built from `config` at runtime; constructed directly
/// in tests so claim checks are exercised without a wasi:config host.
pub struct Policy {
    pub allowed_algs: Vec<String>,
    pub expected_issuer: String,
    pub expected_audience: String,
    pub clock_skew: u64,
}

impl Policy {
    fn from_config() -> Self {
        Policy {
            allowed_algs: config::allowed_algs(),
            expected_issuer: config::expected_issuer(),
            expected_audience: config::expected_audience(),
            clock_skew: config::clock_skew(),
        }
    }
}

pub fn verify(token: &str) -> Result<Claims, AuthError> {
    let policy = Policy::from_config();
    let now = wall_clock::now().seconds;

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

    // 1. Pin the algorithm BEFORE verifying — refuse any alg not on the
    //    allow-list. This blocks algorithm-confusion (e.g. HS256-signed token
    //    forged against an RSA public key when only RS256 is expected).
    if !policy.allowed_algs.iter().any(|a| a == &header.alg) {
        return Err(AuthError::InvalidToken(format!(
            "algorithm {} not allowed",
            header.alg
        )));
    }

    // 2. Verify the signature with the pinned algorithm.
    let signing_input = format!("{h}.{p}");
    match header.alg.as_str() {
        "RS256" => verify_rs256(&payload.iss, header.kid.as_deref(), &signing_input, &signature)?,
        "ES256" => verify_es256(&payload.iss, header.kid.as_deref(), &signing_input, &signature)?,
        "HS256" => verify_hs256(&signing_input, &signature)?,
        other => return Err(AuthError::InvalidToken(format!("unsupported alg {other}"))),
    }

    // 3. Validate the claims (iss / aud / exp / nbf).
    validate_claims(&payload, now, &policy).map_err(ClaimError::into_auth)?;

    Ok(payload_to_claims(payload))
}

/// Reason a token's claims were rejected. A bindings-free enum so claim
/// validation is pure and host-unit-testable (the wasm `AuthError` type only
/// exists for the wasm target). Mapped to `AuthError` at the call site.
#[derive(Debug, PartialEq, Eq)]
pub enum ClaimError {
    NotYetValid,
    Expired,
    UnexpectedIssuer,
    AudienceNotAccepted,
}

impl ClaimError {
    fn into_auth(self) -> AuthError {
        match self {
            ClaimError::NotYetValid => AuthError::InvalidToken("token not yet valid (nbf)".into()),
            ClaimError::Expired => AuthError::Expired,
            ClaimError::UnexpectedIssuer => AuthError::InvalidToken("unexpected issuer".into()),
            ClaimError::AudienceNotAccepted => {
                AuthError::InvalidToken("audience not accepted".into())
            }
        }
    }
}

/// Pure claim validation — no I/O, no bindings types — so it is unit-tested
/// directly (see tests below). Checks, in order: not-before, expiry, issuer,
/// audience. Time checks allow `clock_skew` seconds of tolerance.
fn validate_claims(p: &Payload, now: u64, policy: &Policy) -> Result<(), ClaimError> {
    let skew = policy.clock_skew;

    if p.nbf != 0 && p.nbf > now.saturating_add(skew) {
        return Err(ClaimError::NotYetValid);
    }
    if p.exp != 0 && p.exp.saturating_add(skew) < now {
        return Err(ClaimError::Expired);
    }
    if !policy.expected_issuer.is_empty() && p.iss != policy.expected_issuer {
        return Err(ClaimError::UnexpectedIssuer);
    }
    if !policy.expected_audience.is_empty()
        && !p.aud.as_slice().iter().any(|a| a == &policy.expected_audience)
    {
        return Err(ClaimError::AudienceNotAccepted);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(iss: &str, aud: Aud, exp: u64, nbf: u64) -> Payload {
        Payload {
            iss: iss.into(),
            sub: "s".into(),
            aud,
            exp,
            iat: 0,
            nbf,
            scope: None,
            scp: None,
            rest: Default::default(),
        }
    }

    fn policy(iss: &str, aud: &str) -> Policy {
        Policy {
            allowed_algs: vec!["RS256".into()],
            expected_issuer: iss.into(),
            expected_audience: aud.into(),
            clock_skew: 60,
        }
    }

    const NOW: u64 = 1_000_000;

    #[test]
    fn accepts_valid_claims() {
        let p = payload("https://idp", Aud::One("svc-a".into()), NOW + 3600, 0);
        assert!(validate_claims(&p, NOW, &policy("https://idp", "svc-a")).is_ok());
    }

    #[test]
    fn rejects_expired() {
        let p = payload("https://idp", Aud::None, NOW - 3600, 0);
        assert_eq!(
            validate_claims(&p, NOW, &policy("", "")),
            Err(ClaimError::Expired)
        );
    }

    #[test]
    fn allows_expiry_within_skew() {
        // expired 30s ago, skew 60 -> still valid.
        let p = payload("https://idp", Aud::None, NOW - 30, 0);
        assert!(validate_claims(&p, NOW, &policy("", "")).is_ok());
    }

    #[test]
    fn rejects_not_yet_valid() {
        let p = payload("https://idp", Aud::None, NOW + 3600, NOW + 3600);
        assert_eq!(
            validate_claims(&p, NOW, &policy("", "")),
            Err(ClaimError::NotYetValid)
        );
    }

    #[test]
    fn rejects_wrong_issuer() {
        let p = payload("https://evil", Aud::One("svc-a".into()), NOW + 60, 0);
        assert_eq!(
            validate_claims(&p, NOW, &policy("https://idp", "svc-a")),
            Err(ClaimError::UnexpectedIssuer)
        );
    }

    #[test]
    fn rejects_wrong_audience() {
        // token for svc-b presented where svc-a is expected -> the core
        // audience-confusion check.
        let p = payload("https://idp", Aud::One("svc-b".into()), NOW + 60, 0);
        assert_eq!(
            validate_claims(&p, NOW, &policy("https://idp", "svc-a")),
            Err(ClaimError::AudienceNotAccepted)
        );
    }

    #[test]
    fn accepts_audience_in_array() {
        let p = payload(
            "https://idp",
            Aud::Many(vec!["svc-x".into(), "svc-a".into()]),
            NOW + 60,
            0,
        );
        assert!(validate_claims(&p, NOW, &policy("https://idp", "svc-a")).is_ok());
    }

    #[test]
    fn empty_policy_skips_iss_aud() {
        // no expected iss/aud configured -> those checks are disabled.
        let p = payload("anything", Aud::None, NOW + 60, 0);
        assert!(validate_claims(&p, NOW, &policy("", "")).is_ok());
    }
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

/// HS256 via a shared secret in kv, using the vetted `hmac` crate (RFC 2104)
/// with its built-in constant-time `verify_slice` — no hand-rolled MAC.
fn verify_hs256(signing_input: &str, signature: &[u8]) -> Result<(), AuthError> {
    use hmac::{Hmac, Mac};

    let secret = kv::get("hs256-secret")?
        .ok_or_else(|| AuthError::InvalidToken("no hs256 secret configured".into()))?;

    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|e| AuthError::Internal(format!("hmac key: {e}")))?;
    mac.update(signing_input.as_bytes());
    mac.verify_slice(signature)
        .map_err(|_| AuthError::InvalidToken("hs256 verify failed".into()))
}
