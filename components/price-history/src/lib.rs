//! `price-history` — what a card sold for over time, from quotes that arrive late, out of order and in gaps
//! Before the first quote samples are absent, not zero; stale quotes are labelled with `age_seconds`.

/// A currency, ISO-4217, compared exactly and never converted.
pub type Currency = String;

/// A market price and a "lowest listed" are different questions, never mixed in one series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteKind {
    /// What copies are actually changing hands at.
    Market,
    Low,
    High,
    /// The last completed sale.
    LastSold,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    pub unit_minor: i64,
    pub currency: Currency,
    pub kind: QuoteKind,
    /// Two sources may disagree about one instant; ties resolve by this name.
    pub source: String,
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub unit_minor: i64,
    pub currency: Currency,
    pub source: String,
    /// NOT the instant asked about.
    pub observed_at: u64,
    pub age_seconds: u64,
    pub carried: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Point {
    pub at: u64,
    pub unit_minor: i64,
    pub carried: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriceError {
    /// Distinct from a price of zero; the caller must render it differently.
    NotYetPriced,
    /// Refused rather than converted (see portfolio-value).
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
                return Err(PriceError::MixedCurrency {
                    expected: e.clone(),
                    found: q.currency.clone(),
                })
            }
            _ => {}
        }
    }
    Ok(())
}

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

/// Samples before the first quote are ABSENT: empty means never priced in this window.
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
            Ok(obs) => {
                points.push(Point { at: t, unit_minor: obs.unit_minor, carried: obs.carried })
            }
            Err(PriceError::NotYetPriced) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(points)
}

// Thin WIT mapping only: tests/history.rs judges the plain functions. Gated on wasm32 because
// a cdylib with wit-bindgen exports does not link natively.

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

#[cfg(target_arch = "wasm32")]
use bindings::exports::price::history::history as w;

#[cfg(target_arch = "wasm32")]
struct Component;

#[cfg(target_arch = "wasm32")]
fn kind_in(k: w::QuoteKind) -> QuoteKind {
    match k {
        w::QuoteKind::Market => QuoteKind::Market,
        w::QuoteKind::Low => QuoteKind::Low,
        w::QuoteKind::High => QuoteKind::High,
        w::QuoteKind::LastSold => QuoteKind::LastSold,
    }
}

#[cfg(target_arch = "wasm32")]
fn quotes_in(qs: &[w::Quote]) -> Vec<Quote> {
    qs.iter()
        .map(|q| Quote {
            unit_minor: q.unit_minor,
            currency: q.currency.clone(),
            kind: kind_in(q.kind),
            source: q.source.clone(),
            at: q.at,
        })
        .collect()
}

#[cfg(target_arch = "wasm32")]
fn err_out(e: PriceError) -> w::PriceError {
    match e {
        PriceError::NotYetPriced => w::PriceError::NotYetPriced,
        PriceError::MixedCurrency { expected, found } => {
            w::PriceError::MixedCurrency((expected, found))
        }
        PriceError::ZeroStep => w::PriceError::ZeroStep,
    }
}

#[cfg(target_arch = "wasm32")]
impl w::Guest for Component {
    fn at(
        quotes: Vec<w::Quote>,
        kind: w::QuoteKind,
        at: u64,
    ) -> Result<w::Observed, w::PriceError> {
        crate::at(&quotes_in(&quotes), kind_in(kind), at)
            .map(|o| w::Observed {
                unit_minor: o.unit_minor,
                currency: o.currency,
                source: o.source,
                observed_at: o.observed_at,
                age_seconds: o.age_seconds,
                carried: o.carried,
            })
            .map_err(err_out)
    }

    fn series(
        quotes: Vec<w::Quote>,
        kind: w::QuoteKind,
        since: u64,
        until: u64,
        step: u64,
    ) -> Result<Vec<w::Point>, w::PriceError> {
        crate::series(&quotes_in(&quotes), kind_in(kind), since, until, step)
            .map(|ps| {
                ps.into_iter()
                    .map(|p| w::Point { at: p.at, unit_minor: p.unit_minor, carried: p.carried })
                    .collect()
            })
            .map_err(err_out)
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);
