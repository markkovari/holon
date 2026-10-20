//! saga:app — a durable trip-booking saga over composed contracts.
//! book/compensate is fenced by `idempotency:guard`, so re-runs never double-book or double-refund.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../fsm-workflow/wit",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../idempotency-guard/wit",
            "../event-bus/wit",
            "../id-generate/wit",
            "../outbox/wit",
            "../scheduler-timer/wit",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "wit",
        ],
        world: "saga:app/saga-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
    }
}

use serde_json::{json, Value};

use bindings::event::bus::bus;
use bindings::fsm::workflow::engine as fsm;
use bindings::id::generate::generator as ids;
use bindings::idempotency::guard::store as idem;
use bindings::records::store::store as records;
use bindings::sched::timer::timer;
use bindings::p3::clocks::system_clock;

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};

struct Component;

const SAGAS: &str = "sagas";
const BOOKINGS: &str = "bookings";
const MACHINE: &str = "saga";
const LEGS: [&str; 3] = ["flight", "hotel", "car"];
// ponytail: a const, not the `max-attempts` config knob, to keep rung 3 dependency-free.
const MAX_ATTEMPTS: u64 = 3;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let result = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage_json(),
            (Method::Post, ["trips"]) => create_trip(request).await,
            (Method::Get, ["trips", id]) => get_trip(id),
            (Method::Post, ["trips", id, "run"]) => run_trip(id).await,
            (Method::Post, ["internal", "pump"]) => pump().await,
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(result)
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
}

fn now() -> u64 {
    system_clock::now().seconds as u64
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

async fn create_trip(request: Request) -> Outcome {
    ensure_seeded();
    let body = match parse_body(request).await {
        Ok(v) => v,
        Err(o) => return o,
    };
    let traveler = body["traveler"].as_str().unwrap_or("").trim().to_string();
    if traveler.is_empty() {
        return Outcome::Err(422, "traveler required".into());
    }
    let fail_leg = body["failLeg"].as_str().filter(|l| LEGS.contains(l)).unwrap_or("");
    let flaky_leg = body["flakyLeg"].as_str().filter(|l| LEGS.contains(l)).unwrap_or("");
    let flaky_fails = body["flakyFails"].as_u64().unwrap_or(1);
    let steps: Vec<Value> = LEGS
        .iter()
        .map(|leg| json!({"leg": leg, "state": "pending", "ref": "", "price": price(leg), "at": 0, "attempts": 0}))
        .collect();
    // `golemHost` is the gateway `Host` header for subdomain routing.
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

async fn run_trip(id: &str) -> Outcome {
    if records::get(SAGAS, id).is_err() {
        return Outcome::Err(404, "not_found".into());
    }
    // Legs + commit + up to 3 compensations + finalize: 20 is a safe ceiling.
    for _ in 0..20 {
        if !step(id).await {
            break;
        }
    }
    Outcome::Json(200, saga_json(id))
}

/// The resume/retry entry point after a host restart: advances every live saga ONE step.
async fn pump() -> Outcome {
    // Due timers make a `retrying` leg eligible again; ack so a one-shot timer doesn't re-fire.
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
            if step(&e.id).await {
                advanced += 1;
            }
        }
    }
    Outcome::Json(200, json!({ "advanced": advanced }).to_string())
}

