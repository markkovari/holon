//! `gate-domain` — a durable traffic-shaping gateway (docs/apps/GATE.md) as ONE composed
//! wasm HTTP component. Exports `wasi:http`; imports only WIT contracts:
//! `records:store` (the durable per-key state — the "worker" memory) and
//! `shaper:limit` (the stateless token-bucket / GCRA math). Per-key records are
//! updated under a revision compare-and-set, so concurrent requests to one key
//! serialize like a single-writer Golem worker; batch flush is one CAS update
//! (the atomic region). No auth — a gateway keys by a client-supplied API key.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Map, Value};

use bindings::comp::store::cas;
use bindings::records::store::store as records;
use bindings::wasi::keyvalue::store as kv;
use bindings::shaper::limit::limiter as shaper;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const GCRA: &str = "gcra";
const BATCHES: &str = "batches";
/// How many times a contended compare-and-set is retried before giving up.
///
/// 200, not the 40 it was, and the number is arithmetic rather than taste. With
/// N concurrent writers on one key exactly one wins each round, so a request
/// fails after K rounds with probability ((N-1)/N)^K. At 20 writers and 40
/// rounds that is 12.9% — and a measured 9.7% of hot-key requests came back 503
/// when the rate limiter stopped going through `record-store`.
///
/// It was hidden before: `record-store::update` ran its OWN 40-try loop inside
/// each of these, so the effective budget was 1600. Doing two store operations
/// per attempt instead of eight silently cut the budget by 40×. At 200 the same
/// arithmetic gives 0.0035%, and each attempt is now cheap enough that the worst
/// case is still less work than one old attempt.
const CAS_TRIES: u32 = 200;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage(),
            (Method::Post, ["api", "ratelimit"]) => ratelimit(&request),
            (Method::Post, ["api", "throttle"]) => throttle(&request),
            (Method::Post, ["api", "batch", "submit"]) => batch_submit(&request),
            (Method::Get, ["api", "batch", id]) => batch_get(id),
            (Method::Post, ["api", "reset"]) => reset(&request),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
}

fn now_ms() -> u64 {
    let t = wall_clock::now();
    t.seconds * 1000 + (t.nanoseconds / 1_000_000) as u64
}

fn usage() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "gate",
            "about": "durable traffic-shaping gateway — per-key token-bucket rate limit, GCRA throttle, request batching (the Golem durable-worker patterns)",
            "ratelimit": "POST /api/ratelimit {key, capacity?, refill?, cost?} -> 200/429",
            "throttle": "POST /api/throttle {key, rate?, burst?, cost?} -> 200/429",
            "batch": "POST /api/batch/submit {key, item, max_size?, max_age_ms?}, GET /api/batch/{id}",
            "reset": "POST /api/reset {key}"
        })
        .to_string(),
    )
}

// ---- durable per-key state (records CAS = the single-writer worker) ---------

/// The current record for `key` in `coll`: (id, revision, data). `find_by`
/// returns id-ordered, so racers converge on the same (earliest) record.
fn state_of(coll: &str, key: &str) -> Option<(String, u64, Value)> {
    records::find_by(coll, "key", &json!(key).to_string())
        .ok()?
        .into_iter()
        .next()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok().map(|v| (e.id, e.revision, v)))
}

// ---- rate limit (token bucket) ----------------------------------------------

