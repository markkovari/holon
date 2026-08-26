//! The specification, held out: this file is not writable by the goal.
//!
//! Every case here is a way a portfolio chart lies. The expensive ones are FIFO
//! (average cost gets a plausible-looking wrong answer), the unpriced holding
//! (zero or dropped, both wrong in a direction), and events arriving out of
//! order, which is the normal case when somebody backfills a purchase from 2019.

use portfolio_value::{series, value_at, Event, EventKind, Point, Quote, ValueError, Valuation};

const EUR: &str = "EUR";
const DAY: u64 = 86_400;

fn buy(card: &str, qty: u32, unit: i64, at: u64) -> Event {
    Event {
        card_id: card.into(),
        kind: EventKind::Acquired,
        quantity: qty,
        unit_minor: unit,
        currency: EUR.into(),
        at,
    }
}

fn sell(card: &str, qty: u32, unit: i64, at: u64) -> Event {
    Event {
        card_id: card.into(),
        kind: EventKind::Disposed,
        quantity: qty,
        unit_minor: unit,
        currency: EUR.into(),
        at,
    }
}

fn quote(card: &str, unit: i64, at: u64) -> Quote {
    Quote { card_id: card.into(), unit_minor: unit, currency: EUR.into(), at }
}

/// THE case. Two lots at different prices, one copy sold: which copy left?
///
/// Bought 2 @ €10.00, then 1 @ €40.00, then sold one @ €30.00.
///
/// FIFO: the sold copy cost €10.00, so realised is €20.00 and the two held
/// copies cost €10.00 + €40.00 = €50.00. Average cost would say the sold copy
/// cost €20.00 — realised €10.00, basis €40.00 — which is a different chart and
/// the wrong answer to "did I do well on that one?".
#[test]
fn a_sale_consumes_the_oldest_lot_first() {
    let events = vec![buy("base-4", 2, 1000, 100), buy("base-4", 1, 4000, 200), sell("base-4", 1, 3000, 300)];
    let v = value_at(&events, &[], 400).expect("valuation");
    assert_eq!(v.realised_minor, 2000, "FIFO: sold the €10.00 copy for €30.00");
    assert_eq!(v.cost_basis_minor, 5000, "one €10.00 lot and one €40.00 lot are still held");
}

/// A quote is the latest one at or before the instant asked about — not the
/// newest in the list, and not interpolated between two.
#[test]
fn a_holding_is_valued_at_the_latest_quote_at_or_before_the_instant() {
    let events = vec![buy("base-4", 1, 1000, 0)];
    let quotes = vec![quote("base-4", 5000, 10 * DAY), quote("base-4", 9000, 30 * DAY)];

    let early = value_at(&events, &quotes, 5 * DAY).expect("valuation");
    assert_eq!(early.market_value_minor, 1000, "before any quote, a holding sits at cost");
    assert_eq!(early.unquoted, 1, "and is counted as unpriced");

    let mid = value_at(&events, &quotes, 20 * DAY).expect("valuation");
    assert_eq!(mid.market_value_minor, 5000, "the €50 quote, not the later €90 one");
    assert_eq!(mid.unquoted, 0);
    assert_eq!(mid.unrealised_minor, 4000, "€50 market against €10 cost");

    let late = value_at(&events, &quotes, 40 * DAY).expect("valuation");
    assert_eq!(late.market_value_minor, 9000);
}

/// An unpriced holding is carried at cost and COUNTED. Zero would make the chart
/// dip, dropping it would make it climb, and neither is what happened.
#[test]
fn unpriced_holdings_are_carried_at_cost_and_counted() {
    let events = vec![buy("priced", 1, 1000, 0), buy("bulk-common", 40, 5, 0)];
    let quotes = vec![quote("priced", 8000, 0)];
    let v = value_at(&events, &quotes, DAY).expect("valuation");
    assert_eq!(v.unquoted, 40, "forty commons nothing quoted");
    assert_eq!(v.cost_basis_minor, 1000 + 200);
    assert_eq!(v.market_value_minor, 8000 + 200, "the commons at cost, not zero and not omitted");
}

