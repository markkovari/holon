let eur : String = "EUR"

fn slot(id : String, name : String, kind : @builder.CardKind, quantity : UInt) -> @builder.Slot {
  { card_id: id, name, kind, quantity }
}

fn legal_deck() -> Array[@builder.Slot] {
  [
    slot("sv3-001", "Charmander", BASIC_POKEMON, 4U),
    slot("sv3-125", "Charizard ex", EVOLVED_POKEMON, 4U),
    slot("sv3-200", "Professor's Research", TRAINER, 4U),
    slot("sv3-201", "Boss's Orders", TRAINER, 4U),
    slot("sv3-202", "Ultra Ball", TRAINER, 4U),
    slot("sv3-203", "Nest Ball", TRAINER, 4U),
    slot("sv3-204", "Rare Candy", TRAINER, 4U),
    slot("sv3-205", "Switch", TRAINER, 4U),
    slot("sv3-206", "Iono", TRAINER, 4U),
    slot("sv3-207", "Arven", TRAINER, 4U),
    slot("sve-002", "Fire Energy", BASIC_ENERGY, 20U),
  ]
}

fn price(id : String, unit : Int64) -> @builder.Price {
  { card_id: id, unit_minor: unit, currency: eur }
}

test "a sixty card deck with a basic and no fifth copy is legal" {
  let deck = legal_deck()
  let mut total : UInt = 0U
  for s in deck {
    total = total + s.quantity
  }
  assert_eq(total, 60U)
  assert_eq(@builder.legality(deck).is_empty(), true)
}

test "four copies counts names not printings" {
  let deck : Array[@builder.Slot] = []
  for s in legal_deck() {
    if s.name != "Fire Energy" {
      deck.push(s)
    }
  }
  deck.push(slot("base1-046", "Charmander", BASIC_POKEMON, 4U))
  deck.push(slot("sve-002", "Fire Energy", BASIC_ENERGY, 16U))

  let why = @builder.legality(deck)
  assert_eq(why.length(), 1)
  assert_eq(why[0] is TooManyOfAName(("Charmander", 8U)), true)
}

test "basic energy is uncapped" {
  let deck = [
    slot("sv3-001", "Charmander", BASIC_POKEMON, 4U),
    slot("sve-002", "Fire Energy", BASIC_ENERGY, 56U),
  ]
  assert_eq(@builder.legality(deck).is_empty(), true)
}

test "special energy is capped like everything else" {
  let deck = [
    slot("sv3-001", "Charmander", BASIC_POKEMON, 4U),
    slot("sv3-300", "Double Turbo Energy", SPECIAL_ENERGY, 8U),
    slot("sve-002", "Fire Energy", BASIC_ENERGY, 48U),
  ]
  let why = @builder.legality(deck)
  assert_eq(why.length(), 1)
  assert_eq(why[0] is TooManyOfAName(("Double Turbo Energy", 8U)), true)
}

test "a deck is exactly sixty cards" {
  let small = legal_deck()
  small[small.length() - 1] = slot("sve-002", "Fire Energy", BASIC_ENERGY, 19U)
  let why_small = @builder.legality(small)
  assert_eq(why_small.length(), 1)
  assert_eq(why_small[0] is WrongSize(59U), true)

  let big = legal_deck()
  big[big.length() - 1] = slot("sve-002", "Fire Energy", BASIC_ENERGY, 21U)
  let why_big = @builder.legality(big)
  assert_eq(why_big.length(), 1)
  assert_eq(why_big[0] is WrongSize(61U), true)
}

test "a deck needs at least one basic pokemon" {
  let deck = [
    slot("sv3-125", "Charizard ex", EVOLVED_POKEMON, 4U),
    slot("sv3-200", "Professor's Research", TRAINER, 4U),
    slot("sve-002", "Fire Energy", BASIC_ENERGY, 52U),
  ]
  let why = @builder.legality(deck)
  assert_eq(why.length(), 1)
  assert_eq(why[0] is NoBasicPokemon, true)
}

test "every reason is reported not just the first" {
  let deck = [
    slot("sv3-125", "Charizard ex", EVOLVED_POKEMON, 5U),
    slot("sv3-200", "Professor's Research", TRAINER, 4U),
  ]
  let why = @builder.legality(deck)
  assert_eq(why.length(), 3)
}

test "a zero quantity slot is refused" {
  let deck = legal_deck()
  deck.push(slot("sv3-999", "Nothing", TRAINER, 0U))
  let why = @builder.legality(deck)
  assert_eq(why.length() > 0, true)
  let mut found_zero = false
  for item in why {
    if item is ZeroQuantity("sv3-999") {
      found_zero = true
    }
  }
  assert_eq(found_zero, true)
}

test "the shortfall is what you do not already own" {
  let deck = [
    slot("sv3-001", "Charmander", BASIC_POKEMON, 4U),
    slot("sv3-125", "Charizard ex", EVOLVED_POKEMON, 2U),
  ]
  let owned : Array[@builder.Owned] = [{ card_id: "sv3-001", quantity: 3U }]
  let prices = [price("sv3-001", 500L), price("sv3-125", 4000L)]

  let s = @builder.shortfall(deck, owned, prices, eur)
  assert_eq(s.missing.length(), 2)
  assert_eq(s.missing[0].card_id, "sv3-001")
  assert_eq(s.missing[0].quantity, 1U)
  assert_eq(s.missing[0].cost_minor, Some(500L))

  assert_eq(s.missing[1].card_id, "sv3-125")
  assert_eq(s.missing[1].quantity, 2U)
  assert_eq(s.missing[1].cost_minor, Some(8000L))

  assert_eq(s.cost_minor, 8500L)
  assert_eq(s.currency, eur)
  assert_eq(s.unpriced, 0U)
}

test "owning enough leaves nothing to buy" {
  let deck = [slot("sv3-001", "Charmander", BASIC_POKEMON, 2U)]
  let owned : Array[@builder.Owned] = [{ card_id: "sv3-001", quantity: 9U }]
  let s = @builder.shortfall(deck, owned, [price("sv3-001", 500L)], eur)
  assert_eq(s.missing.is_empty(), true)
  assert_eq(s.cost_minor, 0L)
}

test "a missing card with no price is counted not costed" {
  let deck = [
    slot("sv3-001", "Charmander", BASIC_POKEMON, 2U),
    slot("wbsp-009", "Mew", BASIC_POKEMON, 1U),
  ]
  let s = @builder.shortfall(deck, [], [price("sv3-001", 500L)], eur)
  assert_eq(s.cost_minor, 1000L)
  assert_eq(s.unpriced, 1U)
  assert_eq(s.missing[1].card_id, "wbsp-009")
  assert_eq(s.missing[1].cost_minor, None)
}

test "a price in another currency does not count" {
  let deck = [slot("sv3-001", "Charmander", BASIC_POKEMON, 1U)]
  let usd : @builder.Price = { card_id: "sv3-001", unit_minor: 9999L, currency: "USD" }
  let s = @builder.shortfall(deck, [], [usd], eur)
  assert_eq(s.cost_minor, 0L)
  assert_eq(s.unpriced, 1U)
  assert_eq(s.missing[0].cost_minor, None)
}

test "an empty deck needs nothing" {
  let s = @builder.shortfall([], [], [], eur)
  assert_eq(s.missing.is_empty(), true)
  assert_eq(s.cost_minor, 0L)
  assert_eq(s.currency, eur)
  assert_eq(s.unpriced, 0U)
}
