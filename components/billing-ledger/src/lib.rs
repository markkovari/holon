//! ledger:app — billing ledger over composed capability contracts.
//!
//! Write path (`POST /api/accounts/{id}/entries`): idempotency-key replay
//! cache -> per-account quota -> money parse -> entry record -> balance CAS
//! (revision-guarded, bounded retry) -> outbox feed -> cache the 201. A retry
//! with the same key gets the cached response byte-for-byte, so a client can
//! crash mid-request and resend without double-charging.

#[allow(warnings)]
mod bindings;

use serde::Deserialize;
use serde_json::{json, Value};

use bindings::csv::codec::codec as csv;
use bindings::idempotency::guard::store as idem;
use bindings::money::amount::arithmetic as money;
use bindings::outbox::dispatch::queue as outbox;
use bindings::quota::meter::meter as quota;
use bindings::records::store::store as records;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const ACCOUNTS: &str = "ledger_accounts";
const ENTRIES: &str = "ledger_entries";
/// entries per account per window.
const ENTRY_LIMIT: u64 = 1000;
const QUOTA_PERIOD: u64 = 3600;
/// how long a replayed idempotency-key returns the cached response.
const IDEM_TTL: u64 = 86400;
const CAS_RETRIES: u32 = 3;

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
                    "service": "billing-ledger",
                    "accounts": "POST /api/accounts {name, currency}",
                    "entries": "POST /api/accounts/{id}/entries {kind: charge|credit, amount, description?} + idempotency-key header",
                    "statement": "GET /api/accounts/{id}/statement.csv",
                    "allocate": "GET /api/allocate?amount=100.00&currency=USD&shares=3",
                    "feed": "POST /api/events/drain"
                })
                .to_string(),
            ),
            (Method::Post, ["api", "accounts"]) => create_account(&request),
            (Method::Get, ["api", "accounts"]) => list_accounts(),
            (Method::Get, ["api", "accounts", id]) => get_account(id),
            (Method::Post, ["api", "accounts", id, "entries"]) => post_entry(&request, id),
            (Method::Get, ["api", "accounts", id, "entries"]) => list_entries(id),
            (Method::Get, ["api", "accounts", id, "statement.csv"]) => statement(id),
            (Method::Get, ["api", "allocate"]) => allocate(&query),
            (Method::Post, ["api", "events", "drain"]) => drain_events(),
            _ => Outcome::NotFound,
        };
        emit(response_out, result);
    }
}

enum Outcome {
    Json(u16, String),
    /// replayed idempotent response — served with `idempotent-replay: true`.
    Cached(u16, String),
    Csv(String),
    /// 429 with a Retry-After until the payload epoch-seconds.
    Limited(u64),
    Bad(String),
    Err(u16, String),
    NotFound,
}

// ---- accounts --------------------------------------------------------------

#[derive(Deserialize)]
struct CreateAccount {
    name: String,
    currency: String,
}

fn create_account(request: &IncomingRequest) -> Outcome {
    let req: CreateAccount =
        match read_body(request).and_then(|b| serde_json::from_slice(&b).map_err(|_| ())) {
            Ok(r) => r,
            Err(_) => return Outcome::Bad("expected json body {name, currency}".into()),
        };
    // money:amount owns the currency table — parsing zero validates the code.
    // parse demands exactly the currency's exponent digits, which we don't
    // know yet, so try the exponents in circulation (0, 2, 3).
    if !["0", "0.00", "0.000"].iter().any(|z| money::parse(z, &req.currency).is_ok()) {
        return Outcome::Bad(format!("unknown currency: {}", req.currency));
    }
    let data = json!({ "name": req.name, "currency": req.currency, "units": 0 });
    match records::create(ACCOUNTS, &data.to_string(), &[]) {
        Ok(e) => Outcome::Json(201, account_json(&e).to_string()),
        Err(e) => store_err(e),
    }
}

fn get_account(id: &str) -> Outcome {
    match records::get(ACCOUNTS, id) {
        Ok(e) => Outcome::Json(200, account_json(&e).to_string()),
        Err(records::StoreError::NotFound) => Outcome::NotFound,
        Err(e) => store_err(e),
    }
}

