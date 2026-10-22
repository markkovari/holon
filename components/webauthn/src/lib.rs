//! `webauthn` — the server half of passkey login — verify a WebAuthn registration and an assertion
//! Skipping any single check turns a passkey into a bearer token.

#[allow(warnings)]
mod bindings;
mod cbor;

use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use sha2::{Digest, Sha256};

use bindings::exports::webauthn::verify::verifier::{
    Assertion, Credential, Expectations, Guest, VerifyError,
};
use cbor::Cbor;

struct Component;

/// COSE algorithm identifiers (IANA COSE Algorithms registry).
const ES256: i64 = -7;
const RS256: i64 = -257;

// authData flag bits (WebAuthn §6.1).
const FLAG_UP: u8 = 1 << 0; // user present
const FLAG_UV: u8 = 1 << 2; // user verified
const FLAG_BE: u8 = 1 << 3; // backup eligible
const FLAG_BS: u8 = 1 << 4; // backed up (synced)
const FLAG_AT: u8 = 1 << 6; // attested credential data present

impl Guest for Component {
    fn register(
        exp: Expectations,
        client_data_json: Vec<u8>,
        attestation_object: Vec<u8>,
    ) -> Result<Credential, VerifyError> {
        check_client_data(&client_data_json, "webauthn.create", &exp)?;

        let (att, _) = cbor::decode(&attestation_object).map_err(VerifyError::BadEncoding)?;
        let fmt = att.get("fmt").and_then(Cbor::as_text).unwrap_or("").to_string();
        let auth_data = att
            .get("authData")
            .and_then(Cbor::as_bytes)
            .ok_or_else(|| VerifyError::Malformed("attestation object has no authData".into()))?
            .to_vec();

        let ad = parse_auth_data(&auth_data)?;
        check_ceremony_flags(&ad, &exp)?;

        let (cred_id, cose_bytes) = match (ad.cred_id.as_ref(), ad.cose.as_ref()) {
            (Some(id), Some(cose)) if !id.is_empty() => (id.clone(), cose.clone()),
            _ => return Err(VerifyError::Malformed("no attested credential data".into())),
        };
        let (cose, _) = cbor::decode(&cose_bytes).map_err(VerifyError::BadEncoding)?;
        let alg = cose
            .get_int(3)
            .and_then(Cbor::as_i64)
            .ok_or_else(|| VerifyError::Malformed("COSE key has no alg".into()))?;
        if alg != ES256 && alg != RS256 {
            return Err(VerifyError::UnsupportedAlgorithm(alg as i32));
        }
        // Fail here rather than at first login if the key itself is unusable.
        verifying_key(&cose_bytes, alg)?;

        // `packed` SELF-attestation signs what a login signs, so it is checked; an x5c chain is a trust
        // decision deliberately not made (see the WIT doc).
        if fmt == "packed" {
            if let (Some(att_stmt), None) =
                (att.get("attStmt"), att.get("attStmt").and_then(|s| s.get("x5c")))
            {
                if let Some(sig) = att_stmt.get("sig").and_then(Cbor::as_bytes) {
                    let mut signed = auth_data.clone();
                    signed.extend_from_slice(&Sha256::digest(&client_data_json));
                    verify_signature(&cose_bytes, alg, &signed, sig)?;
                }
            }
        }

        Ok(Credential {
            id: URL_SAFE_NO_PAD.encode(&cred_id),
            public_key: cose_bytes,
            alg: alg as i32,
            sign_count: ad.sign_count,
            aaguid: ad.aaguid.map(hex).unwrap_or_default(),
            user_verified: ad.flags & FLAG_UV != 0,
            backup_eligible: ad.flags & FLAG_BE != 0,
            backed_up: ad.flags & FLAG_BS != 0,
            attestation_format: fmt,
        })
    }

    fn authenticate(
        exp: Expectations,
        cred: Credential,
        client_data_json: Vec<u8>,
        authenticator_data: Vec<u8>,
        signature: Vec<u8>,
    ) -> Result<Assertion, VerifyError> {
        check_client_data(&client_data_json, "webauthn.get", &exp)?;

        let ad = parse_auth_data(&authenticator_data)?;
        check_ceremony_flags(&ad, &exp)?;

        let alg = cred.alg as i64;
        if alg != ES256 && alg != RS256 {
            return Err(VerifyError::UnsupportedAlgorithm(cred.alg));
        }
        let mut signed = authenticator_data.clone();
        signed.extend_from_slice(&Sha256::digest(&client_data_json));
        verify_signature(&cred.public_key, alg, &signed, &signature)?;

        // Both zero: the authenticator has no counter (permitted, common for platform passkeys).
        if !(ad.sign_count == 0 && cred.sign_count == 0) && ad.sign_count <= cred.sign_count {
            return Err(VerifyError::CounterRegressed(ad.sign_count));
        }

        Ok(Assertion {
            sign_count: ad.sign_count,
            user_verified: ad.flags & FLAG_UV != 0,
            backed_up: ad.flags & FLAG_BS != 0,
        })
    }
}

