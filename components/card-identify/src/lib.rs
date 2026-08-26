//! `card-identify` — read a photo of a Pokémon card into its name, set, number and condition, and say which parts are a guess
//!
//! `tests/guess.rs` is the specification and is not writable from here.
//!
//! Nobody types a card in. They photograph it, a vision model describes what it
//! sees, and this turns that answer into the typed fields an app can store —
//! along with the list of fields a person should check, because half of what
//! makes a card valuable is invisible at photo resolution.
//!
//! This crate is the DETERMINISTIC half: model answer in, typed guess out. The
//! vision call itself is a provider (the shape `components/photo-critic` already
//! proves: egress, key from the vault, an image block), and it is deliberately
//! not here — a model call cannot be gated, and this can.
//!
//! ## Never invent a field
//!
//! The expensive failure is not a wrong guess, it is a CONFIDENT wrong guess: a
//! blank or defaulted field that looks entered. A collection where 300 cards
//! silently say "Near Mint" because that was the default is worth an unknown
//! amount of money, and no screen will ever show you which 300.
//!
//! So an absent field stays absent and its name goes in `needs_review`. The app's
//! job is to ask; this crate's job is to know what to ask about.
//!
//! ## Refusing is a valid answer
//!
//! A photo of a hand, a booster wrapper, or two cards at once must come back as
//! an error and not as a card with empty fields. The blank-card row is how a
//! collection quietly fills with garbage nobody deletes.
//!
//! ## Normalising is not guessing
//!
//! `58/165`, `058/165` and `#58` are the same card number written three ways, and
//! collapsing them is a lookup requirement, not an inference — a price source
//! keyed on `058/165` finds nothing for `#58`. Normalisation is applied and does
//! not lower confidence. Anything that needs a fact not on the card — the set
//! total when the photo cut it off, the language when there is no Japanese text
//! to see — is inference, and lowers it.

/// How the card is printed. Drives price more than anything except condition: a
/// reverse holo and a normal copy of one card are different markets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Normal,
    Holo,
    ReverseHolo,
    FirstEdition,
    Shadowless,
    /// Full art, alt art, secret rare — the "looks nothing like the base print"
    /// bucket. Kept coarse on purpose: the fine distinctions are set-specific and
    /// a photo often cannot settle them.
    Special,
}

/// Condition, on the scale the singles market actually uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    Mint,
    NearMint,
    LightlyPlayed,
    ModeratelyPlayed,
    HeavilyPlayed,
    Damaged,
}

/// A professional grade, when the card is in a slab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grade {
    /// `PSA`, `BGS`, `CGC`, uppercased.
    pub grader: String,
    /// Tenths, so BGS 9.5 is 95 and PSA 10 is 100. Integer, because a grade is
    /// the whole basis of the price and a float here would round.
    pub tenths: u16,
}

/// What the model thought it saw.
///
/// Empty string means "not established" for every text field. There is no
/// sentinel and no default: see the module header.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Guess {
    pub name: String,
    /// Human-readable, as printed: `Obsidian Flames`.
    pub set_name: String,
    /// The lookup key, lowercased: `sv3`, `base1`, `sv3pt5`.
    pub set_code: String,
    /// Zero-padded to the set total when it is known: `058/165`. Bare when it is
    /// not: `58`.
    pub number: String,
    pub rarity: String,
    /// ISO-639-1, lowercased. Empty when nothing on the card settles it.
    pub language: String,
    pub variant: Option<Variant>,
    pub condition: Option<Condition>,
    pub graded: Option<Grade>,
    /// 0..=100, the model's own confidence in the identification as a whole.
    pub confidence: u8,
    /// Field names a person should check, sorted, no duplicates. Every absent
    /// field is here, plus any the model flagged itself.
    pub needs_review: Vec<String>,
}

