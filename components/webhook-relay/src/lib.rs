//! relay:app — webhook relay over composed capability contracts.
//!
//! Ingest (`POST /hook/{source}`): rate-limit -> HMAC verify + replay dedup
//! (webhook:ingest) -> optional json:patch transform -> outbox enqueue -> 202.
//! Delivery is an explicit pump (`POST /api/drain`, wasip2 has no background
//! tasks): claim -> github-scheme sign -> notify:dispatch -> ack/fail, with the
//! outbox contract owning retry/backoff and the dead-letter lane.

#[allow(warnings)]
mod bindings;

use serde::Deserialize;
use serde_json::{json, Value};

use bindings::audit::log::query as audit_query;
use bindings::audit::log::recorder;
use bindings::json::patch::patcher;
use bindings::notify::dispatch::dispatcher as notify;
use bindings::outbox::dispatch::queue as outbox;
use bindings::ratelimit::guard::limiter;
use bindings::ratelimit::guard::limiter::LimitError;
use bindings::records::store::store as records;
use bindings::wasi::keyvalue::store as kv;
use bindings::idempotency::guard::store as idem;
use bindings::webhook::sign::signer;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const SOURCES: &str = "relay_sources";
const CLAIM_BATCH: u32 = 25;
const CLAIM_LEASE: u64 = 60;
/// How long a delivery-id stays deduplicated.
///
/// The replay window a sender can retry inside, and it was `webhook:ingest`'s to
/// choose before this component held the mark itself. Twenty-four hours because
/// that is the outer bound of every webhook retry schedule worth naming — GitHub
/// gives up long before it, Stripe keeps trying for three days but with the same
/// delivery-id, so a shorter window would let a genuinely-delivered event be
/// accepted twice.
const DEDUP_TTL: u64 = 24 * 60 * 60;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let query = path.split_once('?').map(|x| x.1).unwrap_or("").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let result = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => Outcome::Json(
                200,
                json!({
                    "service": "webhook-relay",
                    "sources": "POST /api/sources {name, secret, destination, dest-secret, transform?}",
                    "ingest": "POST /hook/{source-id} + x-relay-signature (hex hmac-sha256) + x-relay-delivery",
                    "drain": "POST /api/drain",
                    "dead": "GET /api/dead, POST /api/dead/{id}/replay",
                    "audit": "GET /api/audit?limit=50"
                })
                .to_string(),
            ),
            (Method::Post, ["api", "sources"]) => create_source(&request),
            (Method::Get, ["api", "sources"]) => list_sources(),
            (Method::Get, ["api", "sources", id]) => get_source(id),
            (Method::Delete, ["api", "sources", id]) => delete_source(id),
            (Method::Post, ["hook", id]) => inbound(&request, id),
            (Method::Post, ["api", "drain"]) => drain(),
            (Method::Get, ["api", "dead"]) => dead_letters(),
            (Method::Post, ["api", "dead", id, "replay"]) => replay_dead(id),
            (Method::Get, ["api", "audit"]) => audit_recent(&query),
            _ => Outcome::NotFound,
        };
        emit(response_out, result);
    }
}

enum Outcome {
    Json(u16, String),
    /// 429 with a Retry-After of the payload seconds.
    Limited(u32),
    Bad(String),
    Err(u16, String),
    NotFound,
}

// ---- sources ---------------------------------------------------------------

#[derive(Deserialize)]
struct CreateSource {
    name: String,
    /// inbound HMAC secret senders sign with.
    secret: String,
    /// where accepted deliveries get forwarded.
    destination: String,
    /// outbound signing secret (github scheme) receivers verify with.
    #[serde(rename = "dest-secret", alias = "dest_secret")]
    dest_secret: String,
    /// optional reshape: JSON array -> RFC 6902 patch, object -> merge-patch.
    #[serde(default)]
    transform: Option<Value>,
}

fn secret_ref(source_id: &str) -> String {
    format!("relay:secret:{source_id}")
}

