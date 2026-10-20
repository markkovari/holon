//! relay:app — webhook relay over composed capability contracts.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../idempotency-guard/wit",
            "../webhook-ingest/wit",
            "../jsonpatch/wit",
            "../outbox/wit",
            "../webhook-sign/wit",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../notify-dispatch/wit",
            "../rate-limiter/wit",
            "../audit-log/wit",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "wit",
        ],
        world: "relay:app/relay-app",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use serde::Deserialize;
use serde_json::{json, Value};

use bindings::audit::log::query as audit_query;
use bindings::audit::log::recorder;
use bindings::idempotency::guard::store as idem;
use bindings::json::patch::patcher;
use bindings::notify::dispatch::dispatcher as notify;
use bindings::outbox::dispatch::queue as outbox;
use bindings::ratelimit::guard::limiter;
use bindings::ratelimit::guard::limiter::LimitError;
use bindings::records::store::store as records;
use bindings::wasi::keyvalue::store as kv;
use bindings::webhook::sign::signer;

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};

struct Component;

const SOURCES: &str = "relay_sources";
const CLAIM_BATCH: u32 = 25;
const CLAIM_LEASE: u64 = 60;
/// 24h bounds the retry schedules worth naming; shorter would accept a delivered event twice.
const DEDUP_TTL: u64 = 24 * 60 * 60;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
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
            (Method::Post, ["api", "sources"]) => create_source(request).await,
            (Method::Get, ["api", "sources"]) => list_sources(),
            (Method::Get, ["api", "sources", id]) => get_source(id),
            (Method::Delete, ["api", "sources", id]) => delete_source(id),
            (Method::Post, ["hook", id]) => inbound(request, id).await,
            (Method::Post, ["api", "drain"]) => drain(),
            (Method::Get, ["api", "dead"]) => dead_letters(),
            (Method::Post, ["api", "dead", id, "replay"]) => replay_dead(id),
            (Method::Get, ["api", "audit"]) => audit_recent(&query),
            _ => Outcome::NotFound,
        };
        emit(result)
    }
}

enum Outcome {
    Json(u16, String),
    /// Retry-After seconds.
    Limited(u32),
    Bad(String),
    Err(u16, String),
    NotFound,
}

#[derive(Deserialize)]
struct CreateSource {
    name: String,
    /// Inbound: what senders sign with.
    secret: String,
    destination: String,
    /// Outbound, github scheme.
    #[serde(rename = "dest-secret", alias = "dest_secret")]
    dest_secret: String,
    /// JSON array -> RFC 6902 patch, object -> merge-patch.
    #[serde(default)]
    transform: Option<Value>,
}

fn secret_ref(source_id: &str) -> String {
    format!("relay:secret:{source_id}")
}

