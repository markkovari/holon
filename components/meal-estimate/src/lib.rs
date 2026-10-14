//! `meal-estimate` — read a vision model's description of a meal into numbers, and say which of
//! them a person should check.
//!
//! The deterministic half of photographing a meal, the same split as `card-identify`: the vision
//! call is a provider's (`vision:describe`), and this owns the PROMPT and the PARSER, so the
//! shape the model is asked for and the shape that is read are one decision.
//!
//! A photo supports a rough estimate and never a precise one, and a model's arithmetic is the
//! weakest thing about it. So the sums are done here, the numbers are clamped to what a plate
//! can hold, and anything that does not hang together (calories that disagree with the macros,
//! a portion no plate carries) goes into `unsure` for a person to look at, next to whatever the
//! model said it was unsure of itself.

use serde_json::Value;

/// A number a plate cannot exceed in one item; past it the reading is clamped, not believed.
const MAX_NUMBER: u32 = 100_000;
/// A single item above this is flagged, not refused: a big bowl of rice is real, a kilo of it
/// on one plate is a misreading of the scale.
const BIG_PORTION_G: u32 = 1_500;
const BIG_ITEM_KCAL: u32 = 2_500;
const BIG_MEAL_KCAL: u32 = 4_000;
const MAX_UNSURE: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub name: String,
    pub grams: u32,
    pub kcal: u32,
    pub protein_g: u32,
    pub carbs_g: u32,
    pub fat_g: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Estimate {
    pub items: Vec<Item>,
    pub total_kcal: u32,
    pub total_protein_g: u32,
    pub total_carbs_g: u32,
    pub total_fat_g: u32,
    pub confidence: Confidence,
    pub unsure: Vec<String>,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    NotJson(String),
    NoItems,
    Malformed(String),
}

/// The prompt whose answer [`parse`] reads. `note` is what the person said about the meal.
pub fn prompt(note: &str) -> String {
    let mut p = String::from(
        "You are estimating the nutrition of a meal from a photo, for an athlete who tracks food.\n\
         Reply with ONE JSON object and nothing else:\n\
         {\"items\": [{\"name\": str, \"grams\": int, \"kcal\": int, \"protein_g\": int, \"carbs_g\": int, \"fat_g\": int}],\n \
         \"confidence\": \"low\" | \"medium\" | \"high\",\n \
         \"unsure\": [str],\n \
         \"notes\": str}\n\
         Rules: list each distinct food you can see, with a portion in COOKED weight judged from the plate \
         (a dinner plate is about 26 cm), including visible cooking fat, skin and sauces. Do not invent items \
         you cannot see. If something is hidden or ambiguous, say so in \"unsure\" and lower the confidence. \
         Use plain numbers, no ranges. Never claim precision: a photo only supports a rough estimate.",
    );
    let note: String = note.split_whitespace().collect::<Vec<_>>().join(" ");
    if !note.is_empty() {
        let note: String = note.chars().take(300).collect();
        p.push_str(&format!("\nThe athlete says: {note}"));
    }
    p
}

/// The JSON object in a model's answer, however it was dressed: bare, in a code fence, or after
/// a sentence of preamble.
fn object_in(answer: &str) -> Option<&str> {
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    (end > start).then(|| &answer[start..=end])
}

/// A number the way models write them: a JSON number, or text with digits in it ("150", "150g",
/// "about 150"). Negative or missing is 0, and the second value says it was not clean.
fn number(v: &Value) -> (u32, bool) {
    let raw = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let digits: String = s
                .trim()
                .chars()
                .skip_while(|c| !c.is_ascii_digit() && *c != '-')
                .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
                .collect();
            digits.parse::<f64>().ok()
        }
        _ => None,
    };
    match raw {
        Some(f) if f.is_finite() && f >= 0.0 => {
            (f.round().min(f64::from(MAX_NUMBER)) as u32, f <= f64::from(MAX_NUMBER))
        }
        _ => (0, false),
    }
}