/// Events are sorted by time, not trusted in the order given. Backfilling an old
/// purchase is normal, and it must not change the answer.
#[test]
fn events_out_of_order_give_the_same_answer_as_events_in_order() {
    let ordered = vec![buy("base-4", 2, 1000, 100), buy("base-4", 1, 4000, 200), sell("base-4", 1, 3000, 300)];
    let shuffled = vec![ordered[2].clone(), ordered[0].clone(), ordered[1].clone()];
    assert_eq!(
        value_at(&ordered, &[], 400).expect("ordered"),
        value_at(&shuffled, &[], 400).expect("shuffled"),
        "a backfilled purchase is not a different portfolio"
    );
}

/// Two currencies in one portfolio is refused. The rate on the day of a purchase
/// eight years ago is not knowable here, and a made-up one produces a chart that
/// looks right.
#[test]
fn two_currencies_are_an_error_and_not_a_conversion() {
    let mut usd = buy("base-4", 1, 1000, 200);
    usd.currency = "USD".into();
    let events = vec![buy("base-4", 1, 1000, 100), usd];
    match value_at(&events, &[], 300) {
        Err(ValueError::MixedCurrency { expected, found }) => {
            assert_eq!(expected, "EUR");
            assert_eq!(found, "USD");
        }
        other => panic!("expected MixedCurrency, got {other:?}"),
    }
}

/// A quote in the wrong currency is ignored rather than added. Same reason, and
/// the holding falls back to cost and is counted unpriced — which is honest: no
/// price in this portfolio's currency IS no price.
#[test]
fn a_quote_in_another_currency_does_not_price_a_holding() {
    let events = vec![buy("base-4", 1, 1000, 0)];
    let quotes = vec![Quote { card_id: "base-4".into(), unit_minor: 9999, currency: "USD".into(), at: 0 }];
    let v = value_at(&events, &quotes, DAY).expect("valuation");
    assert_eq!(v.market_value_minor, 1000, "not 9999, and not converted");
    assert_eq!(v.unquoted, 1);
}

/// Selling more than is held is a broken event log. Refused, naming the card and
/// the instant, because the caller has to go and fix data.
#[test]
fn selling_more_than_is_held_is_refused() {
    let events = vec![buy("base-4", 1, 1000, 100), sell("base-4", 2, 3000, 200)];
    match value_at(&events, &[], 300) {
        Err(ValueError::OversoldAt { card_id, at, held, disposed }) => {
            assert_eq!(card_id, "base-4");
            assert_eq!((at, held, disposed), (200, 1, 2));
        }
        other => panic!("expected OversoldAt, got {other:?}"),
    }
}

/// A zero-quantity event is always an upstream bug, so it is never silently
/// skipped.
#[test]
fn a_zero_quantity_event_is_refused() {
    let events = vec![buy("base-4", 0, 1000, 100)];
    assert!(matches!(value_at(&events, &[], 200), Err(ValueError::ZeroQuantity { .. })));
}

/// Selling everything: the position is flat, the gain is realised, and nothing
/// lingers as unrealised.
#[test]
fn a_fully_sold_position_leaves_realised_gain_and_no_basis() {
    let events = vec![buy("base-4", 2, 1000, 100), sell("base-4", 2, 2500, 200)];
    let v = value_at(&events, &quotes_for("base-4", 9999), 300).expect("valuation");
    assert_eq!(v.cost_basis_minor, 0);
    assert_eq!(v.market_value_minor, 0, "nothing held, so no market value — the quote is irrelevant");
    assert_eq!(v.unrealised_minor, 0);
    assert_eq!(v.realised_minor, 3000, "€50.00 proceeds against €20.00 cost");
}

fn quotes_for(card: &str, unit: i64) -> Vec<Quote> {
    vec![quote(card, unit, 0)]
}

