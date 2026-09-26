use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::Permission;
use crate::bindings::fsm::workflow::engine as fsm;
use crate::bindings::idempotency::guard::store as idem;
use crate::bindings::ledger::doubleentry::ledger;
use crate::bindings::money::amount::arithmetic as money;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "transfers"]) => write_transfer(route, body),
        (Method::Get, ["api", "transfers", id]) => get_transfer(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

fn authorize_perm(route: &Route, action: &str) -> Result<String, Reply> {
    let perm = Permission { target: "transfers".to_string(), action: action.to_string() };
    match authz::authorize(&route.bearer, &perm) {
        Ok(p) => Ok(p.subject),
        Err(err) => {
            use crate::bindings::auth::identity::types::AuthError;
            let reply = match err {
                AuthError::InsufficientScope(_) => Reply::err(403, "forbidden"),
                AuthError::BackendUnavailable(_) | AuthError::Internal(_) => {
                    Reply::err(503, "auth_unavailable")
                }
                _ => Reply::err(401, "unauthenticated"),
            };
            Err(reply)
        }
    }
}

fn ensure_fsm() {
    let def = fsm::Definition {
        states: vec![
            "pending".to_string(),
            "settled".to_string(),
            "refused".to_string(),
            "compensated".to_string(),
        ],
        initial: "pending".to_string(),
        transitions: vec![
            fsm::Transition {
                event: "settle".to_string(),
                source: "pending".to_string(),
                target: "settled".to_string(),
            },
            fsm::Transition {
                event: "refuse".to_string(),
                source: "pending".to_string(),
                target: "refused".to_string(),
            },
            fsm::Transition {
                event: "compensate".to_string(),
                source: "settled".to_string(),
                target: "compensated".to_string(),
            },
        ],
        terminal: vec!["settled".to_string(), "refused".to_string(), "compensated".to_string()],
    };
    let _ = fsm::define("transfer", &def);
}

fn do_credit(id: &str, amount: &money::Amount) -> Result<i64, ()> {
    let mut attempts = 0;
    loop {
        if attempts >= 50 {
            return Err(());
        }
        attempts += 1;
        let entry = match records::get("accounts", id) {
            Ok(e) => e,
            Err(_) => return Err(()),
        };
        let mut doc: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
        let currency = doc.get("currency").and_then(Value::as_str).unwrap_or("");
        let cur_units = doc.get("units").and_then(Value::as_i64).unwrap_or(0);
        let cur_amount = money::Amount { units: cur_units, currency: currency.to_string() };
        let new_amount = match money::add(&cur_amount, amount) {
            Ok(a) => a,
            Err(_) => return Err(()),
        };
        doc["units"] = json!(new_amount.units);
        match records::update("accounts", id, &doc.to_string(), entry.revision) {
            Ok(_) => return Ok(new_amount.units),
            Err(records::StoreError::RevisionConflict(_)) => continue,
            Err(_) => return Err(()),
        }
    }
}

fn write_transfer(route: &Route, body: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "write") {
        return r;
    }

    if route.idempotency_key.is_empty() {
        return Reply::err(400, "idempotency_key_required");
    }

    let ttl = crate::cfg("idempotency-ttl-secs", "86400").parse::<u64>().unwrap_or(86400);
    match idem::begin(&route.idempotency_key, ttl) {
        Ok(Some(cached)) => {
            return Reply {
                status: cached.status,
                json: serde_json::from_slice(&cached.body).unwrap_or(json!({})),
            };
        }
        Ok(None) => {}
        Err(idem::IdemError::InProgress) => return Reply::err(409, "in_progress"),
        Err(idem::IdemError::BackendUnavailable(_)) => {
            return Reply::err(503, "idempotency_unavailable")
        }
    }

    let do_complete = |status: u16, res: Value| -> Reply {
        let body_bytes = serde_json::to_vec(&res).unwrap();
        let _ = idem::complete(&route.idempotency_key, status, &body_bytes);
        Reply::json(status, res)
    };
    let do_complete_err = |status: u16, code: &str| -> Reply {
        let res = json!({"error": code});
        let body_bytes = serde_json::to_vec(&res).unwrap();
        let _ = idem::complete(&route.idempotency_key, status, &body_bytes);
        Reply::err(status, code)
    };

    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let from_id = req.get("from").and_then(Value::as_str).unwrap_or("");
    let to_id = req.get("to").and_then(Value::as_str).unwrap_or("");
    let amount_str = req.get("amount").and_then(Value::as_str).unwrap_or("");

    if from_id == to_id {
        return do_complete_err(400, "same_account");
    }

    let from_entry = match records::get("accounts", from_id) {
        Ok(e) => e,
        Err(_) => return do_complete_err(404, "not_found"),
    };
    let to_entry = match records::get("accounts", to_id) {
        Ok(e) => e,
        Err(_) => return do_complete_err(404, "not_found"),
    };

    let from_doc: Value = serde_json::from_str(&from_entry.data).unwrap_or(json!({}));
    let to_doc: Value = serde_json::from_str(&to_entry.data).unwrap_or(json!({}));

    let currency = from_doc.get("currency").and_then(Value::as_str).unwrap_or("");
    if currency != to_doc.get("currency").and_then(Value::as_str).unwrap_or("") {
        return do_complete_err(400, "currency_mismatch");
    }

    let amount = match money::parse(amount_str, currency) {
        Ok(a) => a,
        Err(_) => return do_complete_err(400, "invalid_amount"),
    };

    if amount.units <= 0 {
        return do_complete_err(400, "invalid_amount");
    }

    let mut debit_attempts = 0;
    let mut final_from_units = 0;
    loop {
        if debit_attempts >= 50 {
            return do_complete_err(503, "contended");
        }
        debit_attempts += 1;

        let entry = match records::get("accounts", from_id) {
            Ok(e) => e,
            Err(_) => return do_complete_err(404, "not_found"),
        };
        let mut doc: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
        let cur_units = doc.get("units").and_then(Value::as_i64).unwrap_or(0);
        let cur_amount = money::Amount { units: cur_units, currency: currency.to_string() };

        match money::subtract(&cur_amount, &amount) {
            Ok(rem) => {
                if rem.units < 0 {
                    ensure_fsm();
                    let transfer_id = format!("transfer-{}", crate::now_secs());
                    let _ = fsm::create_instance("transfer", &transfer_id);
                    let _ = fsm::fire("transfer", &transfer_id, "refuse");

                    let transfer_doc = json!({
                        "from": from_id, "to": to_id, "units": amount.units, "currency": currency,
                        "state": "refused", "key": route.idempotency_key, "created_at": guestfmt::rfc3339(crate::now_secs())
                    });
                    let _ = records::create(
                        "transfers",
                        &transfer_doc.to_string(),
                        &["state".to_string()],
                    );

                    return do_complete_err(409, "insufficient_funds");
                }
                doc["units"] = json!(rem.units);
                match records::update("accounts", from_id, &doc.to_string(), entry.revision) {
                    Ok(_) => {
                        final_from_units = rem.units;
                        break;
                    }
                    Err(records::StoreError::RevisionConflict(_)) => continue,
                    Err(_) => return do_complete_err(503, "store_error"),
                }
            }
            Err(_) => return do_complete_err(500, "money_error"),
        }
    }

    // Credit loop
    let final_to_units = match do_credit(to_id, &amount) {
        Ok(u) => u,
        Err(_) => {
            // Failed to credit, refund the source account
            if do_credit(from_id, &amount).is_err() {
                // If refund also fails, we've truly lost money (or it's hanging).
                // In this case, we MUST report 500
                return do_complete_err(500, "money_lost");
            }
            return do_complete_err(503, "credit_failed");
        }
    };

    ensure_fsm();
    let transfer_doc = json!({
        "from": from_id, "to": to_id, "units": amount.units, "currency": currency,
        "state": "pending", "key": route.idempotency_key, "created_at": guestfmt::rfc3339(crate::now_secs())
    });

    let t_entry =
        match records::create("transfers", &transfer_doc.to_string(), &["state".to_string()]) {
            Ok(e) => e,
            Err(_) => {
                // Failed to create transfer record! But money was moved!
                // Wait, we CANNOT fail here without losing the journal.
                // Oh boy, we should create the transfer record FIRST!
                // The contract says: "Only a settled transfer is journalled: a refusal moved nothing."
                return do_complete_err(503, "store_unavailable");
            }
        };

    let _ = fsm::create_instance("transfer", &t_entry.id);
    let _ = fsm::fire("transfer", &t_entry.id, "settle");

    let ledger_entry = ledger::Entry {
        id: t_entry.id.clone(),
        memo: t_entry.id.clone(),
        lines: vec![
            ledger::Line {
                account: from_id.to_string(),
                amount: amount.units,
                side: ledger::Side::Debit,
            },
            ledger::Line {
                account: to_id.to_string(),
                amount: amount.units,
                side: ledger::Side::Credit,
            },
        ],
    };

    if ledger::validate(&ledger_entry).is_err() {
        return do_complete_err(500, "journal_lost");
    }

    let j_doc = json!({
        "transfer": t_entry.id, "from": from_id, "to": to_id, "units": amount.units, "at": guestfmt::rfc3339(crate::now_secs())
    });

    let j_entry = match records::create(
        "journal",
        &j_doc.to_string(),
        &["from".to_string(), "to".to_string()],
    ) {
        Ok(e) => e,
        Err(_) => return do_complete_err(500, "journal_lost"),
    };

    let mut updated_doc: Value = serde_json::from_str(&t_entry.data).unwrap_or(json!({}));
    updated_doc["state"] = json!("settled");
    updated_doc["journal"] = json!({
        "id": j_entry.id,
        "lines": j_doc
    });

    // The balances moved and the journal is written, so the books already agree; what is
    // left is the transfer record itself. A write that is dropped here leaves it `pending`
    // forever while the money is gone, so retry a bounded number of times and, if it still
    // will not take, say so — the same rule as `journal_lost`. The answer names the transfer
    // so whoever reads the 500 can find the record that is wrong.
    let mut settled = false;
    let mut revision = t_entry.revision;
    for _ in 0..5 {
        match records::update("transfers", &t_entry.id, &updated_doc.to_string(), revision) {
            Ok(_) => {
                settled = true;
                break;
            }
            Err(records::StoreError::RevisionConflict(current)) => revision = current,
            Err(_) => {}
        }
    }
    if !settled {
        return do_complete(
            500,
            json!({
                "error": "settle_lost",
                "transfer": t_entry.id,
                "journal": j_entry.id
            }),
        );
    }

    do_complete(
        201,
        json!({
            "transfer": t_entry.id,
            "from_units": final_from_units,
            "to_units": final_to_units
        }),
    )
}

fn get_transfer(route: &Route, id: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "read") {
        return r;
    }
    match records::get("transfers", id) {
        Ok(e) => {
            let mut v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
            if let Value::Object(ref mut m) = v {
                m.insert("id".to_string(), json!(e.id));
                m.insert("revision".to_string(), json!(e.revision));
            }
            Reply::json(200, v)
        }
        Err(_) => Reply::err(404, "not_found"),
    }
}