fn clip(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

/// A vision model's answer, as an estimate.
pub fn parse(answer: &str) -> Result<Estimate, ParseError> {
    let json = object_in(answer)
        .ok_or_else(|| ParseError::NotJson(clip(&answer.replace('\n', " "), 80)))?;
    let v: Value = serde_json::from_str(json).map_err(|e| ParseError::NotJson(e.to_string()))?;
    let listed = v["items"]
        .as_array()
        .ok_or_else(|| ParseError::Malformed("there is no `items` list".into()))?;
    if listed.is_empty() {
        return Err(ParseError::NoItems);
    }

    let mut unsure: Vec<String> = Vec::new();
    let mut items = Vec::new();
    for raw in listed {
        let name = clip(raw["name"].as_str().unwrap_or_default(), 80);
        if name.is_empty() {
            return Err(ParseError::Malformed("an item has no name".into()));
        }
        let mut clean = true;
        let mut get = |k: &str| {
            let (n, ok) = number(&raw[k]);
            clean &= ok;
            n
        };
        let item = Item {
            grams: get("grams"),
            kcal: get("kcal"),
            protein_g: get("protein_g"),
            carbs_g: get("carbs_g"),
            fat_g: get("fat_g"),
            name,
        };
        if !clean {
            unsure.push(format!(
                "{}: a number was missing or unreadable and was taken as 0",
                item.name
            ));
        }
        if item.grams == 0 && item.kcal > 0 {
            unsure.push(format!("{}: no portion was given", item.name));
        }
        if item.grams > BIG_PORTION_G {
            unsure.push(format!(
                "{}: {} g is a very large portion for one plate",
                item.name, item.grams
            ));
        }
        if item.kcal > BIG_ITEM_KCAL {
            unsure.push(format!("{}: {} kcal is a lot for one item", item.name, item.kcal));
        }
        // Atwater factors: the calories a food's macros add up to. A model's own sums drift,
        // and a big disagreement means one of the numbers is wrong without saying which.
        let expected = 4 * item.protein_g + 4 * item.carbs_g + 9 * item.fat_g;
        if expected > 0 && item.kcal > 0 {
            let gap = item.kcal.abs_diff(expected);
            if gap * 100 > 35 * item.kcal.max(expected) + 3_000 {
                unsure.push(format!(
                    "{}: {} kcal does not match its macros (they add up to about {expected})",
                    item.name, item.kcal
                ));
            }
        }
        items.push(item);
    }

    let sum = |f: fn(&Item) -> u32| items.iter().map(f).fold(0u32, u32::saturating_add);
    let (total_kcal, total_protein_g, total_carbs_g, total_fat_g) =
        (sum(|i| i.kcal), sum(|i| i.protein_g), sum(|i| i.carbs_g), sum(|i| i.fat_g));
    if total_kcal > BIG_MEAL_KCAL {
        unsure.push(format!("{total_kcal} kcal is a very large meal; check the portions"));
    }

    let confidence =
        match v["confidence"].as_str().map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("high") => Confidence::High,
            Some("medium") => Confidence::Medium,
            Some("low") => Confidence::Low,
            _ => {
                unsure.push("the model gave no clear confidence, so it is taken as low".into());
                Confidence::Low
            }
        };

    // What the model said it was unsure of comes first: it is the better-informed of the two.
    let mut said: Vec<String> = v["unsure"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| clip(s, 200))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    said.append(&mut unsure);
    said.truncate(MAX_UNSURE);

    Ok(Estimate {
        items,
        total_kcal,
        total_protein_g,
        total_carbs_g,
        total_fat_g,
        confidence,
        unsure: said,
        notes: clip(v["notes"].as_str().unwrap_or_default(), 600),
    })
}

/// The wasm side: the generated bindings and the one export. Compiled only for the component, so
/// the pure core above and its tests link on the host, where the component exports do not exist
/// (the same arrangement as `card-identify`).
#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

