//! `public` visibility, which ADR-0007 said requires a signature and which
//! returned 501 for as long as it had no way to check one.
//!
//! The rule being enforced (ADR-0007 rule 3): a version cannot become public
//! unless its digest is signed by the publisher's key, and the platform verifies
//! before allowing the transition. Everything below is either that rule working
//! or an attempt to get round it.
//!
//! The harness holds the private key and the platform never sees it — which is
//! the point, and also what makes these tests worth having: they sign for real.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use serde_json::{json, Value};

mod harness;
use harness::Platform;

/// A publisher: a key the platform will be told about, and one it never sees.
struct Publisher(SigningKey);

impl Publisher {
    fn new() -> Self {
        Self(SigningKey::random(&mut rand_core::OsRng))
    }
    /// The SEC1 point, which is what gets registered.
    fn public_b64(&self) -> String {
        B64.encode(self.0.verifying_key().to_encoded_point(false).as_bytes())
    }
    /// A signature over the digest STRING, which is what the platform checks.
    fn sign(&self, digest: &str) -> String {
        let sig: Signature = self.0.sign(digest.as_bytes());
        B64.encode(sig.to_bytes())
    }
}

/// Upload a real component and record a push for it, so it has a digest to sign.
///
/// The bytes have to be a genuine component: the upload validates by reflecting
/// the WIT surface (ADR-0006), so a placeholder would be refused before any of
/// this got interesting. `slug` is the smallest one built here.
fn component_with_digest(p: &Platform, token: &str, id: &str) -> (String, String) {
    let wasm = harness::repo_root().join("components/target/wasm32-wasip2/release/slug.wasm");
    let bytes = std::fs::read(&wasm)
        .unwrap_or_else(|e| panic!("missing {} ({e}) — run `just build`", wasm.display()));
    let r = p
        .http
        .post(p.url(&format!("/api/components?id={id}")))
        .bearer_auth(token)
        .body(bytes)
        .send()
        .unwrap();
    let code = r.status().as_u16();
    let row: Value = r.json().unwrap_or(Value::Null);
    assert!(code == 201 || code == 200, "upload of {id} failed: {code} {row}");
    // Taken from the response rather than assumed: the catalogue key is the
    // tenant's, and a test that guesses it passes for the wrong reason the day
    // the naming changes.
    let key = row["key"].as_str().unwrap_or_default().to_string();
    assert!(!key.is_empty(), "the upload did not report a catalogue key: {row}");

    let digest = format!("sha256:{:0>64}", id);
    let (code, body) = p.post_internal(
        "/api/internal/pushed",
        json!({ "key": key, "digest": digest }),
    );
    assert_eq!(code, 200, "recording the push failed: {body}");
    (key, digest)
}

#[test]
fn public_requires_a_signature_over_the_digest() {
    let p = Platform::start(8461);
    let ada = p.user("ada");
    let pubr = Publisher::new();

    let (_key, digest) = component_with_digest(&p, &ada, "widget");

    // 1. Without a key registered, a correct signature still cannot be trusted:
    //    the platform has nothing to check it against.
    let (code, body) = p.post(
        &ada,
        "/api/components/publish",
        json!({ "id": "widget", "visibility": "public", "signature": pubr.sign(&digest) }),
    );
    assert_eq!(code, 403, "public was granted with no key on file: {body}");

    // 2. Register the verifying key. The private half never leaves this test.
    let (code, body) = p.post(
        &ada,
        "/api/keys",
        json!({ "name": "release", "public_key": pubr.public_b64() }),
    );
    assert_eq!(code, 201, "registering a key failed: {body}");

    // 3. A signature over the WRONG digest must not pass. This is the attack that
    //    matters: sign something harmless, publish something else.
    let (code, body) = p.post(
        &ada,
        "/api/components/publish",
        json!({
            "id": "widget", "visibility": "public",
            "signature": pubr.sign("sha256:0000000000000000000000000000000000000000000000000000000000000000"),
        }),
    );
    assert_eq!(code, 403, "a signature over another digest was accepted: {body}");

    // 4. Somebody else's key does not help either.
    let attacker = Publisher::new();
    let (code, body) = p.post(
        &ada,
        "/api/components/publish",
        json!({ "id": "widget", "visibility": "public", "signature": attacker.sign(&digest) }),
    );
    assert_eq!(code, 403, "an unregistered key's signature was accepted: {body}");

    // 5. The real thing.
    let (code, body) = p.post(
        &ada,
        "/api/components/publish",
        json!({ "id": "widget", "visibility": "public", "signature": pubr.sign(&digest) }),
    );
    assert_eq!(code, 200, "a valid signature was refused: {body}");
    let row = body;
    assert_eq!(row["visibility"], json!("public"));
    assert_eq!(row["signed_digest"], json!(digest), "public was not bound to what was signed");
    assert_eq!(row["signed_by"], json!("release"), "provenance: which key vouched for it");
}

#[test]
fn new_bytes_do_not_inherit_a_signature() {
    // ADR-0007 rule 1: visibility widens only by an explicit act. This catalogue
    // keys rows by name rather than by version, so without this the next push
    // would publish bytes nobody signed under the previous version's blessing.
    let p = Platform::start(8462);
    let ada = p.user("ada");
    let pubr = Publisher::new();
    p.post(&ada, "/api/keys", json!({ "name": "release", "public_key": pubr.public_b64() }));

    let (key, digest) = component_with_digest(&p, &ada, "widget");
    let (code, _) = p.post(
        &ada,
        "/api/components/publish",
        json!({ "id": "widget", "visibility": "public", "signature": pubr.sign(&digest) }),
    );
    assert_eq!(code, 200, "the signed publish should have worked");

    // Now push different bytes to the same name.
    let (code, _) = p.post_internal(
        "/api/internal/pushed",
        json!({ "key": key, "digest": "sha256:beef000000000000000000000000000000000000000000000000000000000000" }),
    );
    assert_eq!(code, 200);

    let (_, listed) = p.get(&ada, "/api/components");
    let row = listed["components"]
        .as_array()
        .and_then(|a| a.iter().find(|r| r["id"] == json!("widget")))
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        row["visibility"],
        json!("private"),
        "NEW BYTES INHERITED A PUBLIC LISTING — nobody signed these: {row}"
    );
}
