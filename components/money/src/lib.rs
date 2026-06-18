//! `money` — reference implementation of `money:amount`.
//!
//! Exact money arithmetic over integer minor units. The classic bug is storing
//! money as a float (`0.1 + 0.2 != 0.3`); the fix as a capability is to never
//! use floats at all. An amount is an `s64` count of minor units (cents, pence,
//! fils, satoshi-like) tagged with an ISO-4217 currency code, and every
//! operation here is integer-only and overflow-checked.
//!
//! The currency exponent table (how many decimal places a currency has) is
//! built in: 2 for USD/EUR/GBP/..., 0 for JPY/KRW, 3 for the Gulf dinars
//! (BHD/KWD/TND). `parse`/`format` use that exponent to move between a decimal
//! string and minor units. `allocate` splits a total across N shares with no
//! lost or gained pennies — the remainder is handed out one minor-unit at a
//! time to the leading shares so the parts sum back EXACTLY to the total.
//!
//! Pure compute: no host imports, no state. No floating point anywhere.

#[allow(warnings)]
mod bindings;

use bindings::exports::money::amount::arithmetic::{Amount, Guest, MoneyError};

struct Component;

// ---- ISO-4217 exponent table -------------------------------------------

/// Decimal exponent (number of minor-unit digits) for a currency code, or
/// `None` if the code is not in our built-in table.
fn exponent(currency: &str) -> Option<u32> {
    match currency {
        // exponent 2 — the common case
        "USD" | "EUR" | "GBP" | "CHF" | "CAD" | "AUD" | "CNY" | "INR" | "SEK" | "NOK" | "DKK"
        | "PLN" => Some(2),
        // exponent 0 — no minor unit
        "JPY" | "KRW" => Some(0),
        // exponent 3 — Gulf dinars + Tunisian dinar
        "BHD" | "KWD" | "TND" => Some(3),
        _ => None,
    }
}

/// Validate `currency` against the table, returning its exponent.
fn checked_exponent(currency: &str) -> Result<u32, MoneyError> {
    exponent(currency).ok_or_else(|| MoneyError::UnknownCurrency(currency.to_string()))
}

/// 10^exp as an i64 (exp is at most 3 here, so this never overflows).
fn pow10(exp: u32) -> i64 {
    (0..exp).fold(1i64, |acc, _| acc * 10)
}

impl Guest for Component {
    fn parse(decimal: String, currency: String) -> Result<Amount, MoneyError> {
        let exp = checked_exponent(&currency)?;

        // Strip a single leading sign.
        let (negative, digits) = match decimal.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, decimal.as_str()),
        };

        // Split into major / minor on the (optional) decimal point.
        let (major_str, minor_str) = match digits.split_once('.') {
            Some((m, f)) => (m, f),
            None => (digits, ""),
        };

        // Major part must be non-empty and all digits ("" allowed for ".05"? no —
        // require at least one major digit, e.g. "0.05").
        if major_str.is_empty() || !major_str.bytes().all(|b| b.is_ascii_digit()) {
            return Err(MoneyError::UnknownCurrency(currency));
        }
        // Minor part must be all digits and exactly `exp` of them.
        if !minor_str.bytes().all(|b| b.is_ascii_digit()) || minor_str.len() != exp as usize {
            return Err(MoneyError::UnknownCurrency(currency));
        }

        let major: i64 = major_str
            .parse()
            .map_err(|_| MoneyError::Overflow)?;
        let minor: i64 = if minor_str.is_empty() {
            0
        } else {
            minor_str.parse().map_err(|_| MoneyError::Overflow)?
        };

        // units = major * 10^exp + minor  (all checked)
        let scaled = major
            .checked_mul(pow10(exp))
            .ok_or(MoneyError::Overflow)?;
        let mut units = scaled.checked_add(minor).ok_or(MoneyError::Overflow)?;
        if negative {
            units = units.checked_neg().ok_or(MoneyError::Overflow)?;
        }

        Ok(Amount { units, currency })
    }

    fn format(a: Amount) -> Result<String, MoneyError> {
        let exp = checked_exponent(&a.currency)?;
        if exp == 0 {
            return Ok(a.units.to_string());
        }

        let negative = a.units < 0;
        // unsigned_abs avoids overflow on i64::MIN.
        let abs = a.units.unsigned_abs();
        let divisor = pow10(exp) as u64;
        let major = abs / divisor;
        let minor = abs % divisor;

        let sign = if negative { "-" } else { "" };
        Ok(format!(
            "{sign}{major}.{minor:0width$}",
            width = exp as usize
        ))
    }

    fn add(a: Amount, b: Amount) -> Result<Amount, MoneyError> {
        if a.currency != b.currency {
            return Err(MoneyError::CurrencyMismatch);
        }
        checked_exponent(&a.currency)?;
        let units = a.units.checked_add(b.units).ok_or(MoneyError::Overflow)?;
        Ok(Amount {
            units,
            currency: a.currency,
        })
    }

    fn subtract(a: Amount, b: Amount) -> Result<Amount, MoneyError> {
        if a.currency != b.currency {
            return Err(MoneyError::CurrencyMismatch);
        }
        checked_exponent(&a.currency)?;
        let units = a.units.checked_sub(b.units).ok_or(MoneyError::Overflow)?;
        Ok(Amount {
            units,
            currency: a.currency,
        })
    }

    fn scale(a: Amount, factor: i64) -> Result<Amount, MoneyError> {
        checked_exponent(&a.currency)?;
        let units = a.units.checked_mul(factor).ok_or(MoneyError::Overflow)?;
        Ok(Amount {
            units,
            currency: a.currency,
        })
    }

    fn allocate(total: Amount, shares: u32) -> Result<Vec<Amount>, MoneyError> {
        if shares == 0 {
            return Err(MoneyError::DivideByZero);
        }
        checked_exponent(&total.currency)?;

        let n = shares as i64;
        // Integer division in Rust truncates toward zero, matching the spec.
        let base = total.units / n;
        let remainder = total.units - base * n; // same sign as total.units, |rem| < n

        // Hand out one minor unit to the first |remainder| shares, sign-aware so
        // the parts sum back to total.units exactly.
        let extra: i64 = if remainder >= 0 { 1 } else { -1 };
        let count = remainder.unsigned_abs() as usize;

        let mut out = Vec::with_capacity(shares as usize);
        for i in 0..shares as usize {
            let units = if i < count { base + extra } else { base };
            out.push(Amount {
                units,
                currency: total.currency.clone(),
            });
        }
        Ok(out)
    }

    fn compare(a: Amount, b: Amount) -> Result<i8, MoneyError> {
        if a.currency != b.currency {
            return Err(MoneyError::CurrencyMismatch);
        }
        checked_exponent(&a.currency)?;
        Ok(match a.units.cmp(&b.units) {
            core::cmp::Ordering::Less => -1,
            core::cmp::Ordering::Equal => 0,
            core::cmp::Ordering::Greater => 1,
        })
    }
}

bindings::export!(Component with_types_in bindings);