#[cfg(target_arch = "wasm32")]
mod guest {
    use super::*;
    use crate::bindings::exports::nutrition::meal::estimator as w;

    struct Component;

    impl w::Guest for Component {
        fn prompt(note: String) -> String {
            prompt(&note)
        }

        fn parse(answer: String) -> Result<w::Estimate, w::ParseError> {
            match parse(&answer) {
                Ok(e) => Ok(w::Estimate {
                    items: e
                        .items
                        .into_iter()
                        .map(|i| w::Item {
                            name: i.name,
                            grams: i.grams,
                            kcal: i.kcal,
                            protein_g: i.protein_g,
                            carbs_g: i.carbs_g,
                            fat_g: i.fat_g,
                        })
                        .collect(),
                    total_kcal: e.total_kcal,
                    total_protein_g: e.total_protein_g,
                    total_carbs_g: e.total_carbs_g,
                    total_fat_g: e.total_fat_g,
                    confidence: match e.confidence {
                        Confidence::Low => w::Confidence::Low,
                        Confidence::Medium => w::Confidence::Medium,
                        Confidence::High => w::Confidence::High,
                    },
                    unsure: e.unsure,
                    notes: e.notes,
                }),
                Err(ParseError::NotJson(m)) => Err(w::ParseError::NotJson(m)),
                Err(ParseError::NoItems) => Err(w::ParseError::NoItems),
                Err(ParseError::Malformed(m)) => Err(w::ParseError::Malformed(m)),
            }
        }
    }