fn check_client_data(json: &[u8], want_type: &str, exp: &Expectations) -> Result<(), VerifyError> {
    let v: serde_json::Value = serde_json::from_slice(json)
        .map_err(|e| VerifyError::BadEncoding(format!("clientDataJSON: {e}")))?;

    let ty = v["type"].as_str().unwrap_or_default();
    if ty != want_type {
        return Err(VerifyError::BadType(ty.to_string()));
    }
    // Compare bytes, so padding differences between libraries can't fail a valid ceremony.
    let got = b64url(v["challenge"].as_str().unwrap_or_default())?;
    let want = b64url(&exp.challenge)?;
    if got.is_empty() || got != want {
        return Err(VerifyError::ChallengeMismatch);
    }
    let origin = v["origin"].as_str().unwrap_or_default();
    if origin != exp.origin {
        return Err(VerifyError::OriginMismatch(origin.to_string()));
    }
    Ok(())
}

/// With or without padding: browsers omit it, some client libraries add it back.
fn b64url(s: &str) -> Result<Vec<u8>, VerifyError> {
    let trimmed = s.trim_end_matches('=');
    URL_SAFE_NO_PAD
        .decode(trimmed)
        .or_else(|_| URL_SAFE.decode(s))
        .map_err(|e| VerifyError::BadEncoding(format!("base64url: {e}")))
}

struct AuthData {
    rp_id_hash: [u8; 32],
    flags: u8,
    sign_count: u32,
    aaguid: Option<[u8; 16]>,
    cred_id: Option<Vec<u8>>,
    /// The COSE public key, exactly the bytes the authenticator sent.
    cose: Option<Vec<u8>>,
}

/// WebAuthn §6.1: `rpIdHash(32) || flags(1) || signCount(4) [|| aaguid(16) ||
/// credentialIdLength(2) || credentialId || COSEPublicKey]`.
fn parse_auth_data(buf: &[u8]) -> Result<AuthData, VerifyError> {
    if buf.len() < 37 {
        return Err(VerifyError::Malformed(format!("authData is {} bytes, need >= 37", buf.len())));
    }
    let mut rp_id_hash = [0u8; 32];
    rp_id_hash.copy_from_slice(&buf[..32]);
    let flags = buf[32];
    let sign_count = u32::from_be_bytes([buf[33], buf[34], buf[35], buf[36]]);

    let mut ad =
        AuthData { rp_id_hash, flags, sign_count, aaguid: None, cred_id: None, cose: None };
    if flags & FLAG_AT == 0 {
        return Ok(ad); // an assertion carries no credential
    }
    if buf.len() < 55 {
        return Err(VerifyError::Malformed("attested credential data truncated".into()));
    }
    let mut aaguid = [0u8; 16];
    aaguid.copy_from_slice(&buf[37..53]);
    let id_len = u16::from_be_bytes([buf[53], buf[54]]) as usize;
    let id_end = 55 + id_len;
    if id_len == 0 || id_len > 1023 || buf.len() < id_end {
        return Err(VerifyError::Malformed(format!("credential id length {id_len} does not fit")));
    }
    // The COSE key runs to the end unless extensions follow: use exactly the bytes it consumed.
    let (_, used) = cbor::decode(&buf[id_end..])
        .map_err(|e| VerifyError::BadEncoding(format!("COSE key: {e}")))?;
    ad.aaguid = Some(aaguid);
    ad.cred_id = Some(buf[55..id_end].to_vec());
    ad.cose = Some(buf[id_end..id_end + used].to_vec());
    Ok(ad)
}

fn check_ceremony_flags(ad: &AuthData, exp: &Expectations) -> Result<(), VerifyError> {
    if ad.rp_id_hash[..] != Sha256::digest(exp.rp_id.as_bytes())[..] {
        return Err(VerifyError::RpIdMismatch);
    }
    if ad.flags & FLAG_UP == 0 {
        return Err(VerifyError::UserNotPresent);
    }
    if exp.require_user_verification && ad.flags & FLAG_UV == 0 {
        return Err(VerifyError::UserNotVerified);
    }
    Ok(())
}

