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
pub fn value_at(_events: &[Event], _quotes: &[Quote], _at: u64) -> Result<Valuation, ValueError> {
    Err(ValueError::Empty)
}

/// `value_at` sampled every `step` seconds over `from..=until`, for a chart.
pub fn series(
    _events: &[Event],
    _quotes: &[Quote],
    _from: u64,
    _until: u64,
    _step: u64,
) -> Result<Vec<Point>, ValueError> {
    Err(ValueError::ZeroStep)
}