/// Where one key's bucket lives. Not a record: a bucket has no identity beyond
/// its key, is never listed, and is rewritten on every single request — so the
/// id index and secondary index a `record` carries are pure overhead on the
/// hottest path in this component (bench/FLEET-BENCH.md measured ~8 store round
/// trips per request, of which two were the actual read and write).
fn bucket_key(key: &str) -> String {
    let mut out = String::from("rl_");
    for b in key.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn open_bucket() -> Result<kv::Bucket, Outcome> {
    kv::open("default").map_err(|e| Outcome::Err(503, format!("store unavailable: {e:?}")))
}


fn ratelimit(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let key = match b["key"].as_str().filter(|s| !s.is_empty()) {
        Some(k) => k.to_string(),
        None => return Outcome::Err(422, "key required".into()),
    };
    let capacity = b["capacity"].as_f64().unwrap_or(5.0).max(1.0);
    let refill = b["refill"].as_f64().unwrap_or(1.0).max(0.0);
    let cost = b["cost"].as_f64().unwrap_or(1.0).max(0.0);
    let now = now_ms();

    let bucket = match open_bucket() {
        Ok(b) => b,
        Err(o) => return o,
    };
    let bkey = bucket_key(&key);

    // Two store operations per request: the guarded read, and the guarded write.
    // The revision comes from the store rather than from a record's own counter,
    // so this is the same optimistic-concurrency loop it always was — it just no
    // longer pays for an index it never queries.
    for _ in 0..CAS_TRIES {
        let (rev, state) = match cas::get(&bucket, &bkey) {
            Ok(Some(v)) => {
                let parsed: Value = serde_json::from_slice(&v.value).unwrap_or(Value::Null);
                (
                    v.revision,
                    shaper::Bucket {
                        tokens: parsed["tokens"].as_f64().unwrap_or(0.0),
                        updated_ms: parsed["updated_ms"].as_u64().unwrap_or(0),
                    },
                )
            }
            // uninitialized -> starts full (updated_ms 0). Revision 0 is "must
            // not exist yet", so two racing first-requests cannot both create it.
            Ok(None) => (0, shaper::Bucket { tokens: 0.0, updated_ms: 0 }),
            Err(e) => return Outcome::Err(503, format!("store unavailable: {e:?}")),
        };
        let (dec, next) = shaper::token_bucket(state, now, capacity, refill, cost);
        let nv = json!({ "key": key, "tokens": next.tokens, "updated_ms": next.updated_ms });
        match cas::set(&bucket, &bkey, nv.to_string().as_bytes(), rev) {
            Ok(cas::Outcome::Committed(_)) => {
                return decide_response(&dec, "token-bucket", &key)
            }
            // Someone else moved it between the read and the write: re-read and
            // decide again on what they left behind.
            Ok(cas::Outcome::Conflict(_)) => continue,
            Err(e) => return Outcome::Err(503, format!("store unavailable: {e:?}")),
        }
    }
    Outcome::Err(503, "contended, retry".into())
}

// ---- throttle (GCRA) --------------------------------------------------------

fn throttle(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let key = match b["key"].as_str().filter(|s| !s.is_empty()) {
        Some(k) => k.to_string(),
        None => return Outcome::Err(422, "key required".into()),
    };
    let rate = b["rate"].as_f64().unwrap_or(2.0).max(0.001);
    let period_ms = (1000.0 / rate).round() as u64;
    let burst = b["burst"].as_u64().unwrap_or(3) as u32;
    let cost = b["cost"].as_u64().unwrap_or(1).max(1) as u32;
    let now = now_ms();

    for _ in 0..CAS_TRIES {
        let (tat, existing) = match state_of(GCRA, &key) {
            Some((id, rev, v)) => (v["tat"].as_u64().unwrap_or(0), Some((id, rev))),
            None => (0, None),
        };
        let (dec, new_tat) = shaper::gcra(tat, now, period_ms, burst, cost);
        let nv = json!({ "key": key, "tat": new_tat });
        let committed = match &existing {
            Some((id, rev)) => matches!(records::update(GCRA, id, &nv.to_string(), *rev), Ok(_)),
            None => records::create(GCRA, &nv.to_string(), &["key".to_string()]).is_ok(),
        };
        if committed {
            return decide_response(&dec, "gcra", &key);
        }
    }
    Outcome::Err(503, "contended, retry".into())
}

fn decide_response(dec: &shaper::Decision, algo: &str, key: &str) -> Outcome {
    let body = json!({
        "allowed": dec.allowed, "retry_after_ms": dec.retry_after_ms,
        "remaining": dec.remaining, "algo": algo, "key": key
    })
    .to_string();
    // a real gateway answers 429 on a denial.
    Outcome::Json(if dec.allowed { 200 } else { 429 }, body)
}

// ---- batch (durable coalescer, atomic flush) --------------------------------

/// The "downstream work" a batch performs, per item — here an uppercase to make
/// the coalescing visible. In a real gateway this is the one batched call.
fn process(item: &str) -> String {
    item.to_uppercase()
}

fn batch_submit(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let key = match b["key"].as_str().filter(|s| !s.is_empty()) {
        Some(k) => k.to_string(),
        None => return Outcome::Err(422, "key required".into()),
    };
    let item = b["item"].as_str().unwrap_or("").to_string();
    let max_size = b["max_size"].as_u64().unwrap_or(5).max(1);
    let max_age = b["max_age_ms"].as_u64().unwrap_or(3000);
    let now = now_ms();

    for _ in 0..CAS_TRIES {
        // an OPEN (not-yet-flushed) batch for this key, if any.
        let open = records::find_by(BATCHES, "key", &json!(key).to_string())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok().map(|v| (e.id, e.revision, v)))
            .find(|(_, _, v)| !v["flushed"].as_bool().unwrap_or(false));

        match open {
            None => {
                // start a new batch with this item.
                let d = json!({
                    "key": key, "items": [item], "created_ms": now,
                    "max_size": max_size, "max_age_ms": max_age, "flushed": false, "results": Value::Null
                });
                if let Ok(rec) = records::create(BATCHES, &d.to_string(), &["key".to_string()]) {
                    return Outcome::Json(201, json!({ "batch": rec.id, "index": 0, "size": 1, "flushed": false }).to_string());
                }
                // lost the create race -> loop, find the open batch, append.
            }
            Some((id, rev, mut v)) => {
                let items = v["items"].as_array_mut().unwrap();
                items.push(json!(item));
                let index = items.len() - 1;
                let size = items.len() as u64;
                let created = v["created_ms"].as_u64().unwrap_or(now);
                // flush when full or the window has aged out.
                let flush = size >= v["max_size"].as_u64().unwrap_or(max_size)
                    || now.saturating_sub(created) >= v["max_age_ms"].as_u64().unwrap_or(max_age);
                let mut my_result = Value::Null;
                if flush {
                    let results: Vec<Value> = v["items"].as_array().unwrap().iter().map(|it| json!(process(it.as_str().unwrap_or("")))).collect();
                    my_result = results.get(index).cloned().unwrap_or(Value::Null);
                    v["results"] = json!(results);
                    v["flushed"] = json!(true);
                    v["flushed_ms"] = json!(now);
                }
                // ATOMIC REGION: the append (and, if tripped, the flush) commit as
                // one revision-guarded update — a crash can't flush twice or lose items.
                if records::update(BATCHES, &id, &v.to_string(), rev).is_ok() {
                    return Outcome::Json(
                        201,
                        json!({ "batch": id, "index": index, "size": size, "flushed": flush, "result": my_result }).to_string(),
                    );
                }
                // revision conflict -> another submit landed first; retry.
            }
        }
    }
    Outcome::Err(503, "contended, retry".into())
}