fn list_accounts() -> Outcome {
    match records::list_records(ACCOUNTS, 0, "") {
        Ok(page) => {
            let accounts: Vec<Value> = page.entries.iter().map(account_json).collect();
            Outcome::Json(200, json!({ "accounts": accounts }).to_string())
        }
        Err(e) => store_err(e),
    }
}

fn account_json(entry: &records::Entry) -> Value {
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    let currency = data["currency"].as_str().unwrap_or("").to_string();
    let units = data["units"].as_i64().unwrap_or(0);
    json!({
        "id": entry.id,
        "name": data["name"],
        "currency": currency,
        "balance": fmt(units, &currency),
        "created": entry.created,
    })
}

fn fmt(units: i64, currency: &str) -> String {
    money::format(&money::Amount { units, currency: currency.to_string() })
        .unwrap_or_else(|_| units.to_string())
}

// ---- entries ---------------------------------------------------------------

#[derive(Deserialize)]
struct PostEntry {
    kind: String,
    amount: String,
    #[serde(default)]
    description: Option<String>,
}

fn post_entry(request: &IncomingRequest, account_id: &str) -> Outcome {
    let Some(key) = header(request, "idempotency-key").filter(|k| !k.is_empty()) else {
        return Outcome::Bad("missing idempotency-key header".into());
    };
    let idem_key = format!("ledger:{account_id}:{key}");
    match idem::begin(&idem_key, IDEM_TTL) {
        // seen before: replay the original response verbatim.
        Ok(Some(cached)) => {
            return Outcome::Cached(cached.status, String::from_utf8_lossy(&cached.body).into_owned())
        }
        Ok(None) => {}
        Err(idem::IdemError::InProgress) => {
            return Outcome::Err(409, "request with this idempotency-key is in flight".into())
        }
        Err(idem::IdemError::BackendUnavailable(m)) => return Outcome::Err(503, m),
    }

    // only a successful post is cached; any error forgets the reservation so
    // the client can retry with the same key after fixing the request.
    let outcome = post_entry_inner(request, account_id);
    match &outcome {
        Outcome::Json(status, body) => {
            let _ = idem::complete(&idem_key, *status, body.as_bytes());
        }
        _ => {
            let _ = idem::forget(&idem_key);
        }
    }
    outcome
}

fn post_entry_inner(request: &IncomingRequest, account_id: &str) -> Outcome {
    match quota::reserve(&format!("ledger:{account_id}"), 1, ENTRY_LIMIT, QUOTA_PERIOD) {
        Ok(_) => {}
        Err(quota::QuotaError::Exceeded(_)) => {
            let resets = quota::peek(&format!("ledger:{account_id}"), ENTRY_LIMIT, QUOTA_PERIOD)
                .map(|b| b.resets_at)
                .unwrap_or(0);
            return Outcome::Limited(resets);
        }
        Err(quota::QuotaError::BackendUnavailable(m)) => return Outcome::Err(503, m),
    }

    let req: PostEntry =
        match read_body(request).and_then(|b| serde_json::from_slice(&b).map_err(|_| ())) {
            Ok(r) => r,
            Err(_) => return Outcome::Bad("expected json body {kind, amount, description?}".into()),
        };
    if req.kind != "charge" && req.kind != "credit" {
        return Outcome::Bad("kind must be charge or credit".into());
    }

    let account = match records::get(ACCOUNTS, account_id) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Outcome::NotFound,
        Err(e) => return store_err(e),
    };
    let currency = serde_json::from_str::<Value>(&account.data)
        .ok()
        .and_then(|d| d["currency"].as_str().map(str::to_string))
        .unwrap_or_default();
    let amt = match money::parse(&req.amount, &currency) {
        Ok(a) => a,
        // money reports malformed decimals as unknown-currency; translate.
        Err(_) => {
            return Outcome::Bad(format!(
                "bad amount: expected a {currency} decimal with its exact minor digits (e.g. 1.50)"
            ))
        }
    };
    if amt.units <= 0 {
        return Outcome::Bad("amount must be positive".into());
    }

    // entry first, then balance CAS; a failed CAS compensates by deleting the
    // entry. ponytail: no cross-key transaction in wasi:keyvalue — if the
    // compensation itself fails the entry survives without a balance effect;
    // an outbox-driven reconciler is the upgrade path.
    let entry_data = json!({
        "account": account_id,
        "kind": req.kind,
        "units": amt.units,
        "currency": currency,
        "description": req.description.unwrap_or_default(),
    });
    let entry = match records::create(ENTRIES, &entry_data.to_string(), &["account".to_string()]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };

    let balance = match apply_balance(account_id, &amt, req.kind == "charge") {
        Ok(units) => units,
        Err(o) => {
            let _ = records::delete(ENTRIES, &entry.id);
            return o;
        }
    };

    // durable feed for the external biller; a lost enqueue is visible by the
    // entry existing without a feed event, not worth failing the write over.
    let _ = outbox::enqueue(
        "entry.recorded",
        json!({
            "entry": entry.id,
            "account": account_id,
            "kind": req.kind,
            "amount": fmt(amt.units, &currency),
            "currency": currency,
        })
        .to_string()
        .as_bytes(),
        0,
    );

    Outcome::Json(
        201,
        json!({
            "entry": entry_json(&entry),
            "balance": fmt(balance, &currency),
        })
        .to_string(),
    )
}

