//! `ledger` — double-entry bookkeeping — validate that debits equal credits, and roll entries into balances
//!
//! The double-entry invariant, as pure functions over integer minor units (no
//! float rounding): `validate` enforces that an entry has >= 2 lines, positive
//! amounts, and equal debits and credits; `trial_balance` aggregates a set of
//! validated entries into per-account totals whose grand totals are equal. No
//! state, no host imports.

#[allow(warnings)]
mod bindings;

use std::collections::BTreeMap;

use bindings::exports::ledger::doubleentry::ledger::{
    AccountBalance, Entry, Guest, LedgerError, Side, Trial,
};

struct Component;

/// Sum an entry's debits and credits (checking each amount is positive).
fn totals(e: &Entry) -> Result<(i64, i64), LedgerError> {
    if e.lines.len() < 2 {
        return Err(LedgerError::TooFewLines);
    }
    let mut debits = 0i64;
    let mut credits = 0i64;
    for l in &e.lines {
        if l.amount <= 0 {
            return Err(LedgerError::Nonpositive(l.account.clone()));
        }
        match l.side {
            Side::Debit => debits += l.amount,
            Side::Credit => credits += l.amount,
        }
    }
    Ok((debits, credits))
}

impl Guest for Component {
    fn validate(e: Entry) -> Result<(), LedgerError> {
        let (debits, credits) = totals(&e)?;
        if debits != credits {
            return Err(LedgerError::Unbalanced((debits, credits)));
        }
        Ok(())
    }

    fn trial_balance(entries: Vec<Entry>) -> Result<Trial, LedgerError> {
        // (debits, credits) per account, kept ordered by account name.
        let mut acc: BTreeMap<String, (i64, i64)> = BTreeMap::new();
        for e in &entries {
            let (debits, credits) = totals(e)?;
            if debits != credits {
                return Err(LedgerError::Unbalanced((debits, credits)));
            }
            for l in &e.lines {
                let slot = acc.entry(l.account.clone()).or_insert((0, 0));
                match l.side {
                    Side::Debit => slot.0 += l.amount,
                    Side::Credit => slot.1 += l.amount,
                }
            }
        }
        let mut total_debits = 0i64;
        let mut total_credits = 0i64;
        let accounts: Vec<AccountBalance> = acc
            .into_iter()
            .map(|(account, (debits, credits))| {
                total_debits += debits;
                total_credits += credits;
                AccountBalance { account, debits, credits, net: debits - credits }
            })
            .collect();
        Ok(Trial { accounts, total_debits, total_credits, balanced: total_debits == total_credits })
    }
}

bindings::export!(Component with_types_in bindings);