/// A loss is a negative number and is reported as one. Nothing here clamps at
/// zero: a collection that went down did go down.
#[test]
fn a_loss_is_negative_rather_than_clamped() {
    let events = vec![buy("base-4", 1, 10_000, 100), sell("base-4", 1, 4_000, 200)];
    let v = value_at(&events, &[], 300).expect("valuation");
    assert_eq!(v.realised_minor, -6_000);

    let held = vec![buy("base-4", 1, 10_000, 100)];
    let v = value_at(&held, &quotes_for("base-4", 4_000), 300).expect("valuation");
    assert_eq!(v.unrealised_minor, -6_000);
}

/// The series is what a chart is drawn from: step-aligned samples from `from`,
/// each one the valuation as of that instant.
#[test]
fn the_series_samples_the_valuation_at_each_step() {
    let events = vec![buy("base-4", 1, 1000, 0), buy("base-4", 1, 2000, 2 * DAY)];
    let quotes = vec![quote("base-4", 3000, 0)];
    let points = series(&events, &quotes, 0, 3 * DAY, DAY).expect("series");
    assert_eq!(points.len(), 4, "from, +1d, +2d, +3d — inclusive of both ends");
    assert_eq!(points[0].at, 0);
    assert_eq!(points[3].at, 3 * DAY);
    assert_eq!(points[0].cost_basis_minor, 1000);
    assert_eq!(points[1].cost_basis_minor, 1000, "the second purchase has not happened yet");
    assert_eq!(points[2].cost_basis_minor, 3000, "now it has");
    assert_eq!(points[2].market_value_minor, 6000, "two copies at the €30 quote");
}

/// A step that does not divide the window still ends at `until`, so the last
/// point on the chart is the number the headline figure agrees with. A chart
/// whose final point is three days stale next to a "today" total is the bug this
/// prevents.
#[test]
fn the_series_always_ends_at_until() {
    let events = vec![buy("base-4", 1, 1000, 0)];
    let points = series(&events, &[], 0, 10 * DAY, 3 * DAY).expect("series");
    assert_eq!(points.last().expect("a point").at, 10 * DAY);
    let ats: Vec<u64> = points.iter().map(|p: &Point| p.at).collect();
    assert_eq!(ats, vec![0, 3 * DAY, 6 * DAY, 9 * DAY, 10 * DAY]);
}

/// A zero step does not terminate, so it is refused rather than hung on.
#[test]
fn a_zero_step_is_refused() {
    let events = vec![buy("base-4", 1, 1000, 0)];
    assert!(matches!(series(&events, &[], 0, DAY, 0), Err(ValueError::ZeroStep)));
}

/// No events means there is no currency to report a zero IN, so it is an error
/// rather than a zero-valued portfolio in an invented currency.
#[test]
fn an_empty_event_log_has_no_answer() {
    assert!(matches!(value_at(&[], &[], 100), Err(ValueError::Empty)));
}

/// Realised gain never moves once it has happened. A later quote changes what the
/// remaining cards are worth and nothing about a sale that already completed.
#[test]
fn a_later_quote_does_not_move_realised_gain() {
    let events = vec![buy("base-4", 2, 1000, 100), sell("base-4", 1, 3000, 200)];
    let cheap = value_at(&events, &quotes_for("base-4", 1_000), 300).expect("valuation");
    let dear = value_at(&events, &quotes_for("base-4", 90_000), 300).expect("valuation");
    assert_eq!(cheap.realised_minor, dear.realised_minor, "a sale is history, not a live number");
    assert_ne!(cheap.market_value_minor, dear.market_value_minor);
}

/// The currency is reported, and it is the one the events are in.
#[test]
fn the_valuation_reports_its_currency() {
    let v: Valuation = value_at(&[buy("base-4", 1, 1000, 0)], &[], DAY).expect("valuation");
    assert_eq!(v.currency, EUR);
}
