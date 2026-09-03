//! saga:app — a durable trip-booking saga over composed contracts.
//!
//! Book flight → hotel → car in sequence; if a leg fails, compensate the booked
//! legs in reverse. The engine is a step machine: `step()` does ONE unit of work
//! (book the next pending leg, run one compensation, or finalize) and PERSISTS,
//! so a saga is fully resumable — `run()` loops `step()` to a terminal state;
//! `pump()` advances every live saga one step (for timers / retries / restart
//! resume). Nothing lives in component memory.
//!
//! Durable state = a `saga` record (steps + cursor) + the `fsm:workflow`
//! instance (running → committed | compensating → compensated | failed). Each
//! book/compensate is fenced by `idempotency:guard`, so re-running or re-pumping
//! never double-books or double-refunds. Every move emits an `event:bus` event.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};

use bindings::event::bus::bus;
use bindings::fsm::workflow::engine as fsm;
use bindings::id::generate::generator as ids;
use bindings::idempotency::guard::store as idem;
use bindings::records::store::store as records;
use bindings::sched::timer::timer;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingRequest, OutgoingResponse,
    RequestOptions, ResponseOutparam, Scheme,
};

struct Component;

const SAGAS: &str = "sagas";
const BOOKINGS: &str = "bookings";
const MACHINE: &str = "saga";
const LEGS: [&str; 3] = ["flight", "hotel", "car"];
// ponytail: retry ceiling for a flaky leg — a config knob (`max-attempts`) in
// production; a const here keeps rung 3 dependency-free.
const MAX_ATTEMPTS: u64 = 3;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let result = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage_json(),
            (Method::Post, ["trips"]) => create_trip(&request),
            (Method::Get, ["trips", id]) => get_trip(id),
            (Method::Post, ["trips", id, "run"]) => run_trip(id),
            (Method::Post, ["internal", "pump"]) => pump(),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, result);
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "saga",
            "about": "durable trip-booking saga (flight → hotel → car, compensate on failure)",
            "start": "POST /trips {traveler, failLeg?}",
            "status": "GET /trips/{id}",
            "run": "POST /trips/{id}/run   (drive to committed | compensated)",
            "pump": "POST /internal/pump   (advance every live saga one step)"
        })
        .to_string(),
    )
}

// ---- machine + pricing -------------------------------------------------------

