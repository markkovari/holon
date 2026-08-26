//! `deck-build` — is this deck legal to play, and what is missing from your collection to build it
//!
//! `tests/legality.rs` is the specification and is not writable from here.
//!
//! Two questions a deck builder has to answer and neither is arithmetic on a list:
//! whether a pile of cards is a deck the rules allow, and — given what you already
//! own — what you still have to buy and what that costs.
//!
//! Pure compute. A deck, a collection and some prices in; a verdict and a shopping
//! list out. Nothing here fetches a price or reads a store.
//!
//! ## The four-copy rule counts NAMES, not printings
//!
//! This is the rule an implementation gets wrong while looking finished. "At most
//! four of any card" means four `Pikachu`, across every set, every printing, every
//! rarity — a Base Set Pikachu and an Obsidian Flames Pikachu are the same card for
//! this purpose even though they are different cards for every other purpose in the
//! app. Counting by the id the collection is keyed on gives a deck of sixteen
//! Pikachu that passes, and it is not a legal deck.
//!
//! ## Basic Energy is exempt, Special Energy is not
//!
//! You may play any number of Basic Energy. "Special Energy" cards are ordinary
//! cards and capped at four like everything else. The two are one word apart and the
//! difference is the whole rule.
//!
//! ## Sixty cards, and at least one Basic Pokémon
//!
//! Exactly sixty — not "at least". And a deck with no Basic Pokémon cannot put
//! anything into play on the first turn, so it is illegal however good it looks.
//! Both are checked, and every reason a deck fails is reported rather than the first
//! one: a builder that fixes one problem per attempt is a builder nobody uses.

/// What a card is, for the rules that care. The collection knows far more about a
/// card than this; a deck only needs to know what the format restricts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardKind {
    /// Can be played to the bench on turn one. A deck needs at least one.
    BasicPokemon,
    /// Evolves from something. Useless without its basic, but that is a deck
    /// builder's problem, not a legality one.
    EvolvedPokemon,
    Trainer,
    /// Uncapped: play as many as you like.
    BasicEnergy,
    /// An ordinary card that happens to be energy. Capped at four.
    SpecialEnergy,
}

/// One line of a deck list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// The printing, as the collection keys it: `sv3-125/197`.
    pub card_id: String,
    /// The printed NAME, which is what the four-copy rule counts.
    pub name: String,
    pub kind: CardKind,
    pub quantity: u32,
}

/// What you already own, by printing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owned {
    pub card_id: String,
    pub quantity: u32,
}

/// The market price of one printing, in minor units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Price {
    pub card_id: String,
    pub unit_minor: i64,
    pub currency: String,
}

/// Why a deck is not legal. Every applicable one is reported, not just the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Illegal {
    /// A deck is exactly sixty cards. Carries the count found.
    WrongSize(u32),
    /// More than four of one NAME, across every printing of it. Carries the name
    /// and the count.
    TooManyOfAName { name: String, count: u32 },
    /// Nothing to put into play on turn one.
    NoBasicPokemon,
    /// A slot with no cards in it is a typo, not an empty slot.
    ZeroQuantity(String),
}

/// One line of what you still have to buy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    pub card_id: String,
    pub name: String,
    /// How many more than you own.
    pub quantity: u32,
    /// `quantity` × the market price, or `None` when nothing has priced it — which
    /// is NOT the same as free, and the caller has to be able to tell.
    pub cost_minor: Option<i64>,
}

/// What it would take to build a deck you do not fully own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortfall {
    /// Sorted by card id, so the same deck always produces the same list.
    pub missing: Vec<Missing>,
    /// The total for the lines that HAVE a price.
    pub cost_minor: i64,
    pub currency: String,
    /// How many missing cards nothing has priced. Their cost is not in
    /// `cost_minor`, and a caller that ignores this is quoting a total that is too
    /// low without saying so.
    pub unpriced: u32,
}

/// Every reason this deck is not legal, or an empty list.
pub fn legality(deck: &[Slot]) -> Vec<Illegal> {
    use std::collections::BTreeMap;

    let mut why = Vec::new();

    for slot in deck {
        if slot.quantity == 0 {
            why.push(Illegal::ZeroQuantity(slot.card_id.clone()));
        }
    }

    let total: u32 = deck.iter().map(|s| s.quantity).sum();
    if total != 60 {
        why.push(Illegal::WrongSize(total));
    }

    if !deck.iter().any(|s| s.kind == CardKind::BasicPokemon && s.quantity > 0) {
        why.push(Illegal::NoBasicPokemon);
    }

    let mut by_name: BTreeMap<&str, u32> = BTreeMap::new();
    for slot in deck {
        if slot.kind != CardKind::BasicEnergy {
            *by_name.entry(slot.name.as_str()).or_insert(0) += slot.quantity;
        }
    }
    for (name, count) in by_name {
        if count > 4 {
            why.push(Illegal::TooManyOfAName { name: name.to_string(), count });
        }
    }

    why
}