async fn create_source(request: Request) -> Outcome {
    let req: CreateSource =
        match read_body(request).await.and_then(|b| serde_json::from_slice(&b).map_err(|_| ())) {
            Ok(r) => r,
            Err(_) => {
                return Outcome::Bad(
                    "expected json body {name, secret, destination, dest-secret, transform?}"
                        .into(),
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

    // The INBOUND secret goes to the kv key webhook:ingest reads via `secret-ref`. Secrets never
    // appear in responses.
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

async fn inbound(request: Request, source_id: &str) -> Outcome {
    let source = match records::get(SOURCES, source_id) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Outcome::NotFound,
        Err(e) => return store_err(e),
    };

    // Only bad signatures count, so a healthy sender is never throttled.
    let rl_key = format!("relay:hook:{source_id}");
    match limiter::check(&rl_key) {
        Ok(_) => {}
        Err(LimitError::Locked(secs)) => return Outcome::Limited(secs),
        Err(LimitError::BackendUnavailable(m)) => return Outcome::Err(503, m),
    }

    let Some(delivery) = header(&request, "x-relay-delivery").filter(|d| !d.is_empty()) else {
        return Outcome::Bad("missing x-relay-delivery header".into());
    };
    let sig = header(&request, "x-relay-signature").unwrap_or_default();
    let sig = sig.strip_prefix("sha256=").unwrap_or(&sig).to_string();
    if sig.is_empty() {
        return Outcome::Bad("missing x-relay-signature header".into());
    }
    let payload = read_body(request).await.unwrap_or_default();

    // Verify first, side-effect free: a forged request must not burn a delivery-id, or an attacker
    // who guesses one suppresses the real delivery.
    let secret = match kv::open("default").and_then(|b| b.get(&secret_ref(source_id))) {
        Ok(Some(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
        // A store that lost the secret: 503 so the sender retries, never a 401 that blames it.
        Ok(None) => {
            return Outcome::Err(503, "the inbound secret for this source is missing".into())
        }
        Err(e) => return Outcome::Err(503, format!("reading the inbound secret: {e:?}")),
    };
    // `sig` had its `sha256=` prefix stripped above; the github scheme wants it back.
    let header = format!("sha256={sig}");
    if signer::verify(&payload, &header, &secret, signer::Scheme::Github, 0).is_err() {
        let _ = limiter::record_failure(&rl_key);
        audit("hook.rejected", "bad-signature", source_id, &delivery, "");
        return Outcome::Err(401, "bad signature".into());
    }

    // delivery-ids are scoped per source so two senders can't collide.
    let scoped = format!("{source_id}:{delivery}");
    match idem::begin(&scoped, DEDUP_TTL) {
        // Reserved, not committed: nothing below may return without completing or forgetting it.
        Ok(None) => {}
        // A 2xx so the sender stops.
        Ok(Some(_)) => return Outcome::Json(200, json!({ "replay": true }).to_string()),
        // A concurrent duplicate still in flight; `idempotency:guard` names 409 for it.
        Err(idem::IdemError::InProgress) => {
            return Outcome::Err(409, "a delivery with this id is already in flight".into())
        }
        Err(idem::IdemError::BackendUnavailable(m)) => return Outcome::Err(503, m),
    }

    let outcome = accept(&source, source_id, &delivery, payload);

    // Commit on 2xx (it is in the outbox), forget on anything else so the id is usable again. A
    // failed `forget` is reported, not fatal, so it does not hide the first failure.
    match &outcome {
        Outcome::Json(status, body) if (200..300).contains(status) => {
            if let Err(e) = idem::complete(&scoped, *status, body.as_bytes()) {
                // Queued but not marked: a retry queues it twice. Still a success.
                audit(
                    "hook.accepted",
                    "dedup-not-recorded",
                    source_id,
                    &delivery,
                    &format!("{e:?}"),
                );
            }
        }
        _ => {
            if let Err(e) = idem::forget(&scoped) {
                audit(
                    "hook.rejected",
                    "dedup-not-released",
                    source_id,
                    &delivery,
                    &format!("{e:?}"),
                );
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
    // `inbound` forgets the reservation on any non-2xx, so a retry after a 503 is a real attempt.
    match outbox::enqueue(source_id, outbound.as_bytes(), 0) {
        Ok(event_id) => {
            audit("hook.accepted", "queued", source_id, delivery, &event_id);
            Outcome::Json(202, json!({ "queued": event_id }).to_string())
        }
        Err(e) => Outcome::Err(503, format!("outbox: {e:?}")),
    }
}

fn drain() -> Outcome {
    let events = match outbox::claim(CLAIM_BATCH, CLAIM_LEASE) {
        Ok(evs) => evs,
        Err(e) => return Outcome::Err(503, format!("outbox: {e:?}")),
    };
    let (mut delivered, mut dropped, mut failed, mut dead) = (0u32, 0u32, 0u32, 0u32);
    for ev in &events {
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
            // Source deleted since enqueue: drop, don't retry forever.
            let _ = outbox::ack(&ev.id);
            dropped += 1;
            continue;
        }
        // The signature rides inside the envelope: notify:dispatch messages have no header field.
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

fn audit(event: &str, outcome: &str, source: &str, delivery: &str, detail: &str) {
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

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::NotFound,
        records::StoreError::InvalidJson(m) => Outcome::Bad(m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

/// Without a ceiling a read grew to wasmtime's 64 MiB cap and TRAPPED (a closed connection).
/// A backstop, not a content policy: uploads 413 on their own.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body!(MAX_BODY_BYTES);
guestio::guest_p3_respond!();

fn header(request: &Request, name: &str) -> Option<String> {
    request.get_headers().get(name).into_iter().find_map(|v| String::from_utf8(v).ok())
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let mut it = kv.splitn(2, '=');
        (it.next()? == key).then(|| it.next().unwrap_or("").to_string())
    })
}

fn emit(result: Outcome) -> Result<Response, ErrorCode> {
    match result {
        Outcome::Json(code, body) => answer(code, &[], body),
        Outcome::Limited(secs) => answer(
            429,
            &[("retry-after", &secs.to_string())],
            format!("{{\"error\":\"rate_limited\",\"retryAfter\":{secs}}}"),
        ),
        Outcome::Bad(msg) => answer(400, &[], json!({ "error": msg }).to_string()),
        Outcome::Err(code, msg) => answer(code, &[], json!({ "error": msg }).to_string()),
        Outcome::NotFound => answer(404, &[], "{\"error\":\"not_found\"}".to_string()),
    }
}

fn answer(status: u16, extra: &[(&str, &str)], body: String) -> Result<Response, ErrorCode> {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    for (k, v) in extra {
        let _ = headers.set(k.as_ref(), &[v.as_bytes().to_vec()]);
    }
    respond_with(status, headers, body.into_bytes())
}

bindings::export!(Component with_types_in bindings);