/// Revision-guarded read-modify-write on the account balance. The CAS loop
/// absorbs concurrent posts; money:amount does the arithmetic (overflow-safe).
fn apply_balance(account_id: &str, amt: &money::Amount, charge: bool) -> Result<i64, Outcome> {
    for _ in 0..CAS_RETRIES {
        let account = match records::get(ACCOUNTS, account_id) {
            Ok(e) => e,
            Err(e) => return Err(store_err(e)),
        };
        let mut data: Value = serde_json::from_str(&account.data).unwrap_or(Value::Null);
        let balance = money::Amount {
            units: data["units"].as_i64().unwrap_or(0),
            currency: amt.currency.clone(),
        };
        let updated = if charge {
            money::add(&balance, amt)
        } else {
            money::subtract(&balance, amt)
        };
        let updated = match updated {
            Ok(a) => a,
            Err(e) => return Err(Outcome::Bad(format!("balance: {e:?}"))),
        };
        data["units"] = json!(updated.units);
        match records::update(ACCOUNTS, account_id, &data.to_string(), account.revision) {
            Ok(_) => return Ok(updated.units),
            Err(records::StoreError::RevisionConflict(_)) => continue,
            Err(e) => return Err(store_err(e)),
        }
    }
    Err(Outcome::Err(409, "balance update contention, retry".into()))
}

fn entry_json(entry: &records::Entry) -> Value {
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    let currency = data["currency"].as_str().unwrap_or("").to_string();
    json!({
        "id": entry.id,
        "account": data["account"],
        "kind": data["kind"],
        "amount": fmt(data["units"].as_i64().unwrap_or(0), &currency),
        "currency": currency,
        "description": data["description"],
        "created": entry.created,
    })
}

fn account_entries(account_id: &str) -> Result<Vec<records::Entry>, Outcome> {
    records::find_by(ENTRIES, "account", &json!(account_id).to_string()).map_err(store_err)
}

fn list_entries(account_id: &str) -> Outcome {
    match account_entries(account_id) {
        Ok(entries) => {
            let list: Vec<Value> = entries.iter().map(entry_json).collect();
            Outcome::Json(200, json!({ "entries": list }).to_string())
        }
        Err(o) => o,
    }
}

// ---- statement + allocate ----------------------------------------------------

fn statement(account_id: &str) -> Outcome {
    if let Err(records::StoreError::NotFound) = records::get(ACCOUNTS, account_id) {
        return Outcome::NotFound;
    }
    let entries = match account_entries(account_id) {
        Ok(e) => e,
        Err(o) => return o,
    };
    let mut rows = vec![csv::Row {
        fields: ["id", "kind", "amount", "currency", "description", "created"]
            .map(String::from)
            .to_vec(),
    }];
    for e in &entries {
        let d: Value = serde_json::from_str(&e.data).unwrap_or(Value::Null);
        let currency = d["currency"].as_str().unwrap_or("").to_string();
        rows.push(csv::Row {
            fields: vec![
                e.id.clone(),
                d["kind"].as_str().unwrap_or("").to_string(),
                fmt(d["units"].as_i64().unwrap_or(0), &currency),
                currency,
                d["description"].as_str().unwrap_or("").to_string(),
                e.created.to_string(),
            ],
        });
    }
    let dialect = csv::Dialect { delimiter: ",".to_string(), has_header: true, trim: false };
    Outcome::Csv(csv::format(&rows, &dialect))
}

