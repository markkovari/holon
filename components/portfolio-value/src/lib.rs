//! `portfolio-value` — what a collection is worth now, what it cost, and what selling has already made
//!
//! `tests/valuation.rs` is the specification and is not writable from here.
//!
//! The chart data behind a collection: cost basis, unrealised gain, realised
//! gain, and the value series a line chart is drawn from. Pure compute — events
//! and quotes in, numbers out. Nothing here fetches a price or reads a store.
//!
//! ## FIFO, and why it is not a preference
//!
//! Sell one of three copies of the same card and something has to decide WHICH
//! copy left. Average cost is easier and wrong here: a collector buys the same
//! card at wildly different prices over years, and the whole question they are
//! asking — "did I do well on that one?" — is about a specific purchase. FIFO
//! answers it, matches how a tax authority reads a disposal in most places, and
//! is deterministic, which average cost also is but blurs.
//!
//! So: a sale consumes the OLDEST unsold lot first, at that lot's own cost.
//!
//! ## Money is integer minor units, tagged with a currency
//!
//! Same rule as `money:amount`: never a float, and two currencies never add.
//! Mixing them is an error rather than a conversion, because the exchange rate on
//! the day of a purchase eight years ago is not something this component can
//! know, and inventing one would put a wrong number on a chart that looks right.
//!
//! ## An unpriced holding is not worth zero
//!
//! Most of a real collection has no quote: bulk commons, a card whose set is not
//! in the price source, a misprint nobody lists. Valuing those at zero makes a
//! portfolio chart lie downward, and dropping them makes it lie upward. They are
//! carried at COST and counted, so the caller can say "plus 340 cards not
//! priced" instead of showing a number that pretends to be complete.

use std::collections::{HashMap, VecDeque};

/// A currency, ISO-4217, as it arrives — compared exactly, never converted.
pub type Currency = String;

/// What happened to a card, and when.
///
/// A swap is two events, not one: the card that left is a disposal at the value
/// both sides agreed, and the card that arrived is an acquisition at that same
/// value. That keeps a trade honest in both directions — a swap where you got the
/// better end shows up as a gain, which is the point of recording it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    /// Bought, or received in a swap, at `unit_minor`.
    Acquired,
    /// Sold, or given up in a swap, at `unit_minor`.
    Disposed,
}

/// One thing that happened to one card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Which card. Opaque here: whatever the caller uses to identify a printing.
    pub card_id: String,
    pub kind: EventKind,
    /// How many copies. Zero is an error, not a no-op.
    pub quantity: u32,
    /// Price per copy, in minor units of `currency`.
    pub unit_minor: i64,
    pub currency: Currency,
    /// Unix seconds. Events are sorted by this rather than trusted in order.
    pub at: u64,
}

/// The latest known price for a card at some instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    pub card_id: String,
    pub unit_minor: i64,
    pub currency: Currency,
    /// Unix seconds this price was observed. A quote is used for any instant at
    /// or after this one, until a later quote replaces it.
    pub at: u64,
}

/// What a collection is worth, and how it got there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Valuation {
    /// What the cards still held cost.
    pub cost_basis_minor: i64,
    /// What the cards still held are worth — quoted at market, unquoted at cost.
    pub market_value_minor: i64,
    /// `market_value_minor - cost_basis_minor`. Held cards only.
    pub unrealised_minor: i64,
    /// Proceeds minus FIFO cost, over every disposal up to this instant.
    pub realised_minor: i64,
    pub currency: Currency,
    /// Copies still held that no quote covered. They are inside
    /// `market_value_minor` at cost, and named here so a caller can say so.
    pub unquoted: u32,
}

/// One sample on the value chart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Point {
    pub at: u64,
    pub market_value_minor: i64,
    pub cost_basis_minor: i64,
    pub realised_minor: i64,
    pub unquoted: u32,
}

/// Why a valuation could not be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueError {
    /// Two currencies in one portfolio. Says which, because the fix is a data
    /// fix and the caller needs to know where to look.
    MixedCurrency { expected: Currency, found: Currency },
    /// A disposal of more copies than were held at that instant. The event log is
    /// wrong, and guessing which is a bigger lie than refusing.
    OversoldAt { card_id: String, at: u64, held: u32, disposed: u32 },
    /// A zero-quantity event: not a no-op, because it is always a bug upstream.
    ZeroQuantity { card_id: String, at: u64 },
    /// `series` was asked for a step of zero, which does not terminate.
    ZeroStep,
    /// No events at all — there is no currency to report, so there is no answer.
    Empty,
}

/// What the collection is worth at `at`, over `events` priced by `quotes`.
///
/// Events after `at` are ignored, so this is also how the history is walked.
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
            return Err(ValueError::MixedCurrency { expected: currency, found: e.currency.clone() });
        }
    }

    // Sorted by time, never trusted in the order given — a backfilled purchase
    // must land where it happened, not where it was recorded.
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

/// `value_at` sampled every `step` seconds over `from..=until`, for a chart.
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