/// Why no guess could be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentifyError {
    /// The model says there is no card in the picture.
    NoCard(String),
    /// More than one card is visible. One photo, one card — otherwise the app
    /// cannot know which one the fields describe.
    MoreThanOneCard,
    /// The model declined, or answered with nothing usable.
    Refused(String),
    /// The answer carried no JSON object this crate could read.
    Unparseable(String),
    /// The answer was JSON but had no name in it, which is the one field that
    /// cannot be reviewed into existence later.
    NoName,
}

fn extract_json(answer: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str(answer) {
        return Some(v);
    }
    let start = answer.find('{')?;
    let bytes = answer.as_bytes();
    let mut depth = 0i32;
    let mut end = None;
    for (i, &b) in bytes[start..].iter().enumerate() {
        if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                end = Some(start + i);
                break;
            }
        }
    }
    let end = end?;
    serde_json::from_str(&answer[start..=end]).ok()
}

fn str_field(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Returns (normalised number, needs review).
fn normalize_number(raw: Option<String>) -> (String, bool) {
    match raw {
        None => (String::new(), true),
        Some(s) => {
            let cleaned: String = s.chars().filter(|c| *c != '#' && *c != ' ').collect();
            if let Some((num, total)) = cleaned.split_once('/') {
                if !num.is_empty()
                    && !total.is_empty()
                    && num.chars().all(|c| c.is_ascii_digit())
                    && total.chars().all(|c| c.is_ascii_digit())
                {
                    let width = total.len();
                    return (format!("{:0>width$}/{}", num, total, width = width), false);
                }
            }
            (cleaned, true)
        }
    }
}

fn parse_variant(s: &str) -> Option<Variant> {
    match s.to_lowercase().trim() {
        "holo" | "holofoil" => Some(Variant::Holo),
        "reverse holo" | "reverse holofoil" => Some(Variant::ReverseHolo),
        "1st edition" | "first edition" => Some(Variant::FirstEdition),
        "shadowless" => Some(Variant::Shadowless),
        "normal" => Some(Variant::Normal),
        "full art" | "alt art" | "secret rare" => Some(Variant::Special),
        _ => None,
    }
}

fn parse_condition(s: &str) -> Option<Condition> {
    match s.to_lowercase().trim() {
        "mint" | "m" => Some(Condition::Mint),
        "near mint" | "nm" => Some(Condition::NearMint),
        "lightly played" | "lp" => Some(Condition::LightlyPlayed),
        "moderately played" | "mp" => Some(Condition::ModeratelyPlayed),
        "heavily played" | "hp" => Some(Condition::HeavilyPlayed),
        "damaged" | "dmg" => Some(Condition::Damaged),
        _ => None,
    }
}

fn parse_graded(v: &serde_json::Value) -> Option<Grade> {
    match v {
        serde_json::Value::String(s) => {
            let s = s.trim();
            let idx = s.find(|c: char| c.is_ascii_digit())?;
            let grader = s[..idx].trim().to_uppercase();
            let val: f64 = s[idx..].trim().parse().ok()?;
            if grader.is_empty() {
                return None;
            }
            Some(Grade { grader, tenths: (val * 10.0).round() as u16 })
        }
        serde_json::Value::Object(o) => {
            let grader = o.get("grader")?.as_str()?.trim().to_uppercase();
            let grade = o.get("grade")?.as_f64()?;
            if grader.is_empty() {
                return None;
            }
            Some(Grade { grader, tenths: (grade * 10.0).round() as u16 })
        }
        _ => None,
    }
}

/// Read a vision model's answer into a typed guess.
///
/// `answer` is whatever the model returned: bare JSON, JSON in a fenced block,
/// or JSON with prose either side of it.
pub fn parse(answer: &str) -> Result<Guess, IdentifyError> {
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        return Err(IdentifyError::Unparseable(answer.to_string()));
    }
    let value = extract_json(trimmed).ok_or_else(|| IdentifyError::Unparseable(answer.to_string()))?;
    let obj = value.as_object().ok_or_else(|| IdentifyError::Unparseable(answer.to_string()))?;

    if obj.get("no_card").and_then(|v| v.as_bool()).unwrap_or(false) {
        let reason = obj.get("reason").and_then(|v| v.as_str()).unwrap_or("").to_string();
        return Err(IdentifyError::NoCard(reason));
    }
    if obj.get("cards_visible").and_then(|v| v.as_u64()).unwrap_or(0) > 1 {
        return Err(IdentifyError::MoreThanOneCard);
    }

    let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if name.is_empty() {
        return Err(IdentifyError::NoName);
    }

    let confidence = match obj.get("confidence").and_then(|v| v.as_i64()) {
        None => 0u8,
        Some(c) if (0..=100).contains(&c) => c as u8,
        Some(c) => return Err(IdentifyError::Refused(format!("confidence {c} is out of range 0..=100"))),
    };

    let set_name = str_field(obj, "set_name").unwrap_or_default();
    let set_code = str_field(obj, "set_code").map(|s| s.to_lowercase()).unwrap_or_default();
    let rarity = str_field(obj, "rarity").unwrap_or_default();
    let language = str_field(obj, "language").map(|s| s.to_lowercase()).unwrap_or_default();

    let (number, number_flag) = normalize_number(str_field(obj, "number"));

    let (variant, variant_flag) = match str_field(obj, "variant") {
        None => (None, true),
        Some(s) => match parse_variant(&s) {
            Some(v) => (Some(v), false),
            None => (None, true),
        },
    };

    let graded = obj.get("graded").and_then(parse_graded);
    let graded_present = obj.get("graded").is_some();
    let (condition, condition_flag) = if graded_present {
        (None, false)
    } else {
        match str_field(obj, "condition") {
            None => (None, true),
            Some(s) => match parse_condition(&s) {
                Some(c) => (Some(c), false),
                None => (None, true),
            },
        }
    };

    let mut needs_review: Vec<String> = Vec::new();
    if number_flag {
        needs_review.push("number".to_string());
    }
    if rarity.is_empty() {
        needs_review.push("rarity".to_string());
    }
    if language.is_empty() {
        needs_review.push("language".to_string());
    }
    if variant_flag {
        needs_review.push("variant".to_string());
    }
    if condition_flag {
        needs_review.push("condition".to_string());
    }
    if let Some(uncertain) = obj.get("uncertain").and_then(|v| v.as_array()) {
        for item in uncertain {
            if let Some(s) = item.as_str() {
                needs_review.push(s.to_string());
            }
        }
    }
    needs_review.sort();
    needs_review.dedup();

    Ok(Guess {
        name,
        set_name,
        set_code,
        number,
        rarity,
        language,
        variant,
        condition,
        graded,
        confidence,
        needs_review,
    })
}

/// The prompt the vision provider should send, so the shape this parses and the
/// shape the model is asked for cannot drift apart.
///
/// Lives here rather than in the provider for exactly that reason: they are one
/// decision, and a prompt in another crate is a second place to change.
pub fn prompt() -> &'static str {
    r#"Look at this photo of a trading card and answer with a single JSON object, nothing else.

If there is no card in the photo, reply with {"no_card": true, "reason": "..."}.
If more than one card is visible, reply with {"cards_visible": <count>}.

Otherwise reply with these fields, using an empty string or omitting a field you cannot establish from the photo (never guess or default one):
{
  "name": "the card's name",
  "set_name": "the set name as printed",
  "set_code": "the set's lookup code",
  "number": "the card number as printed, e.g. 58/165 or #58",
  "rarity": "the rarity as printed",
  "language": "the ISO-639-1 language code",
  "variant": "normal | holo | reverse holo | 1st edition | shadowless | full art | alt art | secret rare",
  "condition": "mint | near mint | lightly played | moderately played | heavily played | damaged",
  "graded": "e.g. PSA 10, or omit if not slabbed",
  "confidence": 0-100,
  "uncertain": ["field names you are not sure about"]
}"#
}