/// What is missing from `owned` to build `deck`, and what that costs.
///
/// `currency` is the one to report and the one prices must be in; a price in
/// another currency does not count toward the total and its card is `unpriced`.
pub fn shortfall(deck: &[Slot], owned: &[Owned], prices: &[Price], currency: &str) -> Shortfall {
    use std::collections::BTreeMap;

    let mut needed: BTreeMap<&str, (&str, u32)> = BTreeMap::new();
    for slot in deck {
        let entry = needed.entry(slot.card_id.as_str()).or_insert((slot.name.as_str(), 0));
        entry.1 += slot.quantity;
    }

    let mut have: BTreeMap<&str, u32> = BTreeMap::new();
    for o in owned {
        *have.entry(o.card_id.as_str()).or_insert(0) += o.quantity;
    }

    let mut missing = Vec::new();
    let mut cost_minor = 0i64;
    let mut unpriced = 0u32;

    for (card_id, (name, need)) in needed {
        let owned_qty = have.get(card_id).copied().unwrap_or(0);
        if need <= owned_qty {
            continue;
        }
        let qty = need - owned_qty;
        let price = prices.iter().find(|p| p.card_id == card_id && p.currency == currency);
        let line_cost = price.map(|p| p.unit_minor * qty as i64);
        if let Some(c) = line_cost {
            cost_minor += c;
        } else {
            unpriced += 1;
        }
        missing.push(Missing {
            card_id: card_id.to_string(),
            name: name.to_string(),
            quantity: qty,
            cost_minor: line_cost,
        });
    }

    Shortfall { missing, cost_minor, currency: currency.to_string(), unpriced }
}

// ---- the component -----------------------------------------------------
//
// A mapping between the WIT types and the ones above, and nothing else — the logic
// is judged by `tests/legality.rs` against the plain functions.

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

#[cfg(target_arch = "wasm32")]
use bindings::exports::deck::build::builder as w;

#[cfg(target_arch = "wasm32")]
struct Component;

#[cfg(target_arch = "wasm32")]
fn kind_in(k: w::CardKind) -> CardKind {
    match k {
        w::CardKind::BasicPokemon => CardKind::BasicPokemon,
        w::CardKind::EvolvedPokemon => CardKind::EvolvedPokemon,
        w::CardKind::Trainer => CardKind::Trainer,
        w::CardKind::BasicEnergy => CardKind::BasicEnergy,
        w::CardKind::SpecialEnergy => CardKind::SpecialEnergy,
    }
}

#[cfg(target_arch = "wasm32")]
fn deck_in(deck: &[w::Slot]) -> Vec<Slot> {
    deck.iter()
        .map(|s| Slot {
            card_id: s.card_id.clone(),
            name: s.name.clone(),
            kind: kind_in(s.kind),
            quantity: s.quantity,
        })
        .collect()
}

#[cfg(target_arch = "wasm32")]
impl w::Guest for Component {
    fn legality(deck: Vec<w::Slot>) -> Vec<w::Illegal> {
        crate::legality(&deck_in(&deck))
            .into_iter()
            .map(|i| match i {
                Illegal::WrongSize(n) => w::Illegal::WrongSize(n),
                Illegal::TooManyOfAName { name, count } => w::Illegal::TooManyOfAName((name, count)),
                Illegal::NoBasicPokemon => w::Illegal::NoBasicPokemon,
                Illegal::ZeroQuantity(id) => w::Illegal::ZeroQuantity(id),
            })
            .collect()
    }

    fn shortfall(
        deck: Vec<w::Slot>,
        owned: Vec<w::Owned>,
        prices: Vec<w::Price>,
        currency: String,
    ) -> w::ShortfallReport {
        let owned: Vec<Owned> =
            owned.iter().map(|o| Owned { card_id: o.card_id.clone(), quantity: o.quantity }).collect();
        let prices: Vec<Price> = prices
            .iter()
            .map(|p| Price {
                card_id: p.card_id.clone(),
                unit_minor: p.unit_minor,
                currency: p.currency.clone(),
            })
            .collect();
        let s = crate::shortfall(&deck_in(&deck), &owned, &prices, &currency);
        w::ShortfallReport {
            missing: s
                .missing
                .into_iter()
                .map(|m| w::Missing {
                    card_id: m.card_id,
                    name: m.name,
                    quantity: m.quantity,
                    cost_minor: m.cost_minor,
                })
                .collect(),
            cost_minor: s.cost_minor,
            currency: s.currency,
            unpriced: s.unpriced,
        }
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);
