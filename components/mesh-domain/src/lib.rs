//! `mesh-domain` — the resilience playground (docs/apps/MESH.md) as ONE composed wasm HTTP
//! component. Exports `wasi:http`; imports only WIT contracts: `resilience:breaker`
//! (the stateless breaker + backoff math), `records:store` (the durable per-key
//! circuit state), and `proxy:route` (the real outgoing hop to the upstream).
//!
//! One guarded call, in order:
//!
//!   1. `admit` — if the circuit is OPEN the request is SHED here: 503, and the
//!      upstream is never dialled. That is the whole point of a breaker.
//!   2. forward the real HTTP request through `proxy:route`.
//!   3. judge it: a 5xx, an unreachable upstream, or a response slower than
//!      `slo_ms` is a FAILURE. `observe` feeds that back into the circuit.
//!   4. on failure, wait `backoff(attempt)` and go again — re-checking `admit`
//!      each time, because our own retries may be what trips the breaker.
//!
//! Every circuit mutation is a revision-guarded `records:store` update, so
//! concurrent callers converge on one circuit instead of clobbering each other.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Map, Value};

use bindings::proxy::route::router;
use bindings::records::store::store as records;
use bindings::resilience::breaker::breaker as rb;
use bindings::wasi::clocks::{monotonic_clock, wall_clock};

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const CIRCUITS: &str = "circuits";
const CAS_TRIES: u32 = 40;
/// Hard ceiling on attempts per request, whatever the client asks for — a
/// playground still shouldn't let one request hold the host for a minute.
const MAX_ATTEMPTS_CAP: u32 = 8;
/// Hard ceiling on a single backoff sleep (ms), same reason.
const MAX_BACKOFF_CAP: u32 = 2_000;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage(),
            (Method::Post, ["api", "call"]) => call(&request),
            (Method::Get, ["api", "circuit", key]) => circuit_get(key),
            (Method::Get, ["api", "circuits"]) => circuits_all(),
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

/// Monotonic millis — for measuring a call, never for state timestamps.
fn mono_ms() -> u64 {
    monotonic_clock::now() / 1_000_000
}

fn sleep_ms(ms: u32) {
    if ms == 0 {
        return;
    }
    monotonic_clock::subscribe_duration(ms as u64 * 1_000_000).block();
}

fn usage() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "mesh",
            "about": "resilient upstream calls — retry with backoff + jitter, a durable circuit breaker, and an SLO that treats slow as failed",
            "call": "POST /api/call {key, path?, attempts?, base_ms?, factor_pct?, max_ms?, jitter?, slo_ms?, failure_threshold?, window_ms?, open_ms?, half_open_probes?, success_threshold?}",
            "circuit": "GET /api/circuit/{key}",
            "circuits": "GET /api/circuits",
            "reset": "POST /api/reset {key}",
            "upstream": "the demo upstream fails on demand: path /upstream/hit?fail=1 -> 500, ?delay=400 -> slow, killed -> unreachable"
        })
        .to_string(),
    )
}

// ---- policy (defaults + client overrides) -----------------------------------

struct Policies {
    breaker: rb::Policy,
    retry: rb::RetryPolicy,
    /// A response slower than this counts as a failure. 0 = no SLO.
    slo_ms: u64,
}

fn policies(b: &Value) -> Policies {
    let u32of = |k: &str, d: u32| b[k].as_u64().unwrap_or(d as u64).min(u32::MAX as u64) as u32;
    let u64of = |k: &str, d: u64| b[k].as_u64().unwrap_or(d);
    Policies {
        breaker: rb::Policy {
            failure_threshold: u32of("failure_threshold", 3).max(1),
            window_ms: u64of("window_ms", 10_000),
            open_ms: u64of("open_ms", 2_000),
            half_open_probes: u32of("half_open_probes", 1).max(1),
            success_threshold: u32of("success_threshold", 1).max(1),
        },
        retry: rb::RetryPolicy {
            max_attempts: u32of("attempts", 3).clamp(1, MAX_ATTEMPTS_CAP),
            base_ms: u32of("base_ms", 50),
            factor_pct: u32of("factor_pct", 200).max(100),
            max_ms: u32of("max_ms", 400).min(MAX_BACKOFF_CAP),
            jitter: b["jitter"].as_bool().unwrap_or(true),
        },
        slo_ms: u64of("slo_ms", 0),
    }
}

