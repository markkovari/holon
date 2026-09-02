//! `webhook-relay` — the first gate this component has ever had.
//!
//! Written because of a `ponytail:` marker in `src/lib.rs` saying dedup is completed
//! before the enqueue, and because nothing here could be checked: no gate, no unit
//! tests, no `e2e-*.sh`. A component with a known ordering hazard and no way to
//! observe it is the one shape where a fix cannot be reviewed.
//!
//! ## What the hazard actually is
//!
//! `webhook:ingest/verifier::ingest` verifies the HMAC **and** marks the delivery-id
//! seen, atomically, in one call. Everything that can still fail happens after it:
//!
//!   * the transform can fail        -> 422
//!   * `outbox::enqueue` can fail    -> 503
//!
//! In both cases the delivery-id has already been burnt, so the sender's retry comes
//! back `200 {"replay": true}` — a SUCCESS — for an event that was never queued.
//! Webhook senders retry on non-2xx and stop on 2xx; that is the entire protocol. So
//! a relay that answers 200 to a delivery it dropped tells the sender to stop
//! retrying something that never happened, which is silent message loss on exactly
//! the path idempotency exists to protect.
//!
//! The marker names the 503. The 422 is the same bug and is reachable without any
//! fault injection, which is what makes it testable: a source whose transform cannot
//! apply to the payload rejects every delivery, and the SECOND attempt at one is
//! where the lie shows up.
//!
//! ## What is deliberately not asserted
//!
//! Delivery. `POST /api/drain` signs and sends to `destination`, and proving an
//! outbound webhook arrives wants a sink — `gatelib::Sink` exists for that and the
//! egress allow-list makes it a bigger gate. This one is about ingest ordering.

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

/// The lowercase hex HMAC-SHA256 the relay expects, computed the way a real sender
/// would. `hmac` and `sha2` are already dependencies — `gatelib::totp_now` uses the
/// same crate with SHA-1.
fn sign(secret: &str, payload: &[u8]) -> String {
    use hmac::{Mac, SimpleHmac};
    let mut mac = SimpleHmac::<sha2::Sha256>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(payload);
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// A source, and the inbound secret the test holds so it can sign like a sender.
///
/// `destination` is never contacted — nothing here drains — but it must be http(s)
/// or the create is refused, so it names a port nothing listens on rather than
/// something that could accidentally succeed.
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

/// One signed delivery. `delivery` is the idempotency key the whole bug turns on.
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
    // Two capabilities, not one, and that split IS the fix — see `wit/relay.wit`.
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

    // A bad signature is 401 and must NOT mark the delivery-id: a forged request that
    // burnt an id would let an attacker suppress a real delivery by guessing its id.
    let (status, _) = gate.with_headers(
        "POST",
        &format!("/hook/{id}"),
        None,
        &[("x-relay-delivery", "d-forged"), ("x-relay-signature", "sha256=deadbeef")],
        Some(json!({"event": "created"})),
    );
    assert_eq!(status, 401, "a bad signature must be 401");

    // The missing-header cases, which are the sender's fault and not a replay.
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

    // The same delivery-id again IS a replay, and 200 is right here: it really was
    // queued the first time, so telling the sender to stop is honest.
    let (status, again) = deliver(&gate, &id, secret, "d-1", payload);
    assert_eq!(status, 200, "a repeat of a QUEUED delivery is a 200 replay: {again}");
    assert_eq!(parse(&again)["replay"], json!(true), "a replay must say so: {again}");

    // A forged signature on an id that was never accepted still must not have marked
    // it — otherwise the 401 above would have made `d-forged` un-deliverable.
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
/// This is the ordering bug, and the assertion the `ponytail:` marker in
/// `webhook-relay/src/lib.rs` describes. `ingest` marks the delivery-id seen at the
/// moment it verifies the signature, and the transform runs afterwards — so a source
/// whose transform cannot apply answers 422 with the id already burnt, and the retry
/// gets `200 {"replay": true}`.
///
/// Why that is the expensive kind of wrong rather than a cosmetic status: a webhook
/// sender retries on non-2xx and stops on 2xx. Answering 200 to a delivery that was
/// dropped tells the sender the event landed. Nothing is queued, nothing is dead-
/// lettered, and the only record is an audit line nobody is watching. The 503 the
/// marker names behaves identically and cannot be triggered without failing the
/// outbox on purpose; this path needs nothing but a transform that does not fit.
#[test]
fn a_rejected_delivery_is_not_reported_as_a_replay() {
    let Some(gate) = start() else { return };

    // A patch that cannot apply to the payload: RFC-6902 `remove` of a path that is
    // not there is an error, not a no-op.
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

    // And nothing was queued by either attempt — the drain has nothing to send.
    // Asserted through the audit trail rather than the outbox, because the relay
    // exposes the audit and not the queue.
    let (_, audit) = gate.get("/api/audit?limit=50", None);
    assert!(
        !audit.contains("\"queued\""),
        "a rejected delivery must not have queued anything: {audit}"
    );
}