/// Terminal sagas return false.
async fn step(id: &str) -> bool {
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
                    set_status(&mut data, fire(id, "commit").unwrap_or_else(|| "committed".into()));
                    save(id, &data);
                    publish("saga.committed", &json!({"saga": id}));
                }
                Some(i) => {
                    let leg = leg_name(&data, i);
                    let attempts = data["steps"][i]["attempts"].as_u64().unwrap_or(0);
                    let timer_key = format!("saga:{id}:{leg}");
                    if fail_leg == leg {
                        begin_compensation(id, &mut data, i, &leg);
                    } else if flaky_leg == leg && attempts < flaky_fails {
                        if attempts + 1 < MAX_ATTEMPTS {
                            data["steps"][i]["attempts"] = json!(attempts + 1);
                            data["steps"][i]["state"] = json!("retrying");
                            let _ = timer::schedule_at(&timer_key, now(), b""); // backoff 0 = eligible now
                            save(id, &data);
                            publish(
                                "saga.leg.retry",
                                &json!({"saga": id, "leg": leg, "attempt": attempts + 1}),
                            );
                        } else {
                            let _ = timer::cancel(&timer_key);
                            begin_compensation(id, &mut data, i, &leg);
                        }
                    } else {
                        let _ = timer::cancel(&timer_key);
                        match book_leg(
                            id,
                            &leg,
                            golem.as_ref().map(|(u, h)| (u.as_str(), h.as_str())),
                        )
                        .await
                        {
                            Ok(reference) => {
                                set_leg(&mut data, i, "booked", &reference);
                                save(id, &data);
                                publish(
                                    "saga.leg.booked",
                                    &json!({"saga": id, "leg": leg, "ref": reference}),
                                );
                            }
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
        _ => false,
    }
}

/// Fenced so the booking record is created at most once; book/fail/retry is the caller's call.
async fn book_leg(saga: &str, leg: &str, golem: Option<(&str, &str)>) -> Result<String, String> {
    let key = format!("saga:{saga}:book:{leg}");
    if let Ok(Some(cached)) = idem::begin(&key, 3600) {
        return Ok(String::from_utf8_lossy(&cached.body).into_owned());
    }
    // Single-writer per saga, so in-progress can't race here.
    let reference = match golem {
        Some((url, host)) => match golem_book(url, host, &format!("{leg}-{saga}")).await {
            Ok(result) => format!("{}-golem-{result}", ref_prefix(leg)),
            Err(e) => {
                let _ = idem::forget(&key); // release the key so a retry can re-attempt
                return Err(e);
            }
        },
        None => format!("{}-{}", ref_prefix(leg), ids::short_code(6)),
    };
    let booking = json!({"saga": saga, "leg": leg, "ref": reference, "price": price(leg)});
    let _ = records::create(BOOKINGS, &booking.to_string(), &["saga".to_string()]);
    let _ = idem::complete(&key, 200, reference.as_bytes());
    Ok(reference)
}

fn golem_opts(data: &Value) -> Option<(String, String)> {
    let url = data["golemUrl"].as_str().unwrap_or("");
    if url.is_empty() {
        return None;
    }
    Some((url.to_string(), data["golemHost"].as_str().unwrap_or("").to_string()))
}

async fn golem_book(url: &str, host: &str, workflow: &str) -> Result<String, String> {
    let (scheme, url_authority) = if let Some(rest) = url.strip_prefix("https://") {
        ("https", rest.trim_end_matches('/').to_string())
    } else {
        ("http", url.trim_start_matches("http://").trim_end_matches('/').to_string())
    };
    // wasi:http derives `Host` from the authority (a manual `host` is ignored) and Golem's gateway
    // routes by subdomain Host, so the authority MUST be the gateway host.
    let authority = if host.is_empty() { url_authority } else { host.to_string() };
    let target = format!("{scheme}://{authority}/counters/{workflow}/increment");
    // A truncated reply parses into a plausible, wrong result, so `fetch` reports it as an error.
    let (status, bytes) = fetch(
        Method::Post,
        &target,
        &[("content-type", "application/json")],
        Vec::new(),
    )
    .await
    .map_err(|e| format!("golem unreachable: {e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("golem status {status}"));
    }
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

fn begin_compensation(id: &str, data: &mut Value, i: usize, leg: &str) {
    set_leg(data, i, "failed", "");
    publish("saga.leg.failed", &json!({"saga": id, "leg": leg}));
    set_status(data, fire(id, "fail").unwrap_or_else(|| "compensating".into()));
    save(id, data);
}

fn compensate_leg(saga: &str, leg: &str) {
    let key = format!("saga:{saga}:comp:{leg}");
    if let Ok(Some(_)) = idem::begin(&key, 3600) {
        return;
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

fn next_active(data: &Value) -> Option<usize> {
    let steps = data["steps"].as_array()?;
    steps.iter().position(|s| matches!(s["state"].as_str(), Some("pending") | Some("retrying")))
}

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

// ponytail: revision 0 (last-write-wins) is safe while single-writer; switch to optimistic
// revisions if sagas are ever advanced concurrently.
fn save(id: &str, data: &Value) {
    let _ = records::update(SAGAS, id, &data.to_string(), 0);
}

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

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::Err(404, "not_found".into()),
        records::StoreError::InvalidJson(m) => Outcome::Err(422, m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

async fn parse_body(request: Request) -> Result<Value, Outcome> {
    let body = read_body(request).await.map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if body.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(&body).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

/// Without a ceiling a read grew to wasmtime's 64 MiB cap and TRAPPED (a closed connection).
/// A backstop, not a content policy: uploads 413 on their own.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body!(MAX_BODY_BYTES);
guestio::guest_p3_respond!();
guestio::guest_p3_fetch!(MAX_BODY_BYTES);

fn emit(result: Outcome) -> Result<Response, ErrorCode> {
    match result {
        Outcome::Json(code, body) => respond(code, "application/json", body),
        Outcome::Err(code, msg) => {
            respond(code, "application/json", json!({ "error": msg }).to_string())
        }
    }
}

bindings::export!(Component with_types_in bindings);
