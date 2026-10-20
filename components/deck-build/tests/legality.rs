//! The held-out specification; the deciding case is `four_copies_counts_names_not_printings`.

use deck_build::{legality, shortfall, CardKind, Illegal, Missing, Owned, Price, Slot};

const EUR: &str = "EUR";

fn slot(id: &str, name: &str, kind: CardKind, quantity: u32) -> Slot {
    Slot { card_id: id.into(), name: name.into(), kind, quantity }
}

fn legal_deck() -> Vec<Slot> {
    vec![
        slot("sv3-001", "Charmander", CardKind::BasicPokemon, 4),
        slot("sv3-125", "Charizard ex", CardKind::EvolvedPokemon, 4),
        slot("sv3-200", "Professor's Research", CardKind::Trainer, 4),
        slot("sv3-201", "Boss's Orders", CardKind::Trainer, 4),
        slot("sv3-202", "Ultra Ball", CardKind::Trainer, 4),
        slot("sv3-203", "Nest Ball", CardKind::Trainer, 4),
        slot("sv3-204", "Rare Candy", CardKind::Trainer, 4),
        slot("sv3-205", "Switch", CardKind::Trainer, 4),
        slot("sv3-206", "Iono", CardKind::Trainer, 4),
        slot("sv3-207", "Arven", CardKind::Trainer, 4),
        slot("sve-002", "Fire Energy", CardKind::BasicEnergy, 20),
    ]
}

#[test]
fn a_sixty_card_deck_with_a_basic_and_no_fifth_copy_is_legal() {
    let deck = legal_deck();
    assert_eq!(deck.iter().map(|s| s.quantity).sum::<u32>(), 60, "the fixture is 60");
    assert_eq!(legality(&deck), vec![], "nothing wrong with it: {:?}", legality(&deck));
}

/// Counting by `card_id` (the obvious implementation) lets this through.
#[test]
fn four_copies_counts_names_not_printings() {
    let mut deck = legal_deck();
    deck.retain(|s| s.name != "Fire Energy");
    deck.push(slot("base1-046", "Charmander", CardKind::BasicPokemon, 4));
    deck.push(slot("sve-002", "Fire Energy", CardKind::BasicEnergy, 16));

    let why = legality(&deck);
    assert!(
        why.contains(&Illegal::TooManyOfAName { name: "Charmander".into(), count: 8 }),
        "two printings of one name are eight of that card: {why:?}"
    );
    assert_eq!(why.len(), 1, "{why:?}");
}

#[test]
fn basic_energy_is_uncapped() {
    let deck = vec![
        slot("sv3-001", "Charmander", CardKind::BasicPokemon, 4),
        slot("sve-002", "Fire Energy", CardKind::BasicEnergy, 56),
    ];
    assert_eq!(legality(&deck), vec![], "fifty-six basic energy is legal: {:?}", legality(&deck));
}

#[test]
fn special_energy_is_capped_like_everything_else() {
    let deck = vec![
        slot("sv3-001", "Charmander", CardKind::BasicPokemon, 4),
        slot("sv3-300", "Double Turbo Energy", CardKind::SpecialEnergy, 8),
        slot("sve-002", "Fire Energy", CardKind::BasicEnergy, 48),
    ];
    assert_eq!(
        legality(&deck),
        vec![Illegal::TooManyOfAName { name: "Double Turbo Energy".into(), count: 8 }]
    );
}

#[test]
fn a_deck_is_exactly_sixty_cards() {
    let mut small = legal_deck();
    small.last_mut().expect("energy").quantity = 19;
    assert!(legality(&small).contains(&Illegal::WrongSize(59)), "{:?}", legality(&small));

    let mut big = legal_deck();
    big.last_mut().expect("energy").quantity = 21;
    assert!(legality(&big).contains(&Illegal::WrongSize(61)), "{:?}", legality(&big));
}

#[test]
fn a_deck_needs_at_least_one_basic_pokemon() {
    let deck = vec![
        slot("sv3-125", "Charizard ex", CardKind::EvolvedPokemon, 4),
        slot("sv3-200", "Professor's Research", CardKind::Trainer, 4),
        slot("sve-002", "Fire Energy", CardKind::BasicEnergy, 52),
    ];
    assert!(legality(&deck).contains(&Illegal::NoBasicPokemon), "{:?}", legality(&deck));
}

