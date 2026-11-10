// Tests for card:identify/identifier

test "a complete answer becomes a complete guess" {
  let json_str = "{\"name\":\"Charizard ex\",\"set_name\":\"Obsidian Flames\",\"set_code\":\"sv3\",\"number\":\"125/197\",\"rarity\":\"Double Rare\",\"language\":\"en\",\"variant\":\"holo\",\"condition\":\"near mint\",\"confidence\":88}"
  match parse(json_str) {
    Ok(g) => {
      assert_eq(g.name, "Charizard ex")
      assert_eq(g.set_code, "sv3")
      assert_eq(g.number, "125/197")
      assert_true(g.printing == Some(HOLO))
      assert_true(g.condition == Some(NEAR_MINT))
      assert_eq(g.confidence, b'\x58') // 88
      assert_eq(g.needs_review.length(), 0)
    }
    Err(_) => panic()
  }
}

test "json is found inside a fence or prose" {
  let bare = "{\"name\":\"Pikachu\",\"confidence\":50}"
  let fenced = "Here is what I see:\n\n```json\n" + bare + "\n```\n\nHope that helps."
  let prose = "I think this is a Pikachu. " + bare + " Let me know if you want more detail."

  let a = parse(bare)
  let b = parse(fenced)
  let c = parse(prose)

  match (a, b, c) {
    (Ok(ga), Ok(gb), Ok(gc)) => {
      assert_eq(ga.name, "Pikachu")
      assert_true(ga == gb)
      assert_true(ga == gc)
    }
    _ => panic()
  }
}

test "a photo that is not a card is refused" {
  match parse("{\"no_card\":true,\"reason\":\"this is a booster wrapper\"}") {
    Err(NoCard(reason)) => assert_true(reason.contains("wrapper"))
    _ => panic()
  }

  match parse("") {
    Err(Unparseable(_)) => ()
    _ => panic()
  }

  match parse("I'm sorry, I can't help with that.") {
    Err(Unparseable(_)) => ()
    _ => panic()
  }
}

test "two cards in one photo are refused" {
  match parse("{\"cards_visible\":2,\"name\":\"Pikachu\"}") {
    Err(MoreThanOneCard) => ()
    _ => panic()
  }
}

test "an answer with no name is refused" {
  match parse("{\"set_code\":\"sv3\",\"number\":\"125/197\",\"confidence\":90}") {
    Err(NoName) => ()
    _ => panic()
  }

  match parse("{\"name\":\"   \",\"confidence\":90}") {
    Err(NoName) => ()
    _ => panic()
  }
}

test "absent fields are listed and never defaulted" {
  match parse("{\"name\":\"Pikachu\",\"set_name\":\"Base\",\"set_code\":\"base1\",\"confidence\":40}") {
    Ok(g) => {
      assert_true(g.condition == None)
      assert_true(g.printing == None)
      assert_eq(g.number, "")
      assert_eq(g.language, "")
      assert_eq(g.needs_review, ["condition", "language", "number", "rarity", "variant"])
    }
    _ => panic()
  }
}

test "a field the model flags itself joins the list" {
  let json_str = "{\"name\":\"Pikachu\",\"set_name\":\"Base\",\"set_code\":\"base1\",\"number\":\"58/102\",\"rarity\":\"Common\",\"language\":\"en\",\"variant\":\"normal\",\"condition\":\"near mint\",\"confidence\":70,\"uncertain\":[\"condition\"]}"
  match parse(json_str) {
    Ok(g) => {
      assert_true(g.condition == Some(NEAR_MINT))
      assert_eq(g.needs_review, ["condition"])
    }
    _ => panic()
  }
}

test "needs_review has no duplicates" {
  match parse("{\"name\":\"Pikachu\",\"confidence\":10,\"uncertain\":[\"number\",\"number\",\"rarity\"]}") {
    Ok(g) => {
      assert_eq(g.needs_review, ["condition", "language", "number", "rarity", "variant"])
    }
    _ => panic()
  }
}

test "a number is normalized to padded three-digit form" {
  match parse("{\"name\":\"X\",\"number\":\"#58\",\"confidence\":80}") {
    Ok(g) => assert_eq(g.number, "58")
    _ => panic()
  }

  match parse("{\"name\":\"X\",\"number\":\" 58/165 \",\"confidence\":80}") {
    Ok(g) => assert_eq(g.number, "058/165")
    _ => panic()
  }

  match parse("{\"name\":\"X\",\"number\":\"125/197\",\"confidence\":80}") {
    Ok(g) => assert_eq(g.number, "125/197")
    _ => panic()
  }

  match parse("{\"name\":\"X\",\"number\":\"Promo\",\"confidence\":80}") {
    Ok(g) => {
      assert_eq(g.number, "Promo")
      assert_true(g.needs_review.contains("number"))
    }
    _ => panic()
  }
}