// ---- durable circuit state (records CAS = one circuit per key) --------------

fn zero_circuit() -> rb::Circuit {
    rb::Circuit {
        state: rb::CircuitState::Closed,
        failures: 0,
        successes: 0,
        window_start_ms: 0,
        changed_ms: 0,
        probes: 0,
    }
}

fn state_name(s: rb::CircuitState) -> &'static str {
    match s {
        rb::CircuitState::Closed => "closed",
        rb::CircuitState::Open => "open",
        rb::CircuitState::HalfOpen => "half-open",
    }
}

fn state_of(name: &str) -> rb::CircuitState {
    match name {
        "open" => rb::CircuitState::Open,
        "half-open" => rb::CircuitState::HalfOpen,
        _ => rb::CircuitState::Closed,
    }
}

fn circuit_from(v: &Value) -> rb::Circuit {
    let c = &v["circuit"];
    rb::Circuit {
        state: state_of(c["state"].as_str().unwrap_or("closed")),
        failures: c["failures"].as_u64().unwrap_or(0) as u32,
        successes: c["successes"].as_u64().unwrap_or(0) as u32,
        window_start_ms: c["window_start_ms"].as_u64().unwrap_or(0),
        changed_ms: c["changed_ms"].as_u64().unwrap_or(0),
        probes: c["probes"].as_u64().unwrap_or(0) as u32,
    }
}

fn circuit_json(c: &rb::Circuit) -> Value {
    json!({
        "state": state_name(c.state), "failures": c.failures, "successes": c.successes,
        "window_start_ms": c.window_start_ms, "changed_ms": c.changed_ms, "probes": c.probes
    })
}

/// Read-modify-write one key's circuit under a revision compare-and-set.
///
/// `f` gets the current circuit plus the record's counters and returns
/// `(result, new circuit, counter deltas)`. Retried on a revision conflict, so a
/// racing caller can't lose a trip. Returns none only under sustained contention.
///
/// `open_ms` is stored alongside the state purely so a READ of the circuit can
/// report an honest "probe admitted in N ms" without guessing the policy.
fn cas_circuit<T>(
    key: &str,
    open_ms: u64,
    f: impl Fn(rb::Circuit) -> (T, rb::Circuit, Counters),
) -> Option<T> {
    for _ in 0..CAS_TRIES {
        let current = records::find_by(CIRCUITS, "key", &json!(key).to_string())
            .ok()?
            .into_iter()
            .next()
            .and_then(|e| {
                serde_json::from_str::<Value>(&e.data).ok().map(|v| (e.id, e.revision, v))
            });

        let (existing, doc) = match current {
            Some((id, rev, v)) => (Some((id, rev)), v),
            None => (
                None,
                json!({ "key": key, "circuit": circuit_json(&zero_circuit()), "stats": stats_json(&Counters::default()) }),
            ),
        };
        let (out, next, delta) = f(circuit_from(&doc));
        let mut stats = counters_from(&doc["stats"]);
        stats.add(&delta);
        let nv = json!({ "key": key, "circuit": circuit_json(&next), "stats": stats_json(&stats), "open_ms": open_ms });

        let committed = match &existing {
            Some((id, rev)) => records::update(CIRCUITS, id, &nv.to_string(), *rev).is_ok(),
            None => records::create(CIRCUITS, &nv.to_string(), &["key".to_string()]).is_ok(),
        };
        if committed {
            return Some(out);
        }
        // revision conflict (or a lost create race) -> re-read and retry.
    }
    None
}

/// Observable counters per circuit — what the dashboard shows.
#[derive(Default)]
struct Counters {
    /// Attempts that actually left the host.
    attempts: u64,
    ok: u64,
    failed: u64,
    /// Requests refused by an open circuit (the upstream was never dialled).
    shed: u64,
    trips: u64,
}

impl Counters {
    fn add(&mut self, d: &Counters) {
        self.attempts += d.attempts;
        self.ok += d.ok;
        self.failed += d.failed;
        self.shed += d.shed;
        self.trips += d.trips;
    }
}

fn counters_from(v: &Value) -> Counters {
    let n = |k: &str| v[k].as_u64().unwrap_or(0);
    Counters {
        attempts: n("attempts"),
        ok: n("ok"),
        failed: n("failed"),
        shed: n("shed"),
        trips: n("trips"),
    }
}