fn create_source(request: &IncomingRequest) -> Outcome {
    let req: CreateSource = match read_body(request)
        .and_then(|b| serde_json::from_slice(&b).map_err(|_| ()))
    {
        Ok(r) => r,
        Err(_) => {
            return Outcome::Bad(
                "expected json body {name, secret, destination, dest-secret, transform?}".into(),
            )
        }
    };
    if !(req.destination.starts_with("http://") || req.destination.starts_with("https://")) {
        return Outcome::Bad("destination must be http(s)".into());
    }
    if req.secret.is_empty() || req.dest_secret.is_empty() {
        return Outcome::Bad("secret and dest-secret must be non-empty".into());
    }
    if let Some(t) = &req.transform {
        if !(t.is_array() || t.is_object()) {
            return Outcome::Bad("transform must be a patch array or merge-patch object".into());
        }
    }

    // dest_secret lives in the record (same trust domain as the kv store the
    // record lands in); the INBOUND secret goes to the kv key webhook:ingest
    // reads via `secret-ref`. Secrets never appear in responses.
    let data = json!({
        "name": req.name,
        "destination": req.destination,
        "dest_secret": req.dest_secret,
        "transform": req.transform,
    });
    let entry = match records::create(SOURCES, &data.to_string(), &[]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    let stored = kv::open("default")
        .and_then(|b| b.set(&secret_ref(&entry.id), req.secret.as_bytes()))
        .is_ok();
    if !stored {
        let _ = records::delete(SOURCES, &entry.id);
        return Outcome::Err(503, "could not store inbound secret".into());
    }
    Outcome::Json(201, source_json(&entry).to_string())
}

fn get_source(id: &str) -> Outcome {
    match records::get(SOURCES, id) {
        Ok(e) => Outcome::Json(200, source_json(&e).to_string()),
        Err(records::StoreError::NotFound) => Outcome::NotFound,
        Err(e) => store_err(e),
    }
}

fn list_sources() -> Outcome {
    match records::list_records(SOURCES, 0, "") {
        Ok(page) => {
            let sources: Vec<Value> = page.entries.iter().map(source_json).collect();
            Outcome::Json(200, json!({ "sources": sources }).to_string())
        }
        Err(e) => store_err(e),
    }
}

fn delete_source(id: &str) -> Outcome {
    match records::delete(SOURCES, id) {
        Ok(_) => {}
        Err(records::StoreError::NotFound) => return Outcome::NotFound,
        Err(e) => return store_err(e),
    }
    if let Ok(bucket) = kv::open("default") {
        let _ = bucket.delete(&secret_ref(id));
    }
    Outcome::Json(200, "{\"deleted\":true}".into())
}

fn source_json(entry: &records::Entry) -> Value {
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    json!({
        "id": entry.id,
        "name": data["name"],
        "destination": data["destination"],
        "transform": !data["transform"].is_null(),
        "hook": format!("/hook/{}", entry.id),
        "created": entry.created,
    })
}

// ---- ingest ----------------------------------------------------------------

fn inbound(request: &IncomingRequest, source_id: &str) -> Outcome {
    let source = match records::get(SOURCES, source_id) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Outcome::NotFound,
        Err(e) => return store_err(e),
    };

    // failure-counter lockout per source: only bad signatures count against
    // the window, so a healthy sender is never throttled.
    let rl_key = format!("relay:hook:{source_id}");
    match limiter::check(&rl_key) {
        Ok(_) => {}
        Err(LimitError::Locked(secs)) => return Outcome::Limited(secs),
        Err(LimitError::BackendUnavailable(m)) => return Outcome::Err(503, m),
    }

    let Some(delivery) = header(request, "x-relay-delivery").filter(|d| !d.is_empty()) else {
        return Outcome::Bad("missing x-relay-delivery header".into());
    };
    let sig = header(request, "x-relay-signature").unwrap_or_default();
    let sig = sig.strip_prefix("sha256=").unwrap_or(&sig).to_string();
    if sig.is_empty() {
        return Outcome::Bad("missing x-relay-signature header".into());
    }
    let payload = read_body(request).unwrap_or_default();

    // --- verify, THEN reserve, THEN work, and only then commit ----------------
    //
    // The order is the fix. `webhook:ingest/verifier::ingest` did the first two in
    // one atomic call, which meant the delivery-id was marked seen before anything
    // that could still fail had run — see the note in `wit/relay.wit`.
    //
    // Verification stays first and stays side-effect-free: a forged request must not
    // be able to burn an id, or an attacker who guesses a delivery-id can suppress
    // the real delivery of it. `signer::verify` compares in constant time.
    let secret = match kv::open("default").and_then(|b| b.get(&secret_ref(source_id))) {
        Ok(Some(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
        // The source record exists and its secret does not. `create_source` writes
        // both and rolls the record back if the secret write fails, so this is a
        // store that lost one of them — a 503 the sender should retry, never a 401,
        // which would blame the sender for our missing key.
        Ok(None) => return Outcome::Err(503, "the inbound secret for this source is missing".into()),
        Err(e) => return Outcome::Err(503, format!("reading the inbound secret: {e:?}")),
    };
    // The header as the sender sent it. `sig` has had any `sha256=` prefix stripped
    // above, and the github scheme wants it back — senders that omit it still work,
    // which is the behaviour this component already had.
    let header = format!("sha256={sig}");
    if signer::verify(&payload, &header, &secret, signer::Scheme::Github, 0).is_err() {
        let _ = limiter::record_failure(&rl_key);
        audit("hook.rejected", "bad-signature", source_id, &delivery, "");
        return Outcome::Err(401, "bad signature".into());
    }

    // delivery-ids are scoped per source so two senders can't collide.
    let scoped = format!("{source_id}:{delivery}");
    match idem::begin(&scoped, DEDUP_TTL) {
        // First time for this id: it is reserved, not committed. Nothing below may
        // return without either completing or forgetting it.
        Ok(None) => {}
        // Already handled. 200 with `{"replay": true}` rather than replaying the
        // stored response verbatim: that is what this component has always answered,
        // it is a 2xx so the sender correctly stops, and the case was never broken.
        Ok(Some(_)) => return Outcome::Json(200, json!({ "replay": true }).to_string()),
        // A concurrent duplicate — the other caller holds the reservation and has not
        // finished. Retryable, and `idempotency:guard`'s own contract names 409 for it.
        Err(idem::IdemError::InProgress) => {
            return Outcome::Err(409, "a delivery with this id is already in flight".into())
        }
        Err(idem::IdemError::BackendUnavailable(m)) => return Outcome::Err(503, m),
    }

    let outcome = accept(&source, source_id, &delivery, payload);

    // COMMIT ON SUCCESS, RELEASE ON ANYTHING ELSE.
    //
    // A 2xx means the delivery is in the outbox and a retry of this id must be told
    // to stop. Anything else means it is not, and the id has to be usable again or
    // the sender is told to stop retrying an event that never landed.
    //
    // `forget` failing is reported and not fatal: the delivery already failed, this
    // is the second piece of bad news, and turning it into a different status would
    // hide the first. A guard whose backend is down is also why the enqueue failed.
    match &outcome {
        Outcome::Json(status, body) if (200..300).contains(status) => {
            if let Err(e) = idem::complete(&scoped, *status, body.as_bytes()) {
                // Queued but not marked: a retry will queue it a second time. Worth
                // saying out loud, and still a success — the event IS delivered.
                audit("hook.accepted", "dedup-not-recorded", source_id, &delivery, &format!("{e:?}"));
            }
        }
        _ => {
            if let Err(e) = idem::forget(&scoped) {
                audit("hook.rejected", "dedup-not-released", source_id, &delivery, &format!("{e:?}"));
            }
        }
    }
    outcome
}

fn accept(source: &records::Entry, source_id: &str, delivery: &str, payload: Vec<u8>) -> Outcome {
    let data: Value = serde_json::from_str(&source.data).unwrap_or(Value::Null);
    let doc = String::from_utf8_lossy(&payload).into_owned();
    let transform = &data["transform"];
    let outbound = if transform.is_null() {
        doc
    } else {
        let patch = transform.to_string();
        let result = if transform.is_array() {
            patcher::apply_patch(&doc, &patch)
        } else {
            patcher::apply_merge(&doc, &patch)
        };
        match result {
            Ok(s) => s,
            Err(e) => {
                audit("hook.rejected", "transform-failed", source_id, delivery, &format!("{e:?}"));
                return Outcome::Err(422, format!("transform failed: {e:?}"));
            }
        }
    };
    // The enqueue is the last thing that can fail, and its failure is now visible
    // to the caller: `inbound` forgets the reservation for any non-2xx, so a 503
    // here leaves the delivery-id usable and the sender's retry is a real attempt
    // rather than a `200 {"replay": true}` for an event that was never queued.
    match outbox::enqueue(source_id, outbound.as_bytes(), 0) {
        Ok(event_id) => {
            audit("hook.accepted", "queued", source_id, delivery, &event_id);
            Outcome::Json(202, json!({ "queued": event_id }).to_string())
        }
        Err(e) => Outcome::Err(503, format!("outbox: {e:?}")),
    }
}

// ---- delivery --------------------------------------------------------------

/// Deliver pending events as signed webhooks — the explicit-pump pattern
/// (same as dev-portal's admin drain): claim -> sign -> send -> ack/fail.
fn drain() -> Outcome {
    let events = match outbox::claim(CLAIM_BATCH, CLAIM_LEASE) {
        Ok(evs) => evs,
        Err(e) => return Outcome::Err(503, format!("outbox: {e:?}")),
    };
    let (mut delivered, mut dropped, mut failed, mut dead) = (0u32, 0u32, 0u32, 0u32);
    for ev in &events {
        // topic == source id.
        let (dest, secret, name) = match records::get(SOURCES, &ev.topic) {
            Ok(e) => {
                let d: Value = serde_json::from_str(&e.data).unwrap_or(Value::Null);
                (
                    d["destination"].as_str().unwrap_or("").to_string(),
                    d["dest_secret"].as_str().unwrap_or("").to_string(),
                    d["name"].as_str().unwrap_or("").to_string(),
                )
            }
            Err(_) => (String::new(), String::new(), String::new()),
        };
        if dest.is_empty() {
            // source deleted since enqueue -> drop, don't retry forever.
            let _ = outbox::ack(&ev.id);
            dropped += 1;
            continue;
        }
        // github-scheme HMAC over the payload; the signature rides inside the
        // envelope because notify:dispatch's message has no header field.
        let envelope = match signer::sign(&ev.payload, &secret, signer::Scheme::Github) {
            Ok(s) => json!({
                "id": ev.id,
                "source": name,
                "attempt": ev.attempts + 1,
                "payload": String::from_utf8_lossy(&ev.payload),
                "signature": s.header,
                "timestamp": s.timestamp,
            })
            .to_string(),
            Err(e) => {
                let _ = outbox::fail(&ev.id);
                failed += 1;
                audit("deliver.failed", "sign-error", &ev.topic, &ev.id, &format!("{e:?}"));
                continue;
            }
        };
        let msg = notify::Message {
            channel: notify::Channel::Webhook,
            target: dest,
            subject: ev.topic.clone(),
            body: envelope,
        };
        match notify::send(&msg) {
            Ok(status) if (200..300).contains(&status) => {
                let _ = outbox::ack(&ev.id);
                delivered += 1;
                audit("deliver.ok", &status.to_string(), &ev.topic, &ev.id, "");
            }
            _ => {
                let state = outbox::fail(&ev.id).unwrap_or(outbox::State::Pending);
                if matches!(state, outbox::State::Dead) {
                    dead += 1;
                    audit("deliver.dead", "max-attempts", &ev.topic, &ev.id, "");
                } else {
                    failed += 1;
                    audit("deliver.failed", "upstream", &ev.topic, &ev.id, "");
                }
            }
        }
    }
    Outcome::Json(
        200,
        json!({
            "claimed": events.len(),
            "delivered": delivered,
            "dropped": dropped,
            "failed": failed,
            "dead": dead,
        })
        .to_string(),
    )
}

fn dead_letters() -> Outcome {
    match outbox::dead_letters(50) {
        Ok(evs) => {
            let list: Vec<Value> = evs
                .iter()
                .map(|ev| {
                    json!({
                        "id": ev.id,
                        "source": ev.topic,
                        "attempts": ev.attempts,
                        "payload": String::from_utf8_lossy(&ev.payload),
                        "created": ev.created,
                    })
                })
                .collect();
            Outcome::Json(200, json!({ "dead": list }).to_string())
        }
        Err(e) => Outcome::Err(503, format!("outbox: {e:?}")),
    }
}

fn replay_dead(id: &str) -> Outcome {
    match outbox::replay(id) {
        Ok(_) => Outcome::Json(200, "{\"replayed\":true}".into()),
        Err(outbox::OutboxError::NotFound) => Outcome::NotFound,
        Err(e) => Outcome::Err(503, format!("outbox: {e:?}")),
    }
}

// ---- audit -----------------------------------------------------------------

fn audit(event: &str, outcome: &str, source: &str, delivery: &str, detail: &str) {
    // best-effort trail: recorder fills id + timestamp when left empty.
    let _ = recorder::record_event(&recorder::Event {
        id: String::new(),
        trace_id: delivery.to_string(),
        span_id: String::new(),
        timestamp: 0,
        event: event.to_string(),
        outcome: outcome.to_string(),
        tenant: source.to_string(),
        subject: delivery.to_string(),
        detail: detail.to_string(),
    });
}

fn audit_recent(query: &str) -> Outcome {
    let limit = query_param(query, "limit").and_then(|s| s.parse().ok()).unwrap_or(50);
    match audit_query::recent(limit) {
        Ok(events) => {
            let list: Vec<Value> = events
                .iter()
                .map(|e| {
                    json!({
                        "at": e.timestamp,
                        "event": e.event,
                        "outcome": e.outcome,
                        "source": e.tenant,
                        "subject": e.subject,
                        "detail": e.detail,
                    })
                })
                .collect();
            Outcome::Json(200, json!({ "events": list }).to_string())
        }
        Err(e) => Outcome::Err(503, format!("audit: {e:?}")),
    }
}

// ---- helpers ---------------------------------------------------------------

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::NotFound,
        records::StoreError::InvalidJson(m) => Outcome::Bad(m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

/// The most a request body may be, before the component stops reading it.
///
/// There was no ceiling anywhere: 148 of 150 components accumulated whatever
/// arrived until the guest hit wasmtime's 64 MiB per-store memory cap and TRAPPED,
/// which reaches the caller as a closed connection saying nothing about a size.
/// A component that answers JSON has no business reading sixteen megabytes, and
/// the ones that legitimately handle uploads police it themselves with a 413 and a
/// granted max-size — those are left alone.
///
/// Generous on purpose. This is a backstop against an unbounded read, not a
/// content policy; an API that needs a real limit should state its own and say 413.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_read_body!(MAX_BODY_BYTES);
guestio::guest_write_all!();

fn header(request: &IncomingRequest, name: &str) -> Option<String> {
    request.headers().get(name).into_iter().find_map(|v| String::from_utf8(v).ok())
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let mut it = kv.splitn(2, '=');
        (it.next()? == key).then(|| it.next().unwrap_or("").to_string())
    })
}

// ---- responses -------------------------------------------------------------

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond(response_out, code, &[], body.as_bytes()),
        Outcome::Limited(secs) => respond(
            response_out,
            429,
            &[("retry-after", &secs.to_string())],
            format!("{{\"error\":\"rate_limited\",\"retryAfter\":{secs}}}").as_bytes(),
        ),
        Outcome::Bad(msg) => {
            respond(response_out, 400, &[], json!({ "error": msg }).to_string().as_bytes())
        }
        Outcome::Err(code, msg) => {
            respond(response_out, code, &[], json!({ "error": msg }).to_string().as_bytes())
        }
        Outcome::NotFound => respond(response_out, 404, &[], b"{\"error\":\"not_found\"}"),
    }
}

fn respond(response_out: ResponseOutparam, status: u16, extra: &[(&str, &str)], body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    for (k, v) in extra {
        let _ = headers.set(k.as_ref(), &[v.as_bytes().to_vec()]);
    }
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        let _ = write_all(&stream, body);
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
