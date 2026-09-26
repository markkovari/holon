//! Part 4 — the payout ledger. A vendor's balance, the platform's own
//! cash/fees balance and the payout that zeroes a vendor all read
//! `ledger_entries` back through `ledger:doubleentry/ledger`'s own
//! `trial-balance` — never a hand-rolled sum, which is the way two parts
//! silently disagree about "the same" balance (CONTRACT.md).

use crate::bindings::ledger::doubleentry::ledger as doubleentry;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{introspect, is_admin, Reply, Route};
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    let subject = || route.segments.get(2).map(String::as_str).unwrap_or("");
    match (method, seg.as_slice()) {
        (Method::Get, ["api", "vendors", _, "balance"]) => vendor_balance(route, subject()),
        (Method::Get, ["api", "ledger", "platform"]) => platform_balance(route),
        (Method::Post, ["api", "vendors", _, "payout"]) => payout(route, subject()),
        _ => Reply::err(404, "not_found"),
    }
}

/// Every stored `ledger_entries` document, converted into a
/// `ledger:doubleentry/ledger` `entry`. `records:store` is the only surface
/// shared between parts, so this is where a part 4 balance comes from.
fn load_entries() -> Result<Vec<doubleentry::Entry>, ()> {
    let mut out = Vec::new();
    for stored in crate::list_all("ledger_entries")? {
        let doc: Value = serde_json::from_str(&stored.data).unwrap_or_else(|_| json!({}));
        let memo = doc.get("memo").and_then(Value::as_str).unwrap_or("").to_string();
        let mut lines = Vec::new();
        if let Some(raw_lines) = doc.get("lines").and_then(Value::as_array) {
            for line in raw_lines {
                let account = line.get("account").and_then(Value::as_str).unwrap_or("").to_string();
                let amount = line.get("amount").and_then(Value::as_i64).unwrap_or(0);
                let side = match line.get("side").and_then(Value::as_str) {
                    Some("debit") => doubleentry::Side::Debit,
                    _ => doubleentry::Side::Credit,
                };
                lines.push(doubleentry::Line { account, amount, side });
            }
        }
        out.push(doubleentry::Entry { id: stored.id, memo, lines });
    }
    Ok(out)
}

/// The trial balance over EVERY stored entry — the one source all three
/// routes read. An account with nothing posted to it is simply absent from
/// `accounts`; every caller reads that as `0`, not as an error.
fn trial_accounts() -> Result<Vec<doubleentry::AccountBalance>, Reply> {
    let entries = load_entries().map_err(|_| Reply::err(500, "store_error"))?;
    match doubleentry::trial_balance(&entries) {
        Ok(trial) => Ok(trial.accounts),
        // An empty ledger is a balance of zero (CONTRACT.md).
        Err(_) if entries.is_empty() => Ok(Vec::new()),
        Err(_) => Err(Reply::err(500, "ledger_error")),
    }
}

/// `trial-balance`'s own `net` is debit-positive (debits − credits) — the
/// natural direction for an ASSET like `platform:cash`, but the opposite of
/// the natural direction for a liability/income account. A vendor with 900
/// credited is owed +900, not −900, and fees earned are +100, not −100. Both
/// readings are the SAME `trial-balance` result; the sign is which way the
/// account faces, not a second, hand-rolled sum.
fn net_for(accounts: &[doubleentry::AccountBalance], account: &str) -> i64 {
    accounts.iter().find(|a| a.account == account).map(|a| a.net).unwrap_or(0)
}

/// The same account read credit-positive: what the platform owes a vendor,
/// and what the platform has earned in fees.
fn owed_for(accounts: &[doubleentry::AccountBalance], account: &str) -> i64 {
    -net_for(accounts, account)
}

fn admin_or_self(route: &Route, subject: &str) -> Result<(), Reply> {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(_) => return Err(Reply::err(401, "unauthorized")),
    };
    if principal.subject == subject || is_admin(&principal) {
        Ok(())
    } else {
        Err(Reply::err(403, "forbidden"))
    }
}

fn admin_only(route: &Route) -> Result<(), Reply> {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(_) => return Err(Reply::err(401, "unauthorized")),
    };
    if is_admin(&principal) {
        Ok(())
    } else {
        Err(Reply::err(403, "forbidden"))
    }
}

fn vendor_balance(route: &Route, subject: &str) -> Reply {
    if let Err(r) = admin_or_self(route, subject) {
        return r;
    }
    let accounts = match trial_accounts() {
        Ok(a) => a,
        Err(r) => return r,
    };
    let balance = owed_for(&accounts, &format!("vendor:{subject}"));
    Reply::json(200, json!({"vendor": subject, "balance": balance}))
}

fn platform_balance(route: &Route) -> Reply {
    if let Err(r) = admin_only(route) {
        return r;
    }
    let accounts = match trial_accounts() {
        Ok(a) => a,
        Err(r) => return r,
    };
    Reply::json(
        200,
        json!({
            "cash": net_for(&accounts, "platform:cash"),
            "fees": owed_for(&accounts, "platform:fees"),
        }),
    )
}

fn payout(route: &Route, subject: &str) -> Reply {
    if let Err(r) = admin_only(route) {
        return r;
    }
    let accounts = match trial_accounts() {
        Ok(a) => a,
        Err(r) => return r,
    };
    let vendor_account = format!("vendor:{subject}");
    let balance = owed_for(&accounts, &vendor_account);
    if balance <= 0 {
        return Reply::json(400, json!({"error": "nothing_owed"}));
    }
    let entry = doubleentry::Entry {
        id: format!("payout:{subject}"),
        memo: format!("payout {vendor_account}"),
        lines: vec![
            doubleentry::Line {
                account: vendor_account.clone(),
                amount: balance,
                side: doubleentry::Side::Debit,
            },
            doubleentry::Line {
                account: "platform:cash".to_string(),
                amount: balance,
                side: doubleentry::Side::Credit,
            },
        ],
    };
    // The ledger invariant is the component's, not ours: nothing is stored
    // that it has not called valid.
    if doubleentry::validate(&entry).is_err() {
        return Reply::err(500, "ledger_error");
    }
    let data = json!({
        "memo": entry.memo,
        "lines": [
            {"account": vendor_account, "amount": balance, "side": "debit"},
            {"account": "platform:cash", "amount": balance, "side": "credit"},
        ],
    })
    .to_string();
    match records::create("ledger_entries", &data, &[]) {
        Ok(_) => Reply::json(200, json!({"vendor": subject, "amount": balance, "balance": 0})),
        Err(_) => Reply::err(500, "store_error"),
    }
}