fn stats_json(c: &Counters) -> Value {
    json!({ "attempts": c.attempts, "ok": c.ok, "failed": c.failed, "shed": c.shed, "trips": c.trips })
}

// ---- the guarded call -------------------------------------------------------

fn call(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let key = match b["key"].as_str().filter(|s| !s.is_empty()) {
        Some(k) => k.to_string(),
        None => return Outcome::Err(422, "key required".into()),
    };
    // The client picks which upstream path to hit (that's how the demo asks for a
    // failure: /upstream/hit?fail=1). It is NOT an open proxy: `proxy:route` only
    // forwards paths that match its configured route table, everything else is
    // `no-route`. Still require an absolute path so a stray value can't be read
    // as a full URL.
    let path = b["path"].as_str().unwrap_or("/upstream/hit").to_string();
    if !path.starts_with('/') {
        return Outcome::Err(422, "path must start with /".into());
    }
    let pol = policies(&b);
    let mut attempts_log: Vec<Value> = Vec::new();
    let started = mono_ms();
    let mut last_status = 0u16;
    let mut last_error: Option<String> = None;

    for attempt in 1..=pol.retry.max_attempts {
        // Wait out the backoff before a retry (never before the first attempt).
        if attempt > 1 {
            match rb::backoff(attempt, pol.retry, now_ms() ^ attempt as u64) {
                Some(ms) => sleep_ms(ms.min(MAX_BACKOFF_CAP)),
                None => break, // out of attempts
            }
        }

        // ---- 1. admit? (an open circuit sheds the request right here) -------
        let now = now_ms();
        let admission = cas_circuit(&key, pol.breaker.open_ms, |c| {
            let (a, next) = rb::admit(c, now, pol.breaker);
            let delta = Counters { shed: if a.admit { 0 } else { 1 }, ..Counters::default() };
            (a, next, delta)
        });
        let admission = match admission {
            Some(a) => a,
            None => return Outcome::Err(503, "circuit contended, retry".into()),
        };
        if !admission.admit {
            // Fail fast. Note `attempts` here counts only calls that were MADE.
            return Outcome::Json(
                503,
                json!({
                    "ok": false, "shed": true, "state": state_name(admission.state),
                    "retry_after_ms": admission.retry_after_ms, "attempts": attempts_log,
                    "total_ms": mono_ms().saturating_sub(started), "key": key,
                    "detail": "circuit open — the upstream was not called"
                })
                .to_string(),
            );
        }

        // ---- 2. the real outgoing HTTP hop ---------------------------------
        let t0 = mono_ms();
        let forwarded = router::forward("GET", &path, &[], &[]);
        let elapsed = mono_ms().saturating_sub(t0);

        // ---- 3. judge it ---------------------------------------------------
        let (ok, status, err) = match &forwarded {
            Ok(r) if r.status >= 500 => (false, r.status, Some(format!("upstream {}", r.status))),
            Ok(r) if pol.slo_ms > 0 && elapsed > pol.slo_ms => {
                // Slow is not success. We cannot cancel an in-flight wasi:http
                // request, so this is an SLO judgement after the fact, not a
                // timeout that frees the connection.
                (false, r.status, Some(format!("slo breach: {elapsed}ms > {}ms", pol.slo_ms)))
            }
            Ok(r) => (true, r.status, None),
            Err(router::ProxyError::NoRoute) => {
                // A missing route is OUR misconfiguration, not an upstream
                // failure — don't let it trip the breaker.
                return Outcome::Err(
                    502,
                    format!("no route configured for {path} (set CFG_ROUTES)"),
                );
            }
            Err(router::ProxyError::UpstreamUnreachable(m)) => {
                (false, 0, Some(format!("unreachable: {m}")))
            }
        };
        last_status = status;
        last_error = err.clone();

        // ---- 4. feed the outcome back --------------------------------------
        let now = now_ms();
        let state = cas_circuit(&key, pol.breaker.open_ms, |c| {
            let before = c.state;
            let next = rb::observe(c, now, pol.breaker, ok);
            let tripped = !matches!(before, rb::CircuitState::Open)
                && matches!(next.state, rb::CircuitState::Open);
            let delta = Counters {
                attempts: 1,
                ok: ok as u64,
                failed: !ok as u64,
                trips: tripped as u64,
                shed: 0,
            };
            (next.state, next, delta)
        })
        .map(state_name)
        .unwrap_or("unknown");

        attempts_log.push(json!({
            "n": attempt, "ok": ok, "status": status, "ms": elapsed,
            "state": state, "error": err
        }));

        if ok {
            let body = forwarded.ok().map(|r| String::from_utf8_lossy(&r.body).to_string());
            return Outcome::Json(
                200,
                json!({
                    "ok": true, "shed": false, "state": state, "status": status,
                    "attempts": attempts_log, "total_ms": mono_ms().saturating_sub(started),
                    "key": key, "upstream_body": body
                })
                .to_string(),
            );
        }
    }

    // Every attempt failed — surface it as a bad gateway with the whole trail.
    let state =
        current_doc(&key).map(|v| v["circuit"]["state"].as_str().unwrap_or("closed").to_string());
    Outcome::Json(
        502,
        json!({
            "ok": false, "shed": false, "status": last_status, "error": last_error,
            "state": state.unwrap_or_else(|| "closed".into()), "attempts": attempts_log,
            "total_ms": mono_ms().saturating_sub(started), "key": key
        })
        .to_string(),
    )
}

