//! `price-history` — what a card sold for over time, from quotes that arrive late, out of order and in gaps
//!
//! `tests/history.rs` is the specification and is not writable from here.
//!
//! Turns the quotes a price source actually returns — sparse, duplicated,
//! sometimes stale, sometimes from two sources disagreeing on the same day — into
//! the series a chart can be drawn from. Pure compute: quotes in, series out. It
//! fetches nothing; the fetching is a provider behind `price:source`, and this is
//! the part that has to be right whichever provider answers.
//!
//! ## Carry forward, never interpolate
//!
//! A market has no price on a day nobody traded. The last known price is still
//! the price — that is what "the card is worth €40" means on a Sunday. Drawing a
//! straight line between Friday and Monday invents two days of movement that did
//! not happen, and on a 5-year chart that invention is most of the line.
//!
//! So a gap carries the previous quote forward, unchanged, and the point says it
//! was carried. A caller that wants to render carried points differently — a
//! dashed segment, a lighter stroke — has what it needs; one that does not can
//! ignore the flag and still not be lied to.
//!
//! ## Before the first quote there is no price
//!
//! Not zero. A card whose first observation is in 2021 has no 2019 price, and a
//! chart that starts at zero and jumps shows a gain nobody made. Those samples
//! are absent from the series rather than zero-valued.
//!
//! ## Stale is returned, and labelled
//!
//! A quote from four months ago is the best information available and also barely
//! information. Refusing it would leave a caller with nothing for every card that
//! has stopped trading; returning it silently would put a confident number on a
//! dead listing. It comes back with `age_seconds`, and the caller decides.

/// A currency, ISO-4217, compared exactly and never converted.
pub type Currency = String;

/// Which number a source was asked for. A market price and a "lowest listed" are
/// different questions and must never be mixed into one series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteKind {
    /// What copies are actually changing hands at.
    Market,
    /// The cheapest current listing.
    Low,
    /// The dearest current listing.
    High,
    /// The last completed sale.
    LastSold,
}

/// One observation of a price.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    pub unit_minor: i64,
    pub currency: Currency,
    pub kind: QuoteKind,
    /// Where it came from. Two sources may disagree about the same instant, and
    /// resolving that needs to know which is which.
    pub source: String,
    /// Unix seconds the price was observed.
    pub at: u64,
}

/// A price at an instant, and how much to trust it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub unit_minor: i64,
    pub currency: Currency,
    pub source: String,
    /// When the underlying quote was observed — NOT the instant asked about.
    pub observed_at: u64,
    /// How old the quote was at the instant asked about. Zero means it was
    /// observed exactly then.
    pub age_seconds: u64,
    /// True when this is a previous quote carried forward across a gap rather
    /// than an observation at this instant.
    pub carried: bool,
}

/// One sample on a price chart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Point {
    pub at: u64,
    pub unit_minor: i64,
    pub carried: bool,
}

/// Why a lookup produced nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriceError {
    /// No quote of this kind at or before the instant asked about. Distinct from
    /// a price of zero, and the caller must render it differently.
    NotYetPriced,
    /// Quotes in more than one currency for one card. Refused rather than
    /// converted — see the module header on `portfolio-value` for why.
    MixedCurrency { expected: Currency, found: Currency },
    /// A step of zero does not terminate.
    ZeroStep,
}

fn check_currency(quotes: &[&Quote]) -> Result<(), PriceError> {
    let mut expected: Option<&Currency> = None;
    for q in quotes {
        match expected {
            None => expected = Some(&q.currency),
            Some(e) if e != &q.currency => {
                return Err(PriceError::MixedCurrency { expected: e.clone(), found: q.currency.clone() })
            }
            _ => {}
        }
    }
    Ok(())
}

/// The price of one card at one instant: the latest quote of `kind` at or before
/// `at`, carried forward if it is older than `at`.
pub fn at(quotes: &[Quote], kind: QuoteKind, at: u64) -> Result<Observed, PriceError> {
    let matching: Vec<&Quote> = quotes.iter().filter(|q| q.kind == kind).collect();
    check_currency(&matching)?;

    let mut best: Option<&Quote> = None;
    for q in matching.iter().filter(|q| q.at <= at) {
        best = match best {
            None => Some(q),
            Some(b) if q.at > b.at => Some(q),
            Some(b) if q.at == b.at && q.source < b.source => Some(q),
            Some(b) => Some(b),
        };
    }

    let q = best.ok_or(PriceError::NotYetPriced)?;
    Ok(Observed {
        unit_minor: q.unit_minor,
        currency: q.currency.clone(),
        source: q.source.clone(),
        observed_at: q.at,
        age_seconds: at - q.at,
        carried: q.at != at,
    })
}

/// The price series over `from..=until`, sampled every `step` seconds.
///
/// Samples before the first quote are ABSENT rather than zero, so an empty result
/// means "never priced in this window" and a short one means the card started
/// being priced partway through.
pub fn series(
    quotes: &[Quote],
    kind: QuoteKind,
    from: u64,
    until: u64,
    step: u64,
) -> Result<Vec<Point>, PriceError> {
    if step == 0 {
        return Err(PriceError::ZeroStep);
    }

    let mut times = Vec::new();
    let mut t = from;
    loop {
        times.push(t);
        if t > until {
            break;
        }
        match t.checked_add(step) {
            Some(next) if next <= until => t = next,
            _ => break,
        }
    }
    if times.last() != Some(&until) {
        times.push(until);
    }

    let mut points = Vec::with_capacity(times.len());
    for t in times {
        match at(quotes, kind, t) {
            Ok(obs) => points.push(Point { at: t, unit_minor: obs.unit_minor, carried: obs.carried }),
            Err(PriceError::NotYetPriced) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(points)
}
