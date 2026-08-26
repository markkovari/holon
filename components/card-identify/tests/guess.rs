//! The specification, held out: this file is not writable by the goal.
//!
//! The cases that matter here are not the happy path. They are: a photo that is
//! not a card, a field the model did not establish, and a number written the way
//! a price source cannot look up. Each one, done wrong, puts a row in somebody's
//! collection that looks entered and is fiction.

use card_identify::{parse, prompt, Condition, Grade, IdentifyError, Variant};

/// The happy path, bare JSON.
#[test]
fn a_complete_answer_becomes_a_complete_guess() {
    let g = parse(
        r#"{"name":"Charizard ex","set_name":"Obsidian Flames","set_code":"sv3",
            "number":"125/197","rarity":"Double Rare","language":"en",
            "variant":"holo","condition":"near mint","confidence":88}"#,
    )
    .expect("a guess");
    assert_eq!(g.name, "Charizard ex");
    assert_eq!(g.set_code, "sv3");
    assert_eq!(g.number, "125/197");
    assert_eq!(g.variant, Some(Variant::Holo));
    assert_eq!(g.condition, Some(Condition::NearMint));
    assert_eq!(g.confidence, 88);
    assert!(g.needs_review.is_empty(), "nothing absent, nothing to review: {:?}", g.needs_review);
}

/// Models wrap JSON in a fenced block, and in prose, and in both. All three are
/// the same answer.
#[test]
fn json_is_found_inside_a_fence_or_prose() {
    let bare = r#"{"name":"Pikachu","confidence":50}"#;
    let fenced = format!("Here is what I see:\n\n```json\n{bare}\n```\n\nHope that helps.");
    let prose = format!("I think this is a Pikachu. {bare} Let me know if you want more detail.");
    let a = parse(bare).expect("bare");
    let b = parse(&fenced).expect("fenced");
    let c = parse(&prose).expect("prose");
    assert_eq!(a.name, "Pikachu");
    assert_eq!(a, b, "a fence is not information");
    assert_eq!(a, c, "prose either side is not information");
}

/// THE dangerous case. Not a card must be an error, never a blank row.
#[test]
fn a_photo_that_is_not_a_card_is_refused() {
    match parse(r#"{"no_card":true,"reason":"this is a booster wrapper"}"#) {
        Err(IdentifyError::NoCard(reason)) => assert!(reason.contains("wrapper"), "{reason}"),
        other => panic!("expected NoCard, got {other:?}"),
    }
    assert!(matches!(parse(""), Err(IdentifyError::Unparseable(_))));
    assert!(matches!(parse("I'm sorry, I can't help with that."), Err(IdentifyError::Unparseable(_))));
}

