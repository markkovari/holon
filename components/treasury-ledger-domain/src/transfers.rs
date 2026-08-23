//! `transfers` — see CONTRACT.md.
//!
//! The one hard part: the debit is a compare-and-swap on the source account's revision, not a
//! read-then-write. Of two callers who both saw enough money, exactly one commits; the other
//! sees `RevisionConflict`, re-reads, and this time the comparison refuses it for real. The
//! credit side cannot fail on its merits, so it retries until it lands — and if it truly can't,
//! the money goes back on the source rather than vanishing.

use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::{AuthError, Permission};
use crate::bindings::fsm::workflow::engine as fsm;
use crate::bindings::idempotency::guard::store as idem;
use crate::bindings::ledger::doubleentry::ledger as doubleentry;
use crate::bindings::money::amount::arithmetic as money;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{now_secs, rfc3339, Reply, Route};
use serde_json::{json, Value};

// Bounded retry count for a CAS loop under contention. Twelve concurrent full-balance
// transfers means up to eleven conflicts to burn through before the twelfth attempt is the
// only one still eligible.
const MAX_ATTEMPTS: u32 = 20;

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "transfers"]) => create_transfer(route, body),
        (Method::Get, ["api", "transfers", id]) => get_transfer(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

fn require_auth(route: &Route, target: &str, action: &str) -> Result<(), Reply> {
    if route.bearer.is_empty() {
        return Err(Reply::err(401, "unauthenticated"));
    }
    let required = Permission { target: target.to_string(), action: action.to_string() };
    match authz::authorize(&route.bearer, &required) {
        Ok(_principal) => Ok(()),
        Err(AuthError::InsufficientScope(_)) => Err(Reply::err(403, "forbidden")),
        Err(AuthError::BackendUnavailable(_)) | Err(AuthError::Internal(_)) => {
            Err(Reply::err(503, "auth_unavailable"))
        }
        Err(_) => Err(Reply::err(401, "unauthenticated")),
    }
}

fn get_transfer(route: &Route, id: &str) -> Reply {
    if let Err(r) = require_auth(route, "transfers", "read") {
        return r;
    }
    match records::get("transfers", id) {
        Ok(e) => {
            let mut v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
            if let Some(o) = v.as_object_mut() {
                o.insert("id".into(), json!(e.id));
            }
            Reply::json(200, v)
        }
        Err(_) => Reply::err(404, "not_found"),
    }
}

/// The idempotency envelope: reserve the key, run the transfer exactly once, cache whatever
/// answer comes out (success or refusal alike) so a repeat of the same key replays it verbatim.
fn create_transfer(route: &Route, body: &str) -> Reply {
    if let Err(r) = require_auth(route, "transfers", "write") {
        return r;
    }
    let key = route.idempotency_key.clone();
    if key.is_empty() {
        return Reply::err(400, "idempotency_key_required");
    }

    match idem::begin(&key, 30) {
        Ok(Some(cached)) => {
            let v: Value = serde_json::from_slice(&cached.body).unwrap_or(Value::Null);
            return Reply::json(cached.status, v);
        }
        Ok(None) => {}
        Err(_) => return Reply::err(409, "in_progress"),
    }

    let reply = run_transfer(route, body);
    let _ = idem::complete(&key, reply.status, reply.json.to_string().as_bytes());
    reply
}

fn transfer_doc(
    from: &str,
    to: &str,
    units: i64,
    currency: &str,
    state: &str,
    key: &str,
    created_at: &str,
    transfer_id: &str,
) -> Value {
    let mut doc = json!({
        "from": from, "to": to, "units": units, "currency": currency,
        "state": state, "key": key, "created_at": created_at,
    });
    if state == "settled" {
        doc["journal"] = json!({
            "id": transfer_id,
            "lines": [
                { "account": from, "amount": units, "side": "debit" },
                { "account": to, "amount": units, "side": "credit" },
            ],
        });
    }
    doc
}

fn run_transfer(route: &Route, body: &str) -> Reply {
    let req: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "bad_money"),
    };
    let from = req.get("from").and_then(Value::as_str).unwrap_or("").to_string();
    let to = req.get("to").and_then(Value::as_str).unwrap_or("").to_string();
    let amount_str = req.get("amount").and_then(Value::as_str).unwrap_or("");

    if from == to {
        return Reply::err(400, "same_account");
    }

    let from_entry = match records::get("accounts", &from) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let to_entry = match records::get("accounts", &to) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let from_doc: Value = serde_json::from_str(&from_entry.data).unwrap_or(json!({}));
    let to_doc: Value = serde_json::from_str(&to_entry.data).unwrap_or(json!({}));
    let currency = from_doc.get("currency").and_then(Value::as_str).unwrap_or("").to_string();
    let to_currency = to_doc.get("currency").and_then(Value::as_str).unwrap_or("").to_string();
    if currency.is_empty() || currency != to_currency {
        return Reply::err(400, "currency_mismatch");
    }
    let moving = match money::parse(amount_str, &currency) {
        Ok(a) => a,
        Err(_) => return Reply::err(400, "bad_money"),
    };

    let created_at = rfc3339(now_secs());
    let pending = transfer_doc(
        &from,
        &to,
        moving.units,
        &currency,
        "pending",
        &route.idempotency_key,
        &created_at,
        "",
    );
    let transfer_entry =
        match records::create("transfers", &pending.to_string(), &["state".to_string()]) {
            Ok(e) => e,
            Err(_) => return Reply::err(503, "contended"),
        };
    let transfer_id = transfer_entry.id.clone();

    let _ = fsm::define(
        "transfer",
        &fsm::Definition {
            states: vec![
                "pending".into(),
                "settled".into(),
                "refused".into(),
                "compensated".into(),
            ],
            initial: "pending".into(),
            transitions: vec![
                fsm::Transition {
                    event: "settle".into(),
                    source: "pending".into(),
                    target: "settled".into(),
                },
                fsm::Transition {
                    event: "refuse".into(),
                    source: "pending".into(),
                    target: "refused".into(),
                },
                fsm::Transition {
                    event: "compensate".into(),
                    source: "settled".into(),
                    target: "compensated".into(),
                },
            ],
            terminal: vec!["refused".into(), "compensated".into()],
        },
    );
    let _ = fsm::create_instance("transfer", &transfer_id);

    match debit(&from, moving.units, &currency) {
        DebitOutcome::InsufficientFunds => {
            let _ = fsm::fire("transfer", &transfer_id, "refuse");
            let refused = transfer_doc(
                &from,
                &to,
                moving.units,
                &currency,
                "refused",
                &route.idempotency_key,
                &created_at,
                &transfer_id,
            );
            let _ = records::update(
                "transfers",
                &transfer_id,
                &refused.to_string(),
                transfer_entry.revision,
            );
            Reply::err(409, "insufficient_funds")
        }
        DebitOutcome::Failed => Reply::err(503, "contended"),
        DebitOutcome::Ok(from_units) => match credit(&to, moving.units, &currency) {
            Some(to_units) => {
                let entry = doubleentry::Entry {
                    id: transfer_id.clone(),
                    memo: "transfer".into(),
                    lines: vec![
                        doubleentry::Line {
                            account: from.clone(),
                            amount: moving.units,
                            side: doubleentry::Side::Debit,
                        },
                        doubleentry::Line {
                            account: to.clone(),
                            amount: moving.units,
                            side: doubleentry::Side::Credit,
                        },
                    ],
                };
                if doubleentry::validate(&entry).is_err() {
                    return Reply::err(500, "journal_lost");
                }
                let journal = json!({
                    "transfer": transfer_id, "from": from, "to": to, "units": moving.units,
                    "at": rfc3339(now_secs()),
                });
                if records::create(
                    "journal",
                    &journal.to_string(),
                    &["from".to_string(), "to".to_string()],
                )
                .is_err()
                {
                    return Reply::err(500, "journal_lost");
                }
                let _ = fsm::fire("transfer", &transfer_id, "settle");
                let settled = transfer_doc(
                    &from,
                    &to,
                    moving.units,
                    &currency,
                    "settled",
                    &route.idempotency_key,
                    &created_at,
                    &transfer_id,
                );
                let _ = records::update(
                    "transfers",
                    &transfer_id,
                    &settled.to_string(),
                    transfer_entry.revision,
                );
                Reply::json(
                    201,
                    json!({ "transfer": transfer_id, "from_units": from_units, "to_units": to_units }),
                )
            }
            None => {
                // The debit already committed; the credit could not land after every retry.
                // Put the money back on the source rather than lose it, then refuse.
                let _ = credit(&from, moving.units, &currency);
                let _ = fsm::fire("transfer", &transfer_id, "refuse");
                let refused = transfer_doc(
                    &from,
                    &to,
                    moving.units,
                    &currency,
                    "refused",
                    &route.idempotency_key,
                    &created_at,
                    &transfer_id,
                );
                let _ = records::update(
                    "transfers",
                    &transfer_id,
                    &refused.to_string(),
                    transfer_entry.revision,
                );
                Reply::err(503, "contended")
            }
        },
    }
}

