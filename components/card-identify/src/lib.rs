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

/// Read a vision model's answer into a typed guess.
///
/// `answer` is whatever the model returned: bare JSON, JSON in a fenced block,
/// or JSON with prose either side of it.
pub fn parse(_answer: &str) -> Result<Guess, IdentifyError> {
    Err(IdentifyError::Unparseable(String::new()))
}

/// The prompt the vision provider should send, so the shape this parses and the
/// shape the model is asked for cannot drift apart.
///
/// Lives here rather than in the provider for exactly that reason: they are one
/// decision, and a prompt in another crate is a second place to change.
pub fn prompt() -> &'static str {
    ""
}