/// Two cards in frame: the fields cannot describe both, so neither is stored.
#[test]
fn two_cards_in_one_photo_are_refused() {
    assert_eq!(
        parse(r#"{"cards_visible":2,"name":"Pikachu"}"#),
        Err(IdentifyError::MoreThanOneCard)
    );
}

/// A name is the one field a person cannot review into existence — they would
/// have to identify the card themselves, which is the thing they were avoiding.
#[test]
fn an_answer_with_no_name_is_refused() {
    assert_eq!(parse(r#"{"set_code":"sv3","number":"125/197","confidence":90}"#), Err(IdentifyError::NoName));
    assert_eq!(parse(r#"{"name":"   ","confidence":90}"#), Err(IdentifyError::NoName), "whitespace is not a name");
}

/// An absent field stays absent and is listed. Nothing is defaulted — a defaulted
/// condition is money, silently.
#[test]
fn absent_fields_are_listed_and_never_defaulted() {
    let g = parse(r#"{"name":"Pikachu","set_name":"Base","set_code":"base1","confidence":40}"#).expect("a guess");
    assert_eq!(g.condition, None, "NOT Near Mint");
    assert_eq!(g.variant, None);
    assert_eq!(g.number, "");
    assert_eq!(g.language, "");
    assert_eq!(
        g.needs_review,
        vec!["condition", "language", "number", "rarity", "variant"],
        "sorted, and every absent field named"
    );
}

/// The model can flag its own uncertainty, and that adds to the list rather than
/// replacing it.
#[test]
fn a_field_the_model_flags_itself_joins_the_list() {
    let g = parse(
        r#"{"name":"Pikachu","set_name":"Base","set_code":"base1","number":"58/102",
            "rarity":"Common","language":"en","variant":"normal","condition":"near mint",
            "confidence":70,"uncertain":["condition"]}"#,
    )
    .expect("a guess");
    assert_eq!(g.condition, Some(Condition::NearMint), "still recorded");
    assert_eq!(g.needs_review, vec!["condition"], "and still flagged for a person");
}

/// No duplicates, whether a field is absent or flagged or both.
#[test]
fn needs_review_has_no_duplicates() {
    let g = parse(r#"{"name":"Pikachu","confidence":10,"uncertain":["number","number","rarity"]}"#)
        .expect("a guess");
    let mut sorted = g.needs_review.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(g.needs_review, sorted);
}

/// Numbers are normalised, because a price source keyed on `058/165` finds
/// nothing for `#58`. This is a lookup requirement, not inference, so confidence
/// does not move.
#[test]
fn a_card_number_is_normalised_to_the_set_total() {
    for written in ["58/165", "058/165", "58 / 165", "#58/165"] {
        let g = parse(&format!(r#"{{"name":"X","number":"{written}","confidence":80}}"#)).expect(written);
        assert_eq!(g.number, "058/165", "{written} is the same card");
        assert_eq!(g.confidence, 80, "normalising is not guessing");
    }
}

/// With no set total there is nothing to pad to, so the number is kept as
/// written and flagged — padding it to an invented width would produce a key that
/// matches nothing.
#[test]
fn a_number_with_no_set_total_is_kept_bare_and_flagged() {
    let g = parse(r##"{"name":"X","number":"#58","confidence":80}"##).expect("a guess");
    assert_eq!(g.number, "58");
    assert!(g.needs_review.contains(&"number".to_string()), "{:?}", g.needs_review);
}

/// Set codes are the lookup key and arrive in every casing.
#[test]
fn a_set_code_is_lowercased() {
    let g = parse(r#"{"name":"X","set_code":"SV3PT5","confidence":80}"#).expect("a guess");
    assert_eq!(g.set_code, "sv3pt5");
}

/// Variants arrive as whatever the model felt like writing.
#[test]
fn variants_are_read_from_free_text() {
    for (written, expected) in [
        ("holo", Variant::Holo),
        ("Holofoil", Variant::Holo),
        ("reverse holo", Variant::ReverseHolo),
        ("Reverse Holofoil", Variant::ReverseHolo),
        ("1st Edition", Variant::FirstEdition),
        ("first edition", Variant::FirstEdition),
        ("shadowless", Variant::Shadowless),
        ("normal", Variant::Normal),
        ("full art", Variant::Special),
        ("alt art", Variant::Special),
        ("secret rare", Variant::Special),
    ] {
        let g = parse(&format!(r#"{{"name":"X","variant":"{written}","confidence":80}}"#)).expect(written);
        assert_eq!(g.variant, Some(expected), "variant {written:?}");
    }
}

/// A variant nobody recognises is not silently Normal — that is a price
/// difference — it is unknown and flagged.
#[test]
fn an_unrecognised_variant_is_unknown_rather_than_normal() {
    let g = parse(r#"{"name":"X","variant":"gold crown tera something","confidence":80}"#).expect("a guess");
    assert_eq!(g.variant, None);
    assert!(g.needs_review.contains(&"variant".to_string()));
}

#[test]
fn conditions_are_read_from_the_words_the_market_uses() {
    for (written, expected) in [
        ("mint", Condition::Mint),
        ("M", Condition::Mint),
        ("near mint", Condition::NearMint),
        ("NM", Condition::NearMint),
        ("nm", Condition::NearMint),
        ("lightly played", Condition::LightlyPlayed),
        ("LP", Condition::LightlyPlayed),
        ("moderately played", Condition::ModeratelyPlayed),
        ("MP", Condition::ModeratelyPlayed),
        ("heavily played", Condition::HeavilyPlayed),
        ("HP", Condition::HeavilyPlayed),
        ("damaged", Condition::Damaged),
        ("DMG", Condition::Damaged),
    ] {
        let g = parse(&format!(r#"{{"name":"X","condition":"{written}","confidence":80}}"#)).expect(written);
        assert_eq!(g.condition, Some(expected), "condition {written:?}");
    }
}

/// A slab is a different market from a raw card, and the grade IS the price.
#[test]
fn a_graded_card_carries_its_grader_and_grade_in_tenths() {
    let psa = parse(r#"{"name":"X","graded":"PSA 10","confidence":95}"#).expect("psa");
    assert_eq!(psa.graded, Some(Grade { grader: "PSA".into(), tenths: 100 }));

    let bgs = parse(r#"{"name":"X","graded":"bgs 9.5","confidence":95}"#).expect("bgs");
    assert_eq!(bgs.graded, Some(Grade { grader: "BGS".into(), tenths: 95 }), "9.5 is 95 tenths, not 9");

    let cgc = parse(r#"{"name":"X","graded":{"grader":"cgc","grade":8.5},"confidence":95}"#).expect("cgc");
    assert_eq!(cgc.graded, Some(Grade { grader: "CGC".into(), tenths: 85 }), "object form too");
}

/// A graded card is not condition-graded as well: the slab settles it, and
/// carrying both invites two answers to one question.
#[test]
fn a_graded_card_does_not_also_need_a_condition_review() {
    let g = parse(r#"{"name":"X","graded":"PSA 10","confidence":95}"#).expect("a guess");
    assert!(!g.needs_review.contains(&"condition".to_string()), "{:?}", g.needs_review);
}

/// Confidence outside 0..=100 is a broken answer, not something to clamp: a model
/// that says 150 did not understand the question, and clamping hides that.
#[test]
fn confidence_outside_the_range_is_refused() {
    assert!(matches!(parse(r#"{"name":"X","confidence":150}"#), Err(IdentifyError::Refused(_))));
    assert!(matches!(parse(r#"{"name":"X","confidence":-4}"#), Err(IdentifyError::Refused(_))));
}

/// An answer with no confidence at all is treated as no confidence, not as full
/// confidence. Defaulting the other way is how a guess becomes a fact.
#[test]
fn a_missing_confidence_is_zero() {
    let g = parse(r#"{"name":"Pikachu"}"#).expect("a guess");
    assert_eq!(g.confidence, 0);
}

/// The prompt lives beside the parser so the two cannot drift, and it has to name
/// the fields the parser reads.
#[test]
fn the_prompt_asks_for_exactly_what_the_parser_reads() {
    let p = prompt();
    for field in
        ["name", "set_name", "set_code", "number", "rarity", "language", "variant", "condition", "confidence"]
    {
        assert!(p.contains(field), "the prompt never mentions {field:?}, so the model will not send it");
    }
    assert!(p.contains("no_card"), "the model needs a way to say there is no card");
    assert!(p.contains("cards_visible"), "and a way to say there are several");
    assert!(!p.is_empty());
}