/// Idempotent: define the saga lifecycle machine once (gated on a meta record).
fn ensure_seeded() {
    if records::count("meta").map(|n| n > 0).unwrap_or(false) {
        return;
    }
    fn t(event: &str, source: &str, target: &str) -> fsm::Transition {
        fsm::Transition { event: event.into(), source: source.into(), target: target.into() }
    }
    let def = fsm::Definition {
        states: ["running", "committed", "compensating", "compensated", "failed"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        initial: "running".into(),
        transitions: vec![
            t("commit", "running", "committed"),
            t("fail", "running", "compensating"),
            t("compensated", "compensating", "compensated"),
            t("abort", "compensating", "failed"),
        ],
        terminal: vec!["committed".into(), "compensated".into(), "failed".into()],
    };
    let _ = fsm::define(MACHINE, &def);
    let _ = records::create("meta", "{\"seeded\":true}", &[]);
}

fn price(leg: &str) -> u64 {
    match leg {
        "flight" => 42000,
        "hotel" => 18000,
        _ => 9000, // car
    }
}
fn ref_prefix(leg: &str) -> &str {
    match leg {
        "flight" => "FL",
        "hotel" => "HT",
        _ => "CR",
    }
}

// ---- endpoints ---------------------------------------------------------------

fn create_trip(request: &IncomingRequest) -> Outcome {
    ensure_seeded();
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let traveler = body["traveler"].as_str().unwrap_or("").trim().to_string();
    if traveler.is_empty() {
        return Outcome::Err(422, "traveler required".into());
    }
    // failLeg (optional) makes that leg fail permanently — the compensation demo.
    // flakyLeg + flakyFails (optional) make a leg fail transiently that many
    // times before succeeding — the retry demo. Both must name a known leg.
    let fail_leg = body["failLeg"].as_str().filter(|l| LEGS.contains(l)).unwrap_or("");
    let flaky_leg = body["flakyLeg"].as_str().filter(|l| LEGS.contains(l)).unwrap_or("");
    let flaky_fails = body["flakyFails"].as_u64().unwrap_or(1);
    let steps: Vec<Value> = LEGS
        .iter()
        .map(|leg| json!({"leg": leg, "state": "pending", "ref": "", "price": price(leg), "at": 0, "attempts": 0}))
        .collect();
    // golem-backed legs (optional): when `golemUrl` is set, each leg is booked by
    // invoking a durable Golem worker over HTTP instead of the in-process
    // simulation. `golemHost` is the gateway `Host` header for subdomain routing.
    let golem_url = body["golemUrl"].as_str().unwrap_or("");
    let golem_host = body["golemHost"].as_str().unwrap_or("");
    let data = json!({
        "traveler": traveler,
        "status": "running",
        "startedAt": now(),
        "failLeg": fail_leg,
        "flakyLeg": flaky_leg,
        "flakyFails": flaky_fails,
        "golemUrl": golem_url,
        "golemHost": golem_host,
        "steps": steps,
    });
    let entry = match records::create(SAGAS, &data.to_string(), &["status".to_string()]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    let _ = fsm::create_instance(MACHINE, &entry.id);
    publish("saga.started", &json!({"saga": entry.id, "traveler": traveler}));
    Outcome::Json(201, saga_json(&entry.id))
}

fn get_trip(id: &str) -> Outcome {
    match records::get(SAGAS, id) {
        Ok(_) => Outcome::Json(200, saga_json(id)),
        Err(records::StoreError::NotFound) => Outcome::Err(404, "not_found".into()),
        Err(e) => store_err(e),
    }
}

/// Drive one saga to a terminal state (loop `step` until it stops advancing).
fn run_trip(id: &str) -> Outcome {
    if records::get(SAGAS, id).is_err() {
        return Outcome::Err(404, "not_found".into());
    }
    // legs + commit + up to 3 compensations + finalize — 20 is a safe ceiling.
    for _ in 0..20 {
        if !step(id) {
            break;
        }
    }
    Outcome::Json(200, saga_json(id))
}

/// Advance every live (running | compensating) saga by ONE step. This is the
/// resume/retry entry point: after a host restart, pumping picks each saga up
/// exactly where its persisted state left off.
fn pump() -> Outcome {
    // Drain due retry timers: each marks a `retrying` leg eligible again; the
    // step pass below re-attempts it. Ack so a one-shot timer doesn't re-fire.
    if let Ok(jobs) = timer::due(now(), 100, 60) {
        for j in jobs {
            let _ = timer::ack(&j.key);
        }
    }
    let mut advanced = 0u32;
    for status in ["running", "compensating"] {
        let live =
            records::find_by(SAGAS, "status", &json!(status).to_string()).unwrap_or_default();
        for e in live {
            if step(&e.id) {
                advanced += 1;
            }
        }
    }
    Outcome::Json(200, json!({ "advanced": advanced }).to_string())
}

// ---- the step machine --------------------------------------------------------

/// One unit of work for saga `id`; persists and returns whether it advanced.
/// Terminal sagas return false.
fn step(id: &str) -> bool {
    let entry = match records::get(SAGAS, id) {
        Ok(e) => e,
        Err(_) => return false,
    };
    let mut data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    let status = data["status"].as_str().unwrap_or("").to_string();
    let fail_leg = data["failLeg"].as_str().unwrap_or("").to_string();
    let flaky_leg = data["flakyLeg"].as_str().unwrap_or("").to_string();
    let flaky_fails = data["flakyFails"].as_u64().unwrap_or(0);
    let golem = golem_opts(&data);

    match status.as_str() {
        "running" => {
            match next_active(&data) {
                None => {
                    // every leg booked → commit
                    set_status(&mut data, fire(id, "commit").unwrap_or_else(|| "committed".into()));
                    save(id, &data);
                    publish("saga.committed", &json!({"saga": id}));
                }
                Some(i) => {
                    let leg = leg_name(&data, i);
                    let attempts = data["steps"][i]["attempts"].as_u64().unwrap_or(0);
                    let timer_key = format!("saga:{id}:{leg}");
                    if fail_leg == leg {
                        begin_compensation(id, &mut data, i, &leg); // permanent failure
                    } else if flaky_leg == leg && attempts < flaky_fails {
                        // this attempt fails transiently…
                        if attempts + 1 < MAX_ATTEMPTS {
                            // …and we still have retries: record + arm a retry timer.
                            data["steps"][i]["attempts"] = json!(attempts + 1);
                            data["steps"][i]["state"] = json!("retrying");
                            let _ = timer::schedule_at(&timer_key, now(), b""); // backoff 0 = eligible now
                            save(id, &data);
                            publish(
                                "saga.leg.retry",
                                &json!({"saga": id, "leg": leg, "attempt": attempts + 1}),
                            );
                        } else {
                            // …and retries are exhausted: give up and roll back.
                            let _ = timer::cancel(&timer_key);
                            begin_compensation(id, &mut data, i, &leg);
                        }
                    } else {
                        // success (also the recovery of a previously-flaky leg)
                        let _ = timer::cancel(&timer_key);
                        match book_leg(
                            id,
                            &leg,
                            golem.as_ref().map(|(u, h)| (u.as_str(), h.as_str())),
                        ) {
                            Ok(reference) => {
                                set_leg(&mut data, i, "booked", &reference);
                                save(id, &data);
                                publish(
                                    "saga.leg.booked",
                                    &json!({"saga": id, "leg": leg, "ref": reference}),
                                );
                            }
                            // the leg's durable provider (Golem) failed → roll back,
                            // like any other leg failure. Prior legs are compensated.
                            Err(e) => {
                                data["lastError"] = json!(e);
                                begin_compensation(id, &mut data, i, &leg);
                            }
                        }
                    }
                }
            }
            true
        }
        "compensating" => {
            // undo the last still-booked leg; when none remain, finish.
            match last_booked(&data) {
                Some(i) => {
                    let leg = leg_name(&data, i);
                    compensate_leg(id, &leg);
                    set_leg(&mut data, i, "compensated", "");
                    save(id, &data);
                    publish("saga.leg.compensated", &json!({"saga": id, "leg": leg}));
                    true
                }
                None => {
                    set_status(
                        &mut data,
                        fire(id, "compensated").unwrap_or_else(|| "compensated".into()),
                    );
                    save(id, &data);
                    publish("saga.compensated", &json!({"saga": id}));
                    true
                }
            }
        }
        _ => false, // terminal
    }
}

/// Reserve a leg, fenced so the booking record is created at most once. Returns
/// the booking ref (a fresh one, or the replayed ref if already booked). The
/// decision to book/fail/retry is the caller's; this only performs the reserve.
fn book_leg(saga: &str, leg: &str, golem: Option<(&str, &str)>) -> Result<String, String> {
    let key = format!("saga:{saga}:book:{leg}");
    if let Ok(Some(cached)) = idem::begin(&key, 3600) {
        return Ok(String::from_utf8_lossy(&cached.body).into_owned()); // idempotent replay
    }
    // first caller (single-writer per saga, so in-progress can't race here).
    let reference = match golem {
        // book the leg by invoking a REAL durable Golem worker over HTTP.
        Some((url, host)) => match golem_book(url, host, &format!("{leg}-{saga}")) {
            Ok(result) => format!("{}-golem-{result}", ref_prefix(leg)),
            Err(e) => {
                let _ = idem::forget(&key); // release the key so a retry can re-attempt
                return Err(e);
            }
        },
        // in-process simulated booking.
        None => format!("{}-{}", ref_prefix(leg), ids::short_code(6)),
    };
    let booking = json!({"saga": saga, "leg": leg, "ref": reference, "price": price(leg)});
    let _ = records::create(BOOKINGS, &booking.to_string(), &["saga".to_string()]);
    let _ = idem::complete(&key, 200, reference.as_bytes());
    Ok(reference)
}

/// The (url, host) of the Golem gateway if this saga's legs are golem-backed.
fn golem_opts(data: &Value) -> Option<(String, String)> {
    let url = data["golemUrl"].as_str().unwrap_or("");
    if url.is_empty() {
        return None;
    }
    Some((url.to_string(), data["golemHost"].as_str().unwrap_or("").to_string()))
}

/// Invoke a durable Golem worker via the API gateway (`POST
/// {url}/counters/{workflow}/increment`) and return its result body. This is the
/// same call the `golem-workflow` provider makes — here the saga makes it
/// directly over `wasi:http`, so a saga leg IS a crash-proof Golem worker.
fn golem_book(url: &str, host: &str, workflow: &str) -> Result<String, String> {
    let (scheme, url_authority) = if let Some(rest) = url.strip_prefix("https://") {
        (Scheme::Https, rest.trim_end_matches('/').to_string())
    } else {
        (Scheme::Http, url.trim_start_matches("http://").trim_end_matches('/').to_string())
    };
    // wasi:http derives the `Host` header from the authority (a manual `host`
    // field is ignored), and Golem's gateway routes by subdomain Host. So the
    // authority MUST be the gateway host (e.g. `app.localhost:9006`) — it
    // resolves to loopback locally and yields the right `Host` automatically.
    let authority = if host.is_empty() { url_authority } else { host.to_string() };
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| "set method".to_string())?;
    req.set_scheme(Some(&scheme)).map_err(|_| "set scheme".to_string())?;
    req.set_authority(Some(&authority)).map_err(|_| "set authority".to_string())?;
    req.set_path_with_query(Some(&format!("/counters/{workflow}/increment")))
        .map_err(|_| "set path".to_string())?;
    {
        let out = req.body().map_err(|_| "body".to_string())?;
        let _ = OutgoingBody::finish(out, None);
    }
    let future = outgoing_handler::handle(req, Some(RequestOptions::new()))
        .map_err(|e| format!("golem unreachable: {e:?}"))?;
    future.subscribe().block();
    let resp = future
        .get()
        .ok_or_else(|| "no response".to_string())?
        .map_err(|_| "response taken".to_string())?
        .map_err(|e| format!("http: {e:?}"))?;
    if !(200..300).contains(&resp.status()) {
        return Err(format!("golem status {}", resp.status()));
    }
    let mut bytes = Vec::new();
    if let Ok(incoming) = resp.consume() {
        if let Ok(stream) = incoming.stream() {
            loop {
                match stream.blocking_read(8192) {
                    Ok(c) if c.is_empty() => break,
                    Ok(c) => bytes.extend_from_slice(&c),
                    Err(bindings::wasi::io::streams::StreamError::Closed) => break,
                    // A failed read is not the end of the reply; a truncated one
                    // parses into a plausible and wrong result.
                    Err(_) => {
                        bytes.clear();
                        break;
                    }
                }
            }
        }
    }
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

/// Mark a leg failed and move the saga into `compensating`.
fn begin_compensation(id: &str, data: &mut Value, i: usize, leg: &str) {
    set_leg(data, i, "failed", "");
    publish("saga.leg.failed", &json!({"saga": id, "leg": leg}));
    set_status(data, fire(id, "fail").unwrap_or_else(|| "compensating".into()));
    save(id, data);
}

/// Cancel a leg's booking, fenced so it happens at most once.
fn compensate_leg(saga: &str, leg: &str) {
    let key = format!("saga:{saga}:comp:{leg}");
    if let Ok(Some(_)) = idem::begin(&key, 3600) {
        return; // already compensated
    }
    let bookings = records::find_by(BOOKINGS, "saga", &json!(saga).to_string()).unwrap_or_default();
    for b in bookings {
        let d: Value = serde_json::from_str(&b.data).unwrap_or(Value::Null);
        if d["leg"].as_str() == Some(leg) {
            let _ = records::delete(BOOKINGS, &b.id);
        }
    }
    let _ = idem::complete(&key, 200, b"compensated");
}

// ---- saga-record helpers -----------------------------------------------------

/// First leg still needing work: `pending` (not yet tried) or `retrying` (a
/// transient failure armed a retry timer).
fn next_active(data: &Value) -> Option<usize> {
    let steps = data["steps"].as_array()?;
    steps.iter().position(|s| matches!(s["state"].as_str(), Some("pending") | Some("retrying")))
}

/// Highest-index leg still in `booked` (compensate in reverse order).
fn last_booked(data: &Value) -> Option<usize> {
    let steps = data["steps"].as_array()?;
    (0..steps.len()).rev().find(|&i| steps[i]["state"].as_str() == Some("booked"))
}

fn leg_name(data: &Value, i: usize) -> String {
    data["steps"][i]["leg"].as_str().unwrap_or("").to_string()
}

fn set_leg(data: &mut Value, i: usize, state: &str, reference: &str) {
    data["steps"][i]["state"] = json!(state);
    data["steps"][i]["at"] = json!(now());
    if !reference.is_empty() {
        data["steps"][i]["ref"] = json!(reference);
    }
}

fn set_status(data: &mut Value, state: String) {
    data["status"] = json!(state);
}

/// Persist the saga record. revision 0 = last-write-wins.
// ponytail: single-writer per request/pump tick, so LWW is safe; switch to
// optimistic revisions if sagas are ever advanced concurrently.
fn save(id: &str, data: &Value) {
    let _ = records::update(SAGAS, id, &data.to_string(), 0);
}

/// Fire an fsm transition; returns the new state (None if illegal/unavailable).
fn fire(id: &str, event: &str) -> Option<String> {
    fsm::fire(MACHINE, id, event).ok().map(|s| s.state)
}

fn saga_json(id: &str) -> String {
    let entry = match records::get(SAGAS, id) {
        Ok(e) => e,
        Err(_) => return json!({"error": "not_found"}).to_string(),
    };
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    let history: Vec<Value> = fsm::history(MACHINE, id)
        .unwrap_or_default()
        .iter()
        .map(|h| json!({"event": h.event, "from": h.source, "to": h.target, "at": h.at}))
        .collect();
    json!({
        "id": entry.id,
        "traveler": data["traveler"],
        "status": data["status"],
        "startedAt": data["startedAt"],
        "steps": data["steps"],
        "history": history,
        "lastError": data["lastError"],
    })
    .to_string()
}

fn publish(topic: &str, payload: &Value) {
    let _ = bus::publish(topic, payload.to_string().as_bytes());
}

// ---- http plumbing -----------------------------------------------------------

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::Err(404, "not_found".into()),
        records::StoreError::InvalidJson(m) => Outcome::Err(422, m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn parse_body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let body = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if body.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(&body).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
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
    match result {
        Outcome::Json(code, body) => respond(response_out, code, body.as_bytes()),
        Outcome::Err(code, msg) => {
            respond(response_out, code, json!({ "error": msg }).to_string().as_bytes())
        }
    }
}

fn respond(response_out: ResponseOutparam, status: u16, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
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