    crate::bindings::export!(Component with_types_in crate::bindings);
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLATE: &str = r#"{"items":[
        {"name":"roasted chicken thigh with skin","grams":150,"kcal":320,"protein_g":30,"carbs_g":0,"fat_g":21},
        {"name":"cooked white rice","grams":120,"kcal":155,"protein_g":3,"carbs_g":34,"fat_g":0},
        {"name":"steamed broccoli","grams":60,"kcal":20,"protein_g":2,"carbs_g":4,"fat_g":0}],
        "confidence":"Medium","unsure":["cooking oil amounts"],"notes":"rough estimate"}"#;

    #[test]
    fn a_plate_is_read_and_the_sums_are_done_here() {
        let e = parse(PLATE).unwrap();
        assert_eq!(e.items.len(), 3);
        assert_eq!(
            (e.total_kcal, e.total_protein_g, e.total_carbs_g, e.total_fat_g),
            (495, 35, 38, 21)
        );
        assert_eq!(e.confidence, Confidence::Medium);
        assert_eq!(e.unsure, ["cooking oil amounts"], "what the model said, and nothing invented");
        assert_eq!(e.notes, "rough estimate");
    }

    #[test]
    fn however_the_answer_is_dressed_the_object_is_found() {
        let fenced = format!("Here you go:\n```json\n{PLATE}\n```\nHope that helps!");
        assert_eq!(parse(&fenced).unwrap().total_kcal, 495);
        assert!(
            matches!(parse("I cannot tell what this is."), Err(ParseError::NotJson(m)) if m.contains("cannot tell"))
        );
        assert!(matches!(parse("{ not json }"), Err(ParseError::NotJson(_))));
    }

    #[test]
    fn numbers_are_read_the_way_models_write_them() {
        let a = r#"{"items":[{"name":"rice","grams":"150g","kcal":"about 200","protein_g":3.6,"carbs_g":44,"fat_g":0}],"confidence":"low"}"#;
        let e = parse(a).unwrap();
        let i = &e.items[0];
        assert_eq!(
            (i.grams, i.kcal, i.protein_g, i.carbs_g),
            (150, 200, 4, 44),
            "text with digits, and floats rounded"
        );
    }

    #[test]
    fn a_missing_or_negative_number_is_zero_and_is_flagged() {
        let a = r#"{"items":[{"name":"soup","grams":300,"kcal":-5,"protein_g":null,"carbs_g":10,"fat_g":2}],"confidence":"high"}"#;
        let e = parse(a).unwrap();
        assert_eq!((e.items[0].kcal, e.items[0].protein_g), (0, 0));
        assert!(
            e.unsure.iter().any(|u| u.contains("soup") && u.contains("unreadable")),
            "{:?}",
            e.unsure
        );
    }

    #[test]
    fn calories_that_disagree_with_the_macros_are_flagged_and_honest_ones_are_not() {
        // 4*30 + 4*0 + 9*21 = 309 against 320: fine
        assert!(!parse(PLATE).unwrap().unsure.iter().any(|u| u.contains("match")));
        // 900 kcal from 10g of protein is not food
        let a = r#"{"items":[{"name":"mystery","grams":100,"kcal":900,"protein_g":10,"carbs_g":0,"fat_g":0}],"confidence":"low"}"#;
        let e = parse(a).unwrap();
        assert!(
            e.unsure.iter().any(|u| u.contains("mystery") && u.contains("does not match")),
            "{:?}",
            e.unsure
        );
    }

    #[test]
    fn a_portion_no_plate_carries_is_flagged_not_refused() {
        let a = r#"{"items":[{"name":"rice","grams":2500,"kcal":3000,"protein_g":60,"carbs_g":650,"fat_g":5}],"confidence":"low"}"#;
        let e = parse(a).unwrap();
        assert_eq!(e.items[0].grams, 2500, "kept as read");
        assert!(e.unsure.iter().any(|u| u.contains("very large portion")));
        assert!(e.unsure.iter().any(|u| u.contains("a lot for one item")));
        let none = r#"{"items":[{"name":"bite","grams":0,"kcal":50,"protein_g":1,"carbs_g":10,"fat_g":0}],"confidence":"low"}"#;
        assert!(parse(none).unwrap().unsure.iter().any(|u| u.contains("no portion")));
    }

    #[test]
    fn an_unclear_confidence_is_low_and_says_so() {
        let a = r#"{"items":[{"name":"rice","grams":100,"kcal":130,"protein_g":3,"carbs_g":28,"fat_g":0}],"confidence":"very sure"}"#;
        let e = parse(a).unwrap();
        assert_eq!(e.confidence, Confidence::Low);
        assert!(e.unsure.iter().any(|u| u.contains("confidence")));
    }

    #[test]
    fn wrong_shapes_are_refused_with_a_reason() {
        assert_eq!(parse(r#"{"items":[]}"#), Err(ParseError::NoItems));
        assert!(
            matches!(parse(r#"{"foods":[]}"#), Err(ParseError::Malformed(m)) if m.contains("items"))
        );
        assert!(
            matches!(parse(r#"{"items":[{"grams":5}]}"#), Err(ParseError::Malformed(m)) if m.contains("no name"))
        );
    }

    #[test]
    fn the_unsure_list_is_bounded_and_the_models_own_words_come_first() {
        let many: Vec<String> = (0..30).map(|i| format!("\"thing {i}\"")).collect();
        let a = format!(
            r#"{{"items":[{{"name":"x","grams":10,"kcal":10,"protein_g":1,"carbs_g":1,"fat_g":0}}],"confidence":"low","unsure":[{}]}}"#,
            many.join(",")
        );
        let e = parse(&a).unwrap();
        assert_eq!(e.unsure.len(), MAX_UNSURE);
        assert_eq!(e.unsure[0], "thing 0");
    }

    #[test]
    fn the_prompt_names_the_shape_that_parse_reads_and_carries_the_persons_note() {
        let p = prompt("  post-row \n lunch  ");
        for key in [
            "items",
            "grams",
            "kcal",
            "protein_g",
            "carbs_g",
            "fat_g",
            "confidence",
            "unsure",
            "notes",
        ] {
            assert!(p.contains(key), "the prompt asks for {key}");
        }
        assert!(p.ends_with("The athlete says: post-row lunch"), "{p}");
        assert!(!prompt("").contains("The athlete says"));
        assert!(prompt(&"x".repeat(1000)).len() < prompt("").len() + 340);
    }
}