enum Key {
    Es256(p256::ecdsa::VerifyingKey),
    Rs256(Box<rsa::RsaPublicKey>),
}

/// COSE_Key (RFC 8152): EC2 has `crv`(-1) `x`(-2) `y`(-3); RSA has `n`(-1) `e`(-2).
fn verifying_key(cose_bytes: &[u8], alg: i64) -> Result<Key, VerifyError> {
    let (cose, _) = cbor::decode(cose_bytes).map_err(VerifyError::BadEncoding)?;
    let field = |label: i64, what: &str| -> Result<Vec<u8>, VerifyError> {
        cose.get_int(label)
            .and_then(Cbor::as_bytes)
            .map(|b| b.to_vec())
            .ok_or_else(|| VerifyError::Malformed(format!("COSE key has no {what}")))
    };
    match alg {
        ES256 => {
            if cose.get_int(-1).and_then(Cbor::as_i64) != Some(1) {
                return Err(VerifyError::Malformed("ES256 key is not on P-256".into()));
            }
            let (x, y) = (field(-2, "x")?, field(-3, "y")?);
            if x.len() != 32 || y.len() != 32 {
                return Err(VerifyError::Malformed("P-256 coordinates are not 32 bytes".into()));
            }
            let mut sec1 = Vec::with_capacity(65);
            sec1.push(0x04); // uncompressed point
            sec1.extend_from_slice(&x);
            sec1.extend_from_slice(&y);
            p256::ecdsa::VerifyingKey::from_sec1_bytes(&sec1)
                .map(Key::Es256)
                .map_err(|e| VerifyError::Malformed(format!("P-256 key: {e}")))
        }
        RS256 => {
            let (n, e) = (field(-1, "n")?, field(-2, "e")?);
            rsa::RsaPublicKey::new(rsa::BigUint::from_bytes_be(&n), rsa::BigUint::from_bytes_be(&e))
                .map(|k| Key::Rs256(Box::new(k)))
                .map_err(|e| VerifyError::Malformed(format!("RSA key: {e}")))
        }
        other => Err(VerifyError::UnsupportedAlgorithm(other as i32)),
    }
}

fn verify_signature(
    cose_bytes: &[u8],
    alg: i64,
    message: &[u8],
    sig: &[u8],
) -> Result<(), VerifyError> {
    match verifying_key(cose_bytes, alg)? {
        Key::Es256(vk) => {
            use p256::ecdsa::signature::Verifier;
            // WebAuthn ES256 signatures are ASN.1 DER (unlike JWS, which is r||s).
            let s = p256::ecdsa::Signature::from_der(sig).map_err(|_| VerifyError::BadSignature)?;
            vk.verify(message, &s).map_err(|_| VerifyError::BadSignature)
        }
        Key::Rs256(key) => {
            use rsa::signature::Verifier;
            let vk = rsa::pkcs1v15::VerifyingKey::<Sha256>::new(*key);
            let s =
                rsa::pkcs1v15::Signature::try_from(sig).map_err(|_| VerifyError::BadSignature)?;
            vk.verify(message, &s).map_err(|_| VerifyError::BadSignature)
        }
    }
}

