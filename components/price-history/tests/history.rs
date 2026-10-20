use price_history::{at, series, Observed, Point, PriceError, Quote, QuoteKind};

const DAY: u64 = 86_400;
const EUR: &str = "EUR";

fn q(unit: i64, day: u64, kind: QuoteKind, source: &str) -> Quote {
    Quote { unit_minor: unit, currency: EUR.into(), kind, source: source.into(), at: day * DAY }
}

fn market(unit: i64, day: u64) -> Quote {
    q(unit, day, QuoteKind::Market, "fixture")
}

/// The latest quote at or before the instant — not the newest in the list.
#[test]
fn a_price_is_the_latest_quote_at_or_before_the_instant() {
    let quotes = vec![market(4000, 10), market(9000, 30)];
    let obs: Observed = at(&quotes, QuoteKind::Market, 20 * DAY).expect("priced");
    assert_eq!(obs.unit_minor, 4000, "the €40 quote, not the later €90 one");
    assert_eq!(obs.observed_at, 10 * DAY);
    assert_eq!(obs.age_seconds, 10 * DAY, "ten days stale at the instant asked about");
    assert!(obs.carried, "carried forward across the gap");
}

#[test]
fn a_quote_at_the_exact_instant_is_neither_carried_nor_stale() {
    let obs = at(&[market(4000, 10)], QuoteKind::Market, 10 * DAY).expect("priced");
    assert_eq!(obs.age_seconds, 0);
    assert!(!obs.carried);
}

#[test]
fn before_the_first_quote_there_is_no_price() {
    assert_eq!(at(&[market(4000, 10)], QuoteKind::Market, 5 * DAY), Err(PriceError::NotYetPriced));
}

/// They answer different questions and one is systematically below the other.
#[test]
fn kinds_do_not_mix() {
    let quotes = vec![market(4000, 10), q(1200, 20, QuoteKind::Low, "fixture")];
    let obs = at(&quotes, QuoteKind::Market, 25 * DAY).expect("priced");
    assert_eq!(obs.unit_minor, 4000, "the €12 low listing is not a market price");

    let low = at(&quotes, QuoteKind::Low, 25 * DAY).expect("priced");
    assert_eq!(low.unit_minor, 1200);

    assert_eq!(at(&quotes, QuoteKind::LastSold, 25 * DAY), Err(PriceError::NotYetPriced));
}

/// The NEWEST observation wins; on an exact tie the first-sorting source, whatever the arrival order.
#[test]
fn the_newest_observation_wins_and_ties_are_broken_deterministically() {
    let forward =
        vec![q(4000, 10, QuoteKind::Market, "alpha"), q(5000, 11, QuoteKind::Market, "beta")];
    let reversed: Vec<Quote> = forward.iter().rev().cloned().collect();
    assert_eq!(
        at(&forward, QuoteKind::Market, 20 * DAY).expect("a"),
        at(&reversed, QuoteKind::Market, 20 * DAY).expect("b"),
        "fetch order is not information"
    );
    assert_eq!(at(&forward, QuoteKind::Market, 20 * DAY).expect("priced").unit_minor, 5000);

    let tied =
        vec![q(7000, 10, QuoteKind::Market, "zulu"), q(4000, 10, QuoteKind::Market, "alpha")];
    let obs = at(&tied, QuoteKind::Market, 10 * DAY).expect("priced");
    assert_eq!(obs.source, "alpha", "same instant: the first source by name, so it is stable");
    assert_eq!(obs.unit_minor, 4000);
}

#[test]
fn an_exact_duplicate_changes_nothing() {
    let once = vec![market(4000, 10)];
    let twice = vec![market(4000, 10), market(4000, 10)];
    assert_eq!(at(&once, QuoteKind::Market, 20 * DAY), at(&twice, QuoteKind::Market, 20 * DAY));
    assert_eq!(
        series(&once, QuoteKind::Market, 10 * DAY, 12 * DAY, DAY),
        series(&twice, QuoteKind::Market, 10 * DAY, 12 * DAY, DAY)
    );
}

#[test]
fn two_currencies_are_refused() {
    let mut usd = market(4000, 20);
    usd.currency = "USD".into();
    match at(&[market(4000, 10), usd], QuoteKind::Market, 30 * DAY) {
        Err(PriceError::MixedCurrency { expected, found }) => {
            assert_eq!((expected.as_str(), found.as_str()), ("EUR", "USD"));
        }
        other => panic!("expected MixedCurrency, got {other:?}"),
    }
}

#[test]
fn a_gap_carries_forward_and_never_interpolates() {
    // €40 on day 0, €90 on day 4. Days 1-3 are €40 carried — not €52.50, €65,
    // €77.50, which is what a straight line between them would draw.
    let quotes = vec![market(4000, 0), market(9000, 4)];
    let points = series(&quotes, QuoteKind::Market, 0, 4 * DAY, DAY).expect("series");
    let values: Vec<i64> = points.iter().map(|p: &Point| p.unit_minor).collect();
    assert_eq!(values, vec![4000, 4000, 4000, 4000, 9000], "carried, not interpolated");
    let carried: Vec<bool> = points.iter().map(|p| p.carried).collect();
    assert_eq!(carried, vec![false, true, true, true, false]);
}

#[test]
fn samples_before_the_first_quote_are_absent_not_zero() {
    let points = series(&[market(4000, 3)], QuoteKind::Market, 0, 5 * DAY, DAY).expect("series");
    assert_eq!(points.first().expect("a point").at, 3 * DAY, "the line starts on day 3");
    assert_eq!(points.len(), 3, "days 3, 4, 5 — days 0-2 have no price to draw");
    assert!(points.iter().all(|p| p.unit_minor == 4000));
}

/// Not an error: a caller charting forty cards should get forty answers.
#[test]
fn never_priced_in_the_window_is_an_empty_series() {
    let points = series(&[market(4000, 90)], QuoteKind::Market, 0, 5 * DAY, DAY).expect("series");
    assert!(points.is_empty());
}

/// The last point must agree with the headline number beside it.
#[test]
fn the_series_always_ends_at_until() {
    let points =
        series(&[market(4000, 0)], QuoteKind::Market, 0, 10 * DAY, 3 * DAY).expect("series");
    let ats: Vec<u64> = points.iter().map(|p| p.at).collect();
    assert_eq!(ats, vec![0, 3 * DAY, 6 * DAY, 9 * DAY, 10 * DAY]);
}

#[test]
fn a_zero_step_is_refused() {
    assert_eq!(series(&[market(4000, 0)], QuoteKind::Market, 0, DAY, 0), Err(PriceError::ZeroStep));
}

#[test]
fn no_quotes_is_not_priced() {
    assert_eq!(at(&[], QuoteKind::Market, DAY), Err(PriceError::NotYetPriced));
    assert_eq!(series(&[], QuoteKind::Market, 0, DAY, DAY), Ok(vec![]));
}