test "a set code is lowercased" {
  match parse("{\"name\":\"X\",\"set_code\":\"SV3PT5\",\"confidence\":80}") {
    Ok(g) => assert_eq(g.set_code, "sv3pt5")
    _ => panic()
  }
}

test "variants are read from free text" {
  let cases : Array[(String, VariantKind)] = [
    ("holo", HOLO),
    ("Holofoil", HOLO),
    ("reverse holo", REVERSE_HOLO),
    ("Reverse Holofoil", REVERSE_HOLO),
    ("1st Edition", FIRST_EDITION),
    ("first edition", FIRST_EDITION),
    ("shadowless", SHADOWLESS),
    ("normal", NORMAL),
    ("full art", SPECIAL),
    ("alt art", SPECIAL),
    ("secret rare", SPECIAL),
  ]
  for c in cases {
    let (written, expected) = c
    match parse("{\"name\":\"X\",\"variant\":\"" + written + "\",\"confidence\":80}") {
      Ok(g) => assert_true(g.printing == Some(expected))
      _ => panic()
    }
  }
}

test "an unrecognised variant is unknown rather than normal" {
  match parse("{\"name\":\"X\",\"variant\":\"gold crown tera something\",\"confidence\":80}") {
    Ok(g) => {
      assert_true(g.printing == None)
      assert_true(g.needs_review.contains("variant"))
    }
    _ => panic()
  }
}

test "conditions are read from the words the market uses" {
  let cases : Array[(String, Condition)] = [
    ("mint", MINT),
    ("M", MINT),
    ("near mint", NEAR_MINT),
    ("NM", NEAR_MINT),
    ("nm", NEAR_MINT),
    ("lightly played", LIGHTLY_PLAYED),
    ("LP", LIGHTLY_PLAYED),
    ("moderately played", MODERATELY_PLAYED),
    ("MP", MODERATELY_PLAYED),
    ("heavily played", HEAVILY_PLAYED),
    ("HP", HEAVILY_PLAYED),
    ("damaged", DAMAGED),
    ("DMG", DAMAGED),
  ]
  for c in cases {
    let (written, expected) = c
    match parse("{\"name\":\"X\",\"condition\":\"" + written + "\",\"confidence\":80}") {
      Ok(g) => assert_true(g.condition == Some(expected))
      _ => panic()
    }
  }
}

test "a graded card carries its grader and grade in tenths" {
  match parse("{\"name\":\"X\",\"graded\":\"PSA 10\",\"confidence\":95}") {
    Ok(g) => assert_true(g.graded == Some({ grader: "PSA", tenths: 100U }))
    _ => panic()
  }

  match parse("{\"name\":\"X\",\"graded\":\"bgs 9.5\",\"confidence\":95}") {
    Ok(g) => assert_true(g.graded == Some({ grader: "BGS", tenths: 95U }))
    _ => panic()
  }

  match parse("{\"name\":\"X\",\"graded\":{\"grader\":\"cgc\",\"grade\":8.5},\"confidence\":95}") {
    Ok(g) => assert_true(g.graded == Some({ grader: "CGC", tenths: 85U }))
    _ => panic()
  }
}

test "a graded card does not also need a condition review" {
  match parse("{\"name\":\"X\",\"graded\":\"PSA 10\",\"confidence\":95}") {
    Ok(g) => assert_true(!g.needs_review.contains("condition"))
    _ => panic()
  }
}

test "confidence outside the range is refused" {
  match parse("{\"name\":\"X\",\"confidence\":150}") {
    Err(Refused(_)) => ()
    _ => panic()
  }

  match parse("{\"name\":\"X\",\"confidence\":-4}") {
    Err(Refused(_)) => ()
    _ => panic()
  }
}

test "a missing confidence is zero" {
  match parse("{\"name\":\"Pikachu\"}") {
    Ok(g) => assert_eq(g.confidence, b'\x00')
    _ => panic()
  }
}

test "prompt asks for exactly what the parser reads" {
  let p = prompt()
  for field in [
    "name",
    "set_name",
    "set_code",
    "number",
    "rarity",
    "language",
    "variant",
    "condition",
    "confidence",
  ] {
    assert_true(p.contains(field))
  }
  assert_true(p.contains("no_card"))
  assert_true(p.contains("cards_visible"))
  assert_true(p.length() > 0)
}
