//! The four-copy rule counts names, not printings; Basic Energy is uncapped, Special Energy is not.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardKind {
    BasicPokemon,
    EvolvedPokemon,
    Trainer,
    BasicEnergy,
    SpecialEnergy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub card_id: String,
    /// What the four-copy rule counts.
    pub name: String,
    pub kind: CardKind,
    pub quantity: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owned {
    pub card_id: String,
    pub quantity: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Price {
    pub card_id: String,
    pub unit_minor: i64,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Illegal {
    WrongSize(u32),
    TooManyOfAName { name: String, count: u32 },
    NoBasicPokemon,
    ZeroQuantity(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    pub card_id: String,
    pub name: String,
    pub quantity: u32,
    /// `None` when nothing priced it, which is not free.
    pub cost_minor: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortfall {
    /// Sorted by card id, so the same deck always gives the same list.
    pub missing: Vec<Missing>,
    pub cost_minor: i64,
    pub currency: String,
    /// Not in `cost_minor`: ignoring this quotes a total that is too low.
    pub unpriced: u32,
}

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

/// A price in another currency does not count; its card is `unpriced`.
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
                Illegal::TooManyOfAName { name, count } => {
                    w::Illegal::TooManyOfAName((name, count))
                }
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
        let owned: Vec<Owned> = owned
            .iter()
            .map(|o| Owned { card_id: o.card_id.clone(), quantity: o.quantity })
            .collect();
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