#[test]
fn every_reason_is_reported_not_just_the_first() {
    let deck = vec![
        slot("sv3-125", "Charizard ex", CardKind::EvolvedPokemon, 5),
        slot("sv3-200", "Professor's Research", CardKind::Trainer, 4),
    ];
    let why = legality(&deck);
    assert!(why.contains(&Illegal::WrongSize(9)), "{why:?}");
    assert!(why.contains(&Illegal::NoBasicPokemon), "{why:?}");
    assert!(
        why.contains(&Illegal::TooManyOfAName { name: "Charizard ex".into(), count: 5 }),
        "{why:?}"
    );
}

#[test]
fn a_zero_quantity_slot_is_refused() {
    let mut deck = legal_deck();
    deck.push(slot("sv3-999", "Nothing", CardKind::Trainer, 0));
    assert!(
        legality(&deck).contains(&Illegal::ZeroQuantity("sv3-999".into())),
        "{:?}",
        legality(&deck)
    );
}

fn price(id: &str, unit: i64) -> Price {
    Price { card_id: id.into(), unit_minor: unit, currency: EUR.into() }
}

#[test]
fn the_shortfall_is_what_you_do_not_already_own() {
    let deck = vec![
        slot("sv3-001", "Charmander", CardKind::BasicPokemon, 4),
        slot("sv3-125", "Charizard ex", CardKind::EvolvedPokemon, 2),
    ];
    let owned = vec![Owned { card_id: "sv3-001".into(), quantity: 3 }];
    let prices = vec![price("sv3-001", 500), price("sv3-125", 4000)];

    let s = shortfall(&deck, &owned, &prices, EUR);
    assert_eq!(
        s.missing,
        vec![
            Missing {
                card_id: "sv3-001".into(),
                name: "Charmander".into(),
                quantity: 1,
                cost_minor: Some(500)
            },
            Missing {
                card_id: "sv3-125".into(),
                name: "Charizard ex".into(),
                quantity: 2,
                cost_minor: Some(8000)
            },
        ],
        "sorted by card id, and only the difference"
    );
    assert_eq!(s.cost_minor, 8500);
    assert_eq!(s.currency, EUR);
    assert_eq!(s.unpriced, 0);
}

#[test]
fn owning_enough_leaves_nothing_to_buy() {
    let deck = vec![slot("sv3-001", "Charmander", CardKind::BasicPokemon, 2)];
    let owned = vec![Owned { card_id: "sv3-001".into(), quantity: 9 }];
    let s = shortfall(&deck, &owned, &[price("sv3-001", 500)], EUR);
    assert!(s.missing.is_empty(), "{:?}", s.missing);
    assert_eq!(s.cost_minor, 0);
}

#[test]
fn a_missing_card_with_no_price_is_counted_not_costed() {
    let deck = vec![
        slot("sv3-001", "Charmander", CardKind::BasicPokemon, 2),
        slot("wbsp-009", "Mew", CardKind::BasicPokemon, 1),
    ];
    let s = shortfall(&deck, &[], &[price("sv3-001", 500)], EUR);
    assert_eq!(s.cost_minor, 1000, "only the priced line is in the total");
    assert_eq!(s.unpriced, 1);
    let mew = s.missing.iter().find(|m| m.card_id == "wbsp-009").expect("listed anyway");
    assert_eq!(mew.cost_minor, None, "not Some(0) — nothing said it was free");
}

#[test]
fn a_price_in_another_currency_does_not_count() {
    let deck = vec![slot("sv3-001", "Charmander", CardKind::BasicPokemon, 1)];
    let usd = Price { card_id: "sv3-001".into(), unit_minor: 9999, currency: "USD".into() };
    let s = shortfall(&deck, &[], &[usd], EUR);
    assert_eq!(s.cost_minor, 0);
    assert_eq!(s.unpriced, 1);
    assert_eq!(s.missing[0].cost_minor, None);
}

/// Two printings are different cards to buy, though one name to the four-copy rule.
#[test]
fn the_shopping_list_is_by_printing_even_though_the_cap_is_by_name() {
    let deck = vec![
        slot("sv3-001", "Charmander", CardKind::BasicPokemon, 2),
        slot("base1-046", "Charmander", CardKind::BasicPokemon, 2),
    ];
    let s = shortfall(&deck, &[], &[price("sv3-001", 500), price("base1-046", 9000)], EUR);
    assert_eq!(s.missing.len(), 2, "two printings, two lines: {:?}", s.missing);
    assert_eq!(s.cost_minor, 1000 + 18_000, "and the dear one is not priced as the cheap one");
}

#[test]
fn an_empty_deck_needs_nothing() {
    let s = shortfall(&[], &[], &[], EUR);
    assert!(s.missing.is_empty());
    assert_eq!(s.cost_minor, 0);
    assert_eq!(s.currency, EUR, "the currency asked for is still reported");
    assert_eq!(s.unpriced, 0);
}