/// Penny-exact split — money:allocate distributes the remainder so the parts
/// always sum to the total.
fn allocate(query: &str) -> Outcome {
    let amount = query_param(query, "amount").unwrap_or_default();
    let currency = query_param(query, "currency").unwrap_or_default();
    let shares: u32 = query_param(query, "shares").and_then(|s| s.parse().ok()).unwrap_or(0);
    if amount.is_empty() || currency.is_empty() || shares == 0 {
        return Outcome::Bad("expected ?amount=100.00&currency=USD&shares=3".into());
    }
    let total = match money::parse(&amount, &currency) {
        Ok(a) => a,
        Err(e) => return Outcome::Bad(format!("bad amount: {e:?}")),
    };
    match money::allocate(&total, shares) {
        Ok(parts) => {
            let list: Vec<String> = parts.iter().map(|p| fmt(p.units, &p.currency)).collect();
            Outcome::Json(
                200,
                json!({ "total": fmt(total.units, &currency), "shares": shares, "parts": list })
                    .to_string(),
            )
        }
        Err(e) => Outcome::Bad(format!("allocate: {e:?}")),
    }
}

// ---- feed --------------------------------------------------------------------

/// Claim-and-ack the entry feed — an at-least-once poll for an external
/// consumer (dedupe on event id downstream).
fn drain_events() -> Outcome {
    let events = match outbox::claim(50, 60) {
        Ok(evs) => evs,
        Err(e) => return Outcome::Err(503, format!("outbox: {e:?}")),
    };
    let list: Vec<Value> = events
        .iter()
        .map(|ev| {
            let _ = outbox::ack(&ev.id);
            json!({
                "id": ev.id,
                "topic": ev.topic,
                "data": serde_json::from_slice::<Value>(&ev.payload).unwrap_or(Value::Null),
                "created": ev.created,
            })
        })
        .collect();
    Outcome::Json(200, json!({ "events": list }).to_string())
}

// ---- helpers -----------------------------------------------------------------

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

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let body = request.consume().map_err(|_| ())?;
    let stream = body.stream().map_err(|_| ())?;
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

fn header(request: &IncomingRequest, name: &str) -> Option<String> {
    request
        .headers()
        .get(name)
        .into_iter()
        .find_map(|v| String::from_utf8(v).ok())
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let mut it = kv.splitn(2, '=');
        (it.next()? == key).then(|| it.next().unwrap_or("").to_string())
    })
}

// ---- responses -----------------------------------------------------------------

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond(response_out, code, &[], body.as_bytes(), "application/json"),
        Outcome::Cached(code, body) => respond(
            response_out,
            code,
            &[("idempotent-replay", "true")],
            body.as_bytes(),
            "application/json",
        ),
        Outcome::Csv(body) => respond(response_out, 200, &[], body.as_bytes(), "text/csv"),
        Outcome::Limited(resets_at) => respond(
            response_out,
            429,
            &[],
            format!("{{\"error\":\"quota_exceeded\",\"resetsAt\":{resets_at}}}").as_bytes(),
            "application/json",
        ),
        Outcome::Bad(msg) => respond(
            response_out,
            400,
            &[],
            json!({ "error": msg }).to_string().as_bytes(),
            "application/json",
        ),
        Outcome::Err(code, msg) => respond(
            response_out,
            code,
            &[],
            json!({ "error": msg }).to_string().as_bytes(),
            "application/json",
        ),
        Outcome::NotFound => {
            respond(response_out, 404, &[], b"{\"error\":\"not_found\"}", "application/json")
        }
    }
}

fn respond(
    response_out: ResponseOutparam,
    status: u16,
    extra: &[(&str, &str)],
    body: &[u8],
    content_type: &str,
) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[content_type.as_bytes().to_vec()]);
    for (k, v) in extra {
        let _ = headers.set(k.as_ref(), &[v.as_bytes().to_vec()]);
    }
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in body.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
