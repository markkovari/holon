//! `webhook-relay`'s gate, written because a `ponytail:` marker in `src/lib.rs` flagged an ordering
//! hazard with no way to observe it.
//!
//! `webhook:ingest/verifier::ingest` verifies the HMAC **and** marks the delivery-id seen, atomically,
//! in one call. Everything that can still fail comes after it: the transform (422) and
//! `outbox::enqueue` (503). The delivery-id is then burnt, so the sender's retry gets `200
//! {"replay": true}`, a SUCCESS, for an event never queued. Senders stop retrying on 2xx, so that is
//! silent message loss on the path idempotency exists to protect.
//!
//! The marker names the 503; the 422 is the same bug and needs no fault injection (a source whose
//! transform cannot apply rejects every delivery).
//!
//! Not asserted: delivery. `POST /api/drain` sends to `destination`, and proving arrival wants a
//! `gatelib::Sink` and an egress allow-list; this gate is about ingest ordering.

mod gatelib;
use gatelib::{field, requires_capability, Gate};
use serde_json::{json, Value};

const CRATE: &str = "webhook-relay";

fn start() -> Option<Gate> {
    Gate::compose_and_start("relay", CRATE, &[])
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// The lowercase hex HMAC-SHA256 the relay expects, computed as a real sender would.
fn sign(secret: &str, payload: &[u8]) -> String {
    use hmac::{Mac, SimpleHmac};
    let mut mac = SimpleHmac::<sha2::Sha256>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(payload);
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// A source, and the inbound secret the test holds so it can sign like a sender. `destination` is
/// never contacted but must be http(s), so it names a port nothing listens on.
fn source(gate: &Gate, transform: Value) -> (String, &'static str) {
    const SECRET: &str = "inbound-shh";
    let mut body = json!({
        "name": "gate",
        "secret": SECRET,
        "destination": "http://127.0.0.1:9/",
        "dest-secret": "outbound-shh",
    });
    if !transform.is_null() {
        body["transform"] = transform;
    }
    let (status, resp) = gate.post("/api/sources", None, body);
    assert_eq!(status, 201, "POST /api/sources did not create a source: {resp}");
    let id = field(&resp, "id");
    assert!(!id.is_empty(), "the created source has no id: {resp}");
    assert!(
        !resp.contains(SECRET) && !resp.contains("outbound-shh"),
        "secrets must never appear in a response: {resp}"
    );
    (id, SECRET)
}

/// One signed delivery. `delivery` is the idempotency key the bug turns on.
fn deliver(gate: &Gate, id: &str, secret: &str, delivery: &str, payload: &str) -> (u16, String) {
    let sig = format!("sha256={}", sign(secret, payload.as_bytes()));
    gate.with_headers(
        "POST",
        &format!("/hook/{id}"),
        None,
        &[("x-relay-delivery", delivery), ("x-relay-signature", sig.as_str())],
        Some(json!(serde_json::from_str::<Value>(payload).unwrap_or(Value::Null))),
    )
}

/// The signature and dedup behaviour a sender depends on.
#[test]
fn a_delivery_is_verified_and_deduplicated() {
    let Some(gate) = start() else { return };
    // Two capabilities, not one, and that split IS the fix (see `wit/relay.wit`).
    requires_capability(
        CRATE,
        "webhook:sign/signer",
        "verifying an inbound HMAC is a solved problem here and `signer::verify` does it \
         in constant time with NO side effect — which is what lets the signature check \
         happen before the delivery-id is reserved",
    );
    requires_capability(
        CRATE,
        "idempotency:guard/store",
        "the delivery-id mark has to be a RESERVATION this component commits on success \
         and releases on failure; a dedup that marks and cannot unmark is the bug this \
         gate exists for",
    );

    let (id, secret) = source(&gate, Value::Null);
    let payload = r#"{"event":"created","id":1}"#;

    // A bad signature is 401 and must NOT mark the delivery-id: a forged request that burnt an id
    // would let an attacker suppress a real delivery.
    let (status, _) = gate.with_headers(
        "POST",
        &format!("/hook/{id}"),
        None,
        &[("x-relay-delivery", "d-forged"), ("x-relay-signature", "sha256=deadbeef")],
        Some(json!({"event": "created"})),
    );
    assert_eq!(status, 401, "a bad signature must be 401");

    // The missing-header cases are the sender's fault, not a replay.
    let (status, _) = gate.with_headers(
        "POST",
        &format!("/hook/{id}"),
        None,
        &[("x-relay-signature", "sha256=abc")],
        Some(json!({})),
    );
    assert_eq!(status, 400, "a delivery with no x-relay-delivery header must be 400");

    // The happy path: accepted and queued.
    let (status, ok) = deliver(&gate, &id, secret, "d-1", payload);
    assert_eq!(status, 202, "a correctly signed first delivery must be 202: {ok}");
    assert!(!field(&ok, "queued").is_empty(), "an accepted delivery must name its queue id: {ok}");

    // The same delivery-id again IS a replay, and 200 is right: it really was queued the first time.
    let (status, again) = deliver(&gate, &id, secret, "d-1", payload);
    assert_eq!(status, 200, "a repeat of a QUEUED delivery is a 200 replay: {again}");
    assert_eq!(parse(&again)["replay"], json!(true), "a replay must say so: {again}");

    // A forged signature on a never-accepted id must not have marked it, or `d-forged` is undeliverable.
    let (status, honest) = deliver(&gate, &id, secret, "d-forged", payload);
    assert_eq!(
        status, 202,
        "a delivery-id whose only previous attempt FAILED signature verification must \
         still be deliverable — a 401 that burns an id lets a forger suppress a real \
         delivery by guessing its id: {honest}"
    );
}

/// A delivery the relay REFUSED must not come back as a successful replay.
///
/// The ordering bug the `ponytail:` marker in `webhook-relay/src/lib.rs` describes: `ingest` marks the
/// delivery-id seen when it verifies the signature, the transform runs afterwards, so a transform that
/// cannot apply answers 422 with the id burnt and the retry gets `200 {"replay": true}`. Nothing is
/// queued or dead-lettered; the only record is an audit line. The 503 behaves identically but needs the
/// outbox failed on purpose; this path needs only a transform that does not fit.
#[test]
fn a_rejected_delivery_is_not_reported_as_a_replay() {
    let Some(gate) = start() else { return };

    // A patch that cannot apply: RFC-6902 `remove` of a missing path is an error, not a no-op.
    let (id, secret) = source(&gate, json!([{"op": "remove", "path": "/not-here"}]));
    let payload = r#"{"event":"created","id":7}"#;

    let (first, body) = deliver(&gate, &id, secret, "d-transform", payload);
    assert_eq!(
        first, 422,
        "a transform that cannot apply must be 422 so the sender retries: {body}"
    );

    let (second, retry) = deliver(&gate, &id, secret, "d-transform", payload);
    assert_ne!(
        second, 200,
        "the retry of a delivery that was REFUSED came back {second} — a success. \
         Nothing was queued, so this tells the sender to stop retrying an event that \
         never landed. The delivery-id is marked seen by `ingest`, which runs before \
         the transform, so the failure burnt it: {retry}"
    );
    assert_eq!(second, 422, "the retry must fail the same way the first attempt did: {retry}");
    assert_ne!(
        parse(&retry)["replay"],
        json!(true),
        "a delivery that was never queued is not a replay: {retry}"
    );

    // And nothing was queued by either attempt. Asserted through the audit trail, since the relay
    // exposes the audit and not the queue.
    let (_, audit) = gate.get("/api/audit?limit=50", None);
    assert!(
        !audit.contains("\"queued\""),
        "a rejected delivery must not have queued anything: {audit}"
    );
}
