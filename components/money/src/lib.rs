//! Integer minor units only, never a float; every operation is overflow-checked.

#[allow(warnings)]
mod bindings;

use bindings::exports::money::amount::arithmetic::{Amount, Guest, MoneyError};

struct Component;

fn exponent(currency: &str) -> Option<u32> {
    match currency {
        "USD" | "EUR" | "GBP" | "CHF" | "CAD" | "AUD" | "CNY" | "INR" | "SEK" | "NOK" | "DKK"
        | "PLN" => Some(2),
        "JPY" | "KRW" => Some(0),
        "BHD" | "KWD" | "TND" => Some(3),
        _ => None,
    }
}

fn checked_exponent(currency: &str) -> Result<u32, MoneyError> {
    exponent(currency).ok_or_else(|| MoneyError::UnknownCurrency(currency.to_string()))
}

/// exp is at most 3, so this never overflows.
fn pow10(exp: u32) -> i64 {
    (0..exp).fold(1i64, |acc, _| acc * 10)
}

impl Guest for Component {
    fn parse(decimal: String, currency: String) -> Result<Amount, MoneyError> {
        let exp = checked_exponent(&currency)?;

        let (negative, digits) = match decimal.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, decimal.as_str()),
        };

        let (major_str, minor_str) = match digits.split_once('.') {
            Some((m, f)) => (m, f),
            None => (digits, ""),
        };

        // At least one major digit: "0.05", not ".05".
        if major_str.is_empty() || !major_str.bytes().all(|b| b.is_ascii_digit()) {
            return Err(MoneyError::UnknownCurrency(currency));
        }
        if !minor_str.bytes().all(|b| b.is_ascii_digit()) || minor_str.len() != exp as usize {
            return Err(MoneyError::UnknownCurrency(currency));
        }

        let major: i64 = major_str.parse().map_err(|_| MoneyError::Overflow)?;
        let minor: i64 = if minor_str.is_empty() {
            0
        } else {
            minor_str.parse().map_err(|_| MoneyError::Overflow)?
        };

        let scaled = major.checked_mul(pow10(exp)).ok_or(MoneyError::Overflow)?;
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
        Ok(format!("{sign}{major}.{minor:0width$}", width = exp as usize))
    }

    fn add(a: Amount, b: Amount) -> Result<Amount, MoneyError> {
        if a.currency != b.currency {
            return Err(MoneyError::CurrencyMismatch);
        }
        checked_exponent(&a.currency)?;
        let units = a.units.checked_add(b.units).ok_or(MoneyError::Overflow)?;
        Ok(Amount { units, currency: a.currency })
    }

    fn subtract(a: Amount, b: Amount) -> Result<Amount, MoneyError> {
        if a.currency != b.currency {
            return Err(MoneyError::CurrencyMismatch);
        }
        checked_exponent(&a.currency)?;
        let units = a.units.checked_sub(b.units).ok_or(MoneyError::Overflow)?;
        Ok(Amount { units, currency: a.currency })
    }

    fn scale(a: Amount, factor: i64) -> Result<Amount, MoneyError> {
        checked_exponent(&a.currency)?;
        let units = a.units.checked_mul(factor).ok_or(MoneyError::Overflow)?;
        Ok(Amount { units, currency: a.currency })
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

        // Sign-aware, so the parts sum back to total.units exactly.
        let extra: i64 = if remainder >= 0 { 1 } else { -1 };
        let count = remainder.unsigned_abs() as usize;

        let mut out = Vec::with_capacity(shares as usize);
        for i in 0..shares as usize {
            let units = if i < count { base + extra } else { base };
            out.push(Amount { units, currency: total.currency.clone() });
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