fn batch_get(id: &str) -> Outcome {
    let e = match records::get(BATCHES, id) {
        Ok(e) => e,
        Err(_) => return Outcome::Err(404, "not_found".into()),
    };
    let mut v: Value = serde_json::from_str(&e.data).unwrap_or_else(|_| json!({}));
    v["id"] = json!(id);
    v["size"] = json!(v["items"].as_array().map(|a| a.len()).unwrap_or(0));
    v["age_ms"] = json!(now_ms().saturating_sub(v["created_ms"].as_u64().unwrap_or(0)));
    Outcome::Json(200, v.to_string())
}

// ---- reset (clear a key's durable state, for demo replay) -------------------

fn reset(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let key = match b["key"].as_str().filter(|s| !s.is_empty()) {
        Some(k) => k.to_string(),
        None => return Outcome::Err(422, "key required".into()),
    };
    // The bucket is no longer a record, so it is no longer reachable by the loop
    // below — and a reset that silently stopped resetting the rate limit would be
    // the worst kind of quiet.
    if let Ok(bucket) = kv::open("default") {
        let _ = bucket.delete(&bucket_key(&key));
    }
    for coll in [GCRA, BATCHES] {
        for e in records::find_by(coll, "key", &json!(key).to_string()).unwrap_or_default() {
            let _ = records::delete(coll, &e.id);
        }
    }
    Outcome::Json(200, json!({ "ok": true, "key": key }).to_string())
}

// ---- http plumbing ----------------------------------------------------------

fn body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let raw = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
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

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let b = request.consume().map_err(|_| ())?;
    let stream = b.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                // A ceiling, not a policy: past this the read stops and the caller
                // is told, rather than growing until the store's memory cap traps
                // the component and the connection just closes.
                if buf.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(());
                }
                buf.extend_from_slice(&chunk);
            }
            // `Closed` is how wasi:io says end-of-body; `LastOperationFailed` is a
            // read that went wrong. Collapsing both into `break` returns a TRUNCATED
            // body as if it were complete — the same silent truncation that, on the
            // write side, took four runs to find.
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(_) => return Err(()),
        }
    }
    Ok(buf)
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    let (code, body) = match result {
        Outcome::Json(c, b) => (c, b),
        Outcome::Err(c, m) => (c, json!({ "error": m }).to_string()),
    };
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    let _ = headers.set(&"access-control-allow-origin".to_string(), &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(code);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    let bytes = body.as_bytes();
    if !bytes.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in bytes.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