enum DebitOutcome {
    Ok(i64),
    InsufficientFunds,
    Failed,
}

/// The serialisation point: compare-and-swap on the source account's revision. A conflict is
/// "read again", not a failure — retry with a fresh read and let the comparison happen against
/// what the store actually holds now.
fn debit(account_id: &str, units: i64, currency: &str) -> DebitOutcome {
    for _ in 0..MAX_ATTEMPTS {
        let entry = match records::get("accounts", account_id) {
            Ok(e) => e,
            Err(_) => return DebitOutcome::Failed,
        };
        let mut doc: Value = match serde_json::from_str(&entry.data) {
            Ok(v) => v,
            Err(_) => return DebitOutcome::Failed,
        };
        let balance = doc.get("units").and_then(Value::as_i64).unwrap_or(0);
        let current = money::Amount { units: balance, currency: currency.to_string() };
        let moving = money::Amount { units, currency: currency.to_string() };
        match money::compare(&current, &moving) {
            Ok(cmp) if cmp < 0 => return DebitOutcome::InsufficientFunds,
            Ok(_) => {}
            Err(_) => return DebitOutcome::Failed,
        }
        let updated = match money::subtract(&current, &moving) {
            Ok(a) => a,
            Err(_) => return DebitOutcome::Failed,
        };
        if let Some(o) = doc.as_object_mut() {
            o.insert("units".into(), json!(updated.units));
        }
        match records::update("accounts", account_id, &doc.to_string(), entry.revision) {
            Ok(_) => return DebitOutcome::Ok(updated.units),
            Err(records::StoreError::RevisionConflict(_)) => continue,
            Err(_) => return DebitOutcome::Failed,
        }
    }
    DebitOutcome::Failed
}

/// A credit cannot fail on its merits — only on contention — so it retries until it lands.
fn credit(account_id: &str, units: i64, currency: &str) -> Option<i64> {
    for _ in 0..MAX_ATTEMPTS {
        let entry = match records::get("accounts", account_id) {
            Ok(e) => e,
            Err(_) => return None,
        };
        let mut doc: Value = match serde_json::from_str(&entry.data) {
            Ok(v) => v,
            Err(_) => return None,
        };
        let balance = doc.get("units").and_then(Value::as_i64).unwrap_or(0);
        let current = money::Amount { units: balance, currency: currency.to_string() };
        let moving = money::Amount { units, currency: currency.to_string() };
        let updated = match money::add(&current, &moving) {
            Ok(a) => a,
            Err(_) => return None,
        };
        if let Some(o) = doc.as_object_mut() {
            o.insert("units".into(), json!(updated.units));
        }
        match records::update("accounts", account_id, &doc.to_string(), entry.revision) {
            Ok(_) => return Some(updated.units),
            Err(records::StoreError::RevisionConflict(_)) => continue,
            Err(_) => return None,
        }
    }
    None
}