fn hex(bytes: [u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    // A virtual authenticator with a key we hold, so tests can also forge INVALID ceremonies.
    use p256::ecdsa::signature::Signer;
    use p256::ecdsa::{Signature, SigningKey};

    const RP: &str = "localhost";
    const ORIGIN: &str = "http://localhost:3053";
    const CHALLENGE: &str = "Y2hhbGxlbmdlLTEyMw"; // base64url("challenge-123")

    fn exp(uv: bool) -> Expectations {
        Expectations {
            rp_id: RP.into(),
            origin: ORIGIN.into(),
            challenge: CHALLENGE.into(),
            require_user_verification: uv,
        }
    }

    fn client_data(ty: &str, challenge: &str, origin: &str) -> Vec<u8> {
        format!(
            r#"{{"type":"{ty}","challenge":"{challenge}","origin":"{origin}","crossOrigin":false}}"#
        )
        .into_bytes()
    }

    fn cose_key(sk: &SigningKey) -> Vec<u8> {
        let point = sk.verifying_key().to_encoded_point(false);
        let mut m = std::collections::BTreeMap::new();
        m.insert(1, Cbor::Uint(2)); // kty: EC2
        m.insert(3, Cbor::Nint(ES256));
        m.insert(-1, Cbor::Uint(1)); // crv: P-256
        m.insert(-2, Cbor::Bytes(point.x().unwrap().to_vec()));
        m.insert(-3, Cbor::Bytes(point.y().unwrap().to_vec()));
        cbor::encode_int_map(&m)
    }

    fn auth_data(rp: &str, flags: u8, count: u32, cred: Option<(&[u8], &[u8])>) -> Vec<u8> {
        let mut ad = Sha256::digest(rp.as_bytes()).to_vec();
        ad.push(flags);
        ad.extend_from_slice(&count.to_be_bytes());
        if let Some((id, cose)) = cred {
            ad.extend_from_slice(&[0u8; 16]); // aaguid
            ad.extend_from_slice(&(id.len() as u16).to_be_bytes());
            ad.extend_from_slice(id);
            ad.extend_from_slice(cose);
        }
        ad
    }

    fn attestation_object(auth_data: &[u8]) -> Vec<u8> {
        let mut out = vec![0xa3];
        out.extend(b"\x63fmt\x64none\x67attStmt\xa0\x68authData");
        out.extend(bytes_head(auth_data.len()));
        out.extend(auth_data);
        out
    }

    fn bytes_head(n: usize) -> Vec<u8> {
        match n {
            0..=23 => vec![0x40 | n as u8],
            24..=0xff => vec![0x58, n as u8],
            _ => vec![0x59, (n >> 8) as u8, n as u8],
        }
    }

    fn register_ok() -> (SigningKey, Credential) {
        let sk = SigningKey::from_bytes(&[7u8; 32].into()).unwrap();
        let cose = cose_key(&sk);
        let ad = auth_data(RP, FLAG_UP | FLAG_UV | FLAG_AT, 0, Some((b"cred-1", &cose)));
        let cred = <Component as Guest>::register(
            exp(true),
            client_data("webauthn.create", CHALLENGE, ORIGIN),
            attestation_object(&ad),
        )
        .expect("registration verifies");
        (sk, cred)
    }

    fn assert_with(
        sk: &SigningKey,
        rp: &str,
        flags: u8,
        count: u32,
        cd: &[u8],
    ) -> (Vec<u8>, Vec<u8>) {
        let ad = auth_data(rp, flags, count, None);
        let mut signed = ad.clone();
        signed.extend_from_slice(&Sha256::digest(cd));
        let sig: Signature = sk.sign(&signed);
        (ad, sig.to_der().as_bytes().to_vec())
    }

    #[test]
    fn registration_extracts_the_credential() {
        let (_, cred) = register_ok();
        assert_eq!(cred.alg, ES256 as i32);
        assert_eq!(cred.id, URL_SAFE_NO_PAD.encode(b"cred-1"));
        assert!(cred.user_verified);
        assert_eq!(cred.aaguid, "0".repeat(32), "all-zero aaguid = model not disclosed");
        assert_eq!(cred.attestation_format, "none");
    }

    #[test]
    fn login_verifies_and_returns_the_new_counter() {
        let (sk, cred) = register_ok();
        let cd = client_data("webauthn.get", CHALLENGE, ORIGIN);
        let (ad, sig) = assert_with(&sk, RP, FLAG_UP | FLAG_UV, 5, &cd);
        let a = <Component as Guest>::authenticate(exp(true), cred, cd, ad, sig)
            .expect("assertion verifies");
        assert_eq!(a.sign_count, 5);
        assert!(a.user_verified);
    }

    #[test]
    fn every_binding_is_actually_checked() {
        let (sk, cred) = register_ok();
        let good = client_data("webauthn.get", CHALLENGE, ORIGIN);

        let cd = client_data("webauthn.create", CHALLENGE, ORIGIN);
        let (ad, sig) = assert_with(&sk, RP, FLAG_UP | FLAG_UV, 1, &cd);
        assert!(matches!(
            <Component as Guest>::authenticate(exp(false), cred.clone(), cd, ad, sig),
            Err(VerifyError::BadType(_))
        ));

        let cd = client_data("webauthn.get", "b3RoZXItY2hhbGxlbmdl", ORIGIN);
        let (ad, sig) = assert_with(&sk, RP, FLAG_UP, 1, &cd);
        assert!(matches!(
            <Component as Guest>::authenticate(exp(false), cred.clone(), cd, ad, sig),
            Err(VerifyError::ChallengeMismatch)
        ));

        let cd = client_data("webauthn.get", CHALLENGE, "http://evil.example");
        let (ad, sig) = assert_with(&sk, RP, FLAG_UP, 1, &cd);
        assert!(matches!(
            <Component as Guest>::authenticate(exp(false), cred.clone(), cd, ad, sig),
            Err(VerifyError::OriginMismatch(o)) if o == "http://evil.example"
        ));

        let (ad, sig) = assert_with(&sk, "other.example", FLAG_UP, 1, &good);
        assert!(matches!(
            <Component as Guest>::authenticate(exp(false), cred.clone(), good.clone(), ad, sig),
            Err(VerifyError::RpIdMismatch)
        ));

        let (ad, sig) = assert_with(&sk, RP, 0, 1, &good);
        assert!(matches!(
            <Component as Guest>::authenticate(exp(false), cred.clone(), good.clone(), ad, sig),
            Err(VerifyError::UserNotPresent)
        ));

        let (ad, sig) = assert_with(&sk, RP, FLAG_UP, 1, &good);
        assert!(matches!(
            <Component as Guest>::authenticate(exp(true), cred.clone(), good.clone(), ad, sig),
            Err(VerifyError::UserNotVerified)
        ));

        let other = SigningKey::from_bytes(&[9u8; 32].into()).unwrap();
        let (ad, sig) = assert_with(&other, RP, FLAG_UP, 1, &good);
        assert!(matches!(
            <Component as Guest>::authenticate(exp(false), cred.clone(), good.clone(), ad, sig),
            Err(VerifyError::BadSignature)
        ));

        // flip a counter byte after signing
        let (mut ad, sig) = assert_with(&sk, RP, FLAG_UP, 9, &good);
        ad[36] ^= 0xff;
        assert!(matches!(
            <Component as Guest>::authenticate(exp(false), cred, good, ad, sig),
            Err(VerifyError::BadSignature)
        ));
    }

    #[test]
    fn a_cloned_authenticator_is_caught_by_the_counter() {
        let (sk, mut cred) = register_ok();
        let cd = client_data("webauthn.get", CHALLENGE, ORIGIN);

        let (ad, sig) = assert_with(&sk, RP, FLAG_UP, 4, &cd);
        cred.sign_count =
            <Component as Guest>::authenticate(exp(false), cred.clone(), cd.clone(), ad, sig)
                .unwrap()
                .sign_count;
        assert_eq!(cred.sign_count, 4);

        // The same counter again (or lower) means two copies of the key exist.
        let (ad, sig) = assert_with(&sk, RP, FLAG_UP, 4, &cd);
        assert!(matches!(
            <Component as Guest>::authenticate(exp(false), cred.clone(), cd.clone(), ad, sig),
            Err(VerifyError::CounterRegressed(4))
        ));

        // A counter-less authenticator (always 0) is permitted only if it was registered at 0 too.
        let (sk0, cred0) = register_ok();
        let (ad, sig) = assert_with(&sk0, RP, FLAG_UP, 0, &cd);
        assert!(<Component as Guest>::authenticate(exp(false), cred0, cd, ad, sig).is_ok());
    }

    #[test]
    fn unsupported_algorithms_are_named_not_ignored() {
        // Ed25519 (COSE -8) is real and we don't do it — say so.
        let sk = SigningKey::from_bytes(&[3u8; 32].into()).unwrap();
        let point = sk.verifying_key().to_encoded_point(false);
        let mut m = std::collections::BTreeMap::new();
        m.insert(1, Cbor::Uint(1));
        m.insert(3, Cbor::Nint(-8));
        m.insert(-1, Cbor::Uint(6));
        m.insert(-2, Cbor::Bytes(point.x().unwrap().to_vec()));
        let cose = cbor::encode_int_map(&m);
        let ad = auth_data(RP, FLAG_UP | FLAG_AT, 0, Some((b"cred-x", &cose)));
        assert!(matches!(
            <Component as Guest>::register(
                exp(false),
                client_data("webauthn.create", CHALLENGE, ORIGIN),
                attestation_object(&ad)
            ),
            Err(VerifyError::UnsupportedAlgorithm(-8))
        ));
    }

    #[test]
    fn malformed_input_is_rejected_cleanly() {
        let e = exp(false);
        assert!(matches!(
            <Component as Guest>::register(e.clone(), b"not json".to_vec(), vec![]),
            Err(VerifyError::BadEncoding(_))
        ));
        let cd = client_data("webauthn.create", CHALLENGE, ORIGIN);
        // valid CBOR, no authData
        assert!(matches!(
            <Component as Guest>::register(e.clone(), cd.clone(), b"\xa0".to_vec()),
            Err(VerifyError::Malformed(_))
        ));
        // authData too short to hold the fixed header
        assert!(matches!(
            <Component as Guest>::register(e, cd, attestation_object(&[0u8; 20])),
            Err(VerifyError::Malformed(_))
        ));
    }
}