// ---- reads ------------------------------------------------------------------

fn current_doc(key: &str) -> Option<Value> {
    records::find_by(CIRCUITS, "key", &json!(key).to_string())
        .ok()?
        .into_iter()
        .next()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok())
}

/// A circuit as the UI wants it: state, counters, and — when open — how long
/// until a probe is admitted. Derived on read; nothing here mutates the circuit
/// (a GET must not spend the half-open probe budget).
fn view(doc: &Value, now: u64) -> Value {
    let c = circuit_from(doc);
    let open_ms = doc["open_ms"].as_u64().unwrap_or(2_000);
    let open_for = if matches!(c.state, rb::CircuitState::Open) {
        now.saturating_sub(c.changed_ms)
    } else {
        0
    };
    let mut v = doc.clone();
    v["retry_after_ms"] = json!(open_ms.saturating_sub(open_for));
    v["would_admit"] = json!(!matches!(c.state, rb::CircuitState::Open) || open_for >= open_ms);
    v["open_for_ms"] = json!(open_for);
    v
}

fn circuit_get(key: &str) -> Outcome {
    match current_doc(key) {
        Some(doc) => Outcome::Json(200, view(&doc, now_ms()).to_string()),
        None => Outcome::Json(
            200,
            json!({ "key": key, "circuit": circuit_json(&zero_circuit()), "stats": stats_json(&Counters::default()), "would_admit": true, "retry_after_ms": 0, "open_for_ms": 0 })
                .to_string(),
        ),
    }
}

fn circuits_all() -> Outcome {
    let now = now_ms();
    let list: Vec<Value> = records::list_records(CIRCUITS, 200, "")
        .map(|p| p.entries)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .map(|doc| view(&doc, now))
        .collect();
    Outcome::Json(200, json!({ "circuits": list }).to_string())
}

fn reset(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let key = match b["key"].as_str().filter(|s| !s.is_empty()) {
        Some(k) => k.to_string(),
        None => return Outcome::Err(422, "key required".into()),
    };
    for e in records::find_by(CIRCUITS, "key", &json!(key).to_string()).unwrap_or_default() {
        let _ = records::delete(CIRCUITS, &e.id);
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
/// There was no ceiling anywhere: when this was measured, 148 of the tree's 150
/// components accumulated whatever arrived until the guest hit wasmtime's 64 MiB
/// per-store memory cap and TRAPPED, which reaches the caller as a closed
/// connection saying nothing about a size.
/// A component that answers JSON has no business reading sixteen megabytes, and
/// the ones that legitimately handle uploads police it themselves with a 413 and a
/// granted max-size — those are left alone.
///
/// Generous on purpose. This is a backstop against an unbounded read, not a
/// content policy; an API that needs a real limit should state its own and say 413.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_read_body!(MAX_BODY_BYTES);
guestio::guest_write_all!();

fn emit(response_out: ResponseOutparam, result: Outcome) {
    let (code, body) = match result {
        Outcome::Json(c, b) => (c, b),
        Outcome::Err(c, m) => (c, json!({ "error": m }).to_string()),
    };
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(code);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    let bytes = body.as_bytes();
    if !bytes.is_empty() {
        let stream = out.write().expect("write stream");
        let _ = write_all(&stream, bytes);
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
