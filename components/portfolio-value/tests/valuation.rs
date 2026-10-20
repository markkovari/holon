use portfolio_value::{series, value_at, Event, EventKind, Point, Quote, Valuation, ValueError};

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

/// Bought 2 @ €10, 1 @ €40, sold 1 @ €30: FIFO realises €20 (basis €50); average cost would say €10 (basis €40).
#[test]
fn a_sale_consumes_the_oldest_lot_first() {
    let events = vec![
        buy("base-4", 2, 1000, 100),
        buy("base-4", 1, 4000, 200),
        sell("base-4", 1, 3000, 300),
    ];
    let v = value_at(&events, &[], 400).expect("valuation");
    assert_eq!(v.realised_minor, 2000, "FIFO: sold the €10.00 copy for €30.00");
    assert_eq!(v.cost_basis_minor, 5000, "one €10.00 lot and one €40.00 lot are still held");
}

/// The latest quote at or before the instant: not the newest in the list, not interpolated.
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

#[test]
fn unpriced_holdings_are_carried_at_cost_and_counted() {
    let events = vec![buy("priced", 1, 1000, 0), buy("bulk-common", 40, 5, 0)];
    let quotes = vec![quote("priced", 8000, 0)];
    let v = value_at(&events, &quotes, DAY).expect("valuation");
    assert_eq!(v.unquoted, 40, "forty commons nothing quoted");
    assert_eq!(v.cost_basis_minor, 1000 + 200);
    assert_eq!(v.market_value_minor, 8000 + 200, "the commons at cost, not zero and not omitted");
}

#[test]
fn events_out_of_order_give_the_same_answer_as_events_in_order() {
    let ordered = vec![
        buy("base-4", 2, 1000, 100),
        buy("base-4", 1, 4000, 200),
        sell("base-4", 1, 3000, 300),
    ];
    let shuffled = vec![ordered[2].clone(), ordered[0].clone(), ordered[1].clone()];
    assert_eq!(
        value_at(&ordered, &[], 400).expect("ordered"),
        value_at(&shuffled, &[], 400).expect("shuffled"),
        "a backfilled purchase is not a different portfolio"
    );
}

/// A rate from eight years ago is not knowable here, and a made-up one produces a chart that looks right.
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

/// No price in this portfolio's currency IS no price.
#[test]
fn a_quote_in_another_currency_does_not_price_a_holding() {
    let events = vec![buy("base-4", 1, 1000, 0)];
    let quotes =
        vec![Quote { card_id: "base-4".into(), unit_minor: 9999, currency: "USD".into(), at: 0 }];
    let v = value_at(&events, &quotes, DAY).expect("valuation");
    assert_eq!(v.market_value_minor, 1000, "not 9999, and not converted");
    assert_eq!(v.unquoted, 1);
}

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

#[test]
fn a_zero_quantity_event_is_refused() {
    let events = vec![buy("base-4", 0, 1000, 100)];
    assert!(matches!(value_at(&events, &[], 200), Err(ValueError::ZeroQuantity { .. })));
}

#[test]
fn a_fully_sold_position_leaves_realised_gain_and_no_basis() {
    let events = vec![buy("base-4", 2, 1000, 100), sell("base-4", 2, 2500, 200)];
    let v = value_at(&events, &quotes_for("base-4", 9999), 300).expect("valuation");
    assert_eq!(v.cost_basis_minor, 0);
    assert_eq!(
        v.market_value_minor, 0,
        "nothing held, so no market value — the quote is irrelevant"
    );
    assert_eq!(v.unrealised_minor, 0);
    assert_eq!(v.realised_minor, 3000, "€50.00 proceeds against €20.00 cost");
}

fn quotes_for(card: &str, unit: i64) -> Vec<Quote> {
    vec![quote(card, unit, 0)]
}

/// Nothing clamps at zero: a collection that went down did go down.
#[test]
fn a_loss_is_negative_rather_than_clamped() {
    let events = vec![buy("base-4", 1, 10_000, 100), sell("base-4", 1, 4_000, 200)];
    let v = value_at(&events, &[], 300).expect("valuation");
    assert_eq!(v.realised_minor, -6_000);

    let held = vec![buy("base-4", 1, 10_000, 100)];
    let v = value_at(&held, &quotes_for("base-4", 4_000), 300).expect("valuation");
    assert_eq!(v.unrealised_minor, -6_000);
}

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

/// The last point must agree with the headline "today" total.
#[test]
fn the_series_always_ends_at_until() {
    let events = vec![buy("base-4", 1, 1000, 0)];
    let points = series(&events, &[], 0, 10 * DAY, 3 * DAY).expect("series");
    assert_eq!(points.last().expect("a point").at, 10 * DAY);
    let ats: Vec<u64> = points.iter().map(|p: &Point| p.at).collect();
    assert_eq!(ats, vec![0, 3 * DAY, 6 * DAY, 9 * DAY, 10 * DAY]);
}

#[test]
fn a_zero_step_is_refused() {
    let events = vec![buy("base-4", 1, 1000, 0)];
    assert!(matches!(series(&events, &[], 0, DAY, 0), Err(ValueError::ZeroStep)));
}

#[test]
fn an_empty_event_log_has_no_answer() {
    assert!(matches!(value_at(&[], &[], 100), Err(ValueError::Empty)));
}

#[test]
fn a_later_quote_does_not_move_realised_gain() {
    let events = vec![buy("base-4", 2, 1000, 100), sell("base-4", 1, 3000, 200)];
    let cheap = value_at(&events, &quotes_for("base-4", 1_000), 300).expect("valuation");
    let dear = value_at(&events, &quotes_for("base-4", 90_000), 300).expect("valuation");
    assert_eq!(cheap.realised_minor, dear.realised_minor, "a sale is history, not a live number");
    assert_ne!(cheap.market_value_minor, dear.market_value_minor);
}

#[test]
fn the_valuation_reports_its_currency() {
    let v: Valuation = value_at(&[buy("base-4", 1, 1000, 0)], &[], DAY).expect("valuation");
    assert_eq!(v.currency, EUR);
}
