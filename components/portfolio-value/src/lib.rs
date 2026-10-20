//! FIFO, not average cost: a collector asks about a specific purchase. Integer minor units, no FX.
//! An unpriced holding is carried at COST and counted: zero lies downward, dropping it lies upward.

use std::collections::{HashMap, VecDeque};

/// Compared exactly, never converted.
pub type Currency = String;

/// A swap is two events at the agreed value, so a lopsided swap shows up as a gain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    /// Bought, or received in a swap, at `unit_minor`.
    Acquired,
    /// Sold, or given up in a swap, at `unit_minor`.
    Disposed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Opaque: whatever the caller uses to identify a printing.
    pub card_id: String,
    pub kind: EventKind,
    /// Zero is an error, not a no-op.
    pub quantity: u32,
    pub unit_minor: i64,
    pub currency: Currency,
    /// Sorted by this rather than trusted in order.
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    pub card_id: String,
    pub unit_minor: i64,
    pub currency: Currency,
    /// Used for any instant at or after this one, until a later quote replaces it.
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Valuation {
    pub cost_basis_minor: i64,
    /// Quoted at market, unquoted at cost.
    pub market_value_minor: i64,
    pub unrealised_minor: i64,
    /// Proceeds minus FIFO cost, over every disposal up to this instant.
    pub realised_minor: i64,
    pub currency: Currency,
    /// Inside `market_value_minor` at cost, named so a caller can say so.
    pub unquoted: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Point {
    pub at: u64,
    pub market_value_minor: i64,
    pub cost_basis_minor: i64,
    pub realised_minor: i64,
    pub unquoted: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueError {
    /// Names both: the fix is a data fix.
    MixedCurrency { expected: Currency, found: Currency },
    /// Guessing which event is the lie is a bigger lie than refusing.
    OversoldAt { card_id: String, at: u64, held: u32, disposed: u32 },
    /// Always a bug upstream, so not a no-op.
    ZeroQuantity { card_id: String, at: u64 },
    /// A zero step does not terminate.
    ZeroStep,
    /// No events means no currency to report.
    Empty,
}

/// Events after `at` are ignored, which is also how the history is walked.
pub fn value_at(events: &[Event], quotes: &[Quote], at: u64) -> Result<Valuation, ValueError> {
    if events.is_empty() {
        return Err(ValueError::Empty);
    }

    let currency = events[0].currency.clone();
    for e in events {
        if e.quantity == 0 {
            return Err(ValueError::ZeroQuantity { card_id: e.card_id.clone(), at: e.at });
        }
        if e.currency != currency {
            return Err(ValueError::MixedCurrency {
                expected: currency,
                found: e.currency.clone(),
            });
        }
    }

    // Sorted by time: a backfilled purchase must land where it happened.
    let mut sorted: Vec<&Event> = events.iter().collect();
    sorted.sort_by_key(|e| e.at);

    let mut lots: HashMap<&str, VecDeque<(u32, i64)>> = HashMap::new();
    let mut realised: i64 = 0;

    for e in &sorted {
        if e.at > at {
            continue;
        }
        match e.kind {
            EventKind::Acquired => {
                lots.entry(&e.card_id).or_default().push_back((e.quantity, e.unit_minor));
            }
            EventKind::Disposed => {
                let dq = lots.entry(&e.card_id).or_default();
                let held: u32 = dq.iter().map(|(q, _)| *q).sum();
                if held < e.quantity {
                    return Err(ValueError::OversoldAt {
                        card_id: e.card_id.clone(),
                        at: e.at,
                        held,
                        disposed: e.quantity,
                    });
                }
                let mut remaining = e.quantity;
                let mut cost = 0i64;
                while remaining > 0 {
                    let (q, unit) = dq.front_mut().expect("held covers disposed");
                    if *q <= remaining {
                        cost += *q as i64 * *unit;
                        remaining -= *q;
                        dq.pop_front();
                    } else {
                        cost += remaining as i64 * *unit;
                        *q -= remaining;
                        remaining = 0;
                    }
                }
                realised += e.quantity as i64 * e.unit_minor - cost;
            }
        }
    }

    let mut cost_basis = 0i64;
    let mut market_value = 0i64;
    let mut unquoted = 0u32;
    for (card_id, dq) in &lots {
        let held: u32 = dq.iter().map(|(q, _)| *q).sum();
        if held == 0 {
            continue;
        }
        let lot_cost: i64 = dq.iter().map(|(q, u)| *q as i64 * *u).sum();
        cost_basis += lot_cost;

        let best_quote = quotes
            .iter()
            .filter(|q| q.card_id == *card_id && q.at <= at && q.currency == currency)
            .max_by_key(|q| q.at);

        match best_quote {
            Some(q) => market_value += held as i64 * q.unit_minor,
            None => {
                market_value += lot_cost;
                unquoted += held;
            }
        }
    }

    Ok(Valuation {
        cost_basis_minor: cost_basis,
        market_value_minor: market_value,
        unrealised_minor: market_value - cost_basis,
        realised_minor: realised,
        currency,
        unquoted,
    })
}

pub fn series(
    events: &[Event],
    quotes: &[Quote],
    from: u64,
    until: u64,
    step: u64,
) -> Result<Vec<Point>, ValueError> {
    if step == 0 {
        return Err(ValueError::ZeroStep);
    }

    let mut points = Vec::new();
    let mut t = from;
    loop {
        let v = value_at(events, quotes, t)?;
        points.push(Point {
            at: t,
            market_value_minor: v.market_value_minor,
            cost_basis_minor: v.cost_basis_minor,
            realised_minor: v.realised_minor,
            unquoted: v.unquoted,
        });
        if t >= until {
            break;
        }
        t = t.saturating_add(step);
        if t > until {
            t = until;
        }
    }
    Ok(points)
}

// Thin WIT mapping only: tests/valuation.rs judges the plain functions. Gated on wasm32 because
// a cdylib with wit-bindgen exports does not link natively.

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

#[cfg(target_arch = "wasm32")]
use bindings::exports::portfolio::value::valuation as w;

#[cfg(target_arch = "wasm32")]
struct Component;

#[cfg(target_arch = "wasm32")]
impl From<&w::Event> for Event {
    fn from(e: &w::Event) -> Self {
        Event {
            card_id: e.item_id.clone(),
            kind: match e.kind {
                w::EventKind::Acquired => EventKind::Acquired,
                w::EventKind::Disposed => EventKind::Disposed,
            },
            quantity: e.quantity,
            unit_minor: e.unit_minor,
            currency: e.currency.clone(),
            at: e.at,
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl From<&w::Quote> for Quote {
    fn from(q: &w::Quote) -> Self {
        Quote {
            card_id: q.item_id.clone(),
            unit_minor: q.unit_minor,
            currency: q.currency.clone(),
            at: q.at,
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl From<ValueError> for w::ValueError {
    fn from(e: ValueError) -> Self {
        match e {
            ValueError::MixedCurrency { expected, found } => {
                w::ValueError::MixedCurrency((expected, found))
            }
            ValueError::OversoldAt { card_id, at, held, disposed } => {
                w::ValueError::OversoldAt((card_id, at, held, disposed))
            }
            ValueError::ZeroQuantity { card_id, at } => w::ValueError::ZeroQuantity((card_id, at)),
            ValueError::ZeroStep => w::ValueError::ZeroStep,
            ValueError::Empty => w::ValueError::Empty,
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl From<Valuation> for w::Valuation {
    fn from(v: Valuation) -> Self {
        w::Valuation {
            cost_basis_minor: v.cost_basis_minor,
            market_value_minor: v.market_value_minor,
            unrealised_minor: v.unrealised_minor,
            realised_minor: v.realised_minor,
            currency: v.currency,
            unquoted: v.unquoted,
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl w::Guest for Component {
    fn value_at(
        events: Vec<w::Event>,
        quotes: Vec<w::Quote>,
        at: u64,
    ) -> Result<w::Valuation, w::ValueError> {
        let events: Vec<Event> = events.iter().map(Into::into).collect();
        let quotes: Vec<Quote> = quotes.iter().map(Into::into).collect();
        value_at(&events, &quotes, at).map(Into::into).map_err(Into::into)
    }

    fn series(
        events: Vec<w::Event>,
        quotes: Vec<w::Quote>,
        since: u64,
        until: u64,
        step: u64,
    ) -> Result<Vec<w::Point>, w::ValueError> {
        let events: Vec<Event> = events.iter().map(Into::into).collect();
        let quotes: Vec<Quote> = quotes.iter().map(Into::into).collect();
        series(&events, &quotes, since, until, step)
            .map(|ps| {
                ps.into_iter()
                    .map(|p| w::Point {
                        at: p.at,
                        market_value_minor: p.market_value_minor,
                        cost_basis_minor: p.cost_basis_minor,
                        realised_minor: p.realised_minor,
                        unquoted: p.unquoted,
                    })
                    .collect()
            })
            .map_err(Into::into)
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);
