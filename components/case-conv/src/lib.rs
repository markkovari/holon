//! `case-conv` — turn an identifier written in one case convention into another.
//!
//! `tests/case.rs` is the specification and is not writable from here. None of
//! the four functions below is implemented yet.
//!
//! Pure compute: split into words, rejoin in the target convention.
//!
//! ## Splitting into words
//!
//! `_`, `-` and whitespace are explicit separators: consumed, never emitted.
//! Inside a run with no separators, a new word starts at:
//!
//!   * a lowercase-to-uppercase transition (`myVar` -> `my`, `Var`);
//!   * the LAST letter of an uppercase run, when followed by a lowercase letter —
//!     the acronym boundary (`HTTPServer` -> `HTTP`, `Server`, not `H`, `TTPServer`
//!     and not `HTTPS`, `erver`);
//!   * a letter/digit transition in either direction (`Sensor2` -> `Sensor`, `2`;
//!     `2Sensors` -> `2`, `Sensors`).
//!
//! Consecutive separators collapse (`foo__bar` is two words, not three with an
//! empty one in the middle), and leading/trailing separators produce no empty
//! word (`_foo_` is one word).
//!
//! ## Rejoining
//!
//!   * `snake`: every word lowercased, joined with `_`.
//!   * `kebab`: every word lowercased, joined with `-`.
//!   * `camel`: first word lowercased, every other word Capitalized (first
//!     letter upper, rest lower — so an acronym gets normalized: `HTTP` becomes
//!     `Http`), no separator.
//!   * `pascal`: every word Capitalized, no separator.
//!
//! An empty string is empty in every convention — there is nothing to split.

/// Split `s` into words per the rules above.
///
/// A single left-to-right scan: separators flush the current word and are
/// dropped; everywhere else a one-character lookbehind (plus a one-character
/// lookahead just for the acronym case) decides whether a boundary falls
/// right before the current character.
fn words(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut result = Vec::new();
    let mut cur = String::new();

    for i in 0..chars.len() {
        let c = chars[i];
        if c == '_' || c == '-' || c.is_whitespace() {
            if !cur.is_empty() {
                result.push(std::mem::take(&mut cur));
            }
            continue;
        }

        if i > 0 {
            let prev = chars[i - 1];
            let is_sep = prev == '_' || prev == '-' || prev.is_whitespace();
            let boundary = !is_sep
                && ((prev.is_lowercase() && c.is_uppercase())
                    || (prev.is_uppercase()
                        && c.is_uppercase()
                        && chars.get(i + 1).is_some_and(|n| n.is_lowercase()))
                    || (prev.is_alphabetic() && c.is_numeric())
                    || (prev.is_numeric() && c.is_alphabetic()));
            if boundary && !cur.is_empty() {
                result.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        result.push(cur);
    }
    result
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
    }
}

/// `s` split into words and rejoined as `snake_case`.
pub fn to_snake(s: &str) -> String {
    words(s)
        .iter()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join("_")
}

/// `s` split into words and rejoined as `kebab-case`.
pub fn to_kebab(s: &str) -> String {
    words(s)
        .iter()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join("-")
}

/// `s` split into words and rejoined as `camelCase`.
pub fn to_camel(s: &str) -> String {
    let ws = words(s);
    let mut out = String::new();
    for (i, w) in ws.iter().enumerate() {
        if i == 0 {
            out.push_str(&w.to_lowercase());
        } else {
            out.push_str(&capitalize(w));
        }
    }
    out
}

/// `s` split into words and rejoined as `PascalCase`.
pub fn to_pascal(s: &str) -> String {
    words(s).iter().map(|w| capitalize(w)).collect()
}

// ---- the component -----------------------------------------------------
//
// A mapping between the WIT types and the ones above. `tests/case.rs` judges the
// plain functions; this adds no behaviour, which is the only way that specification
// keeps covering what actually ships.

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

#[cfg(target_arch = "wasm32")]
use bindings::exports::case::conv::conv as w;

#[cfg(target_arch = "wasm32")]
struct Component;

#[cfg(target_arch = "wasm32")]
impl w::Guest for Component {
    fn to_snake(s: String) -> String {
        crate::to_snake(&s)
    }
    fn to_kebab(s: String) -> String {
        crate::to_kebab(&s)
    }
    fn to_camel(s: String) -> String {
        crate::to_camel(&s)
    }
    fn to_pascal(s: String) -> String {
        crate::to_pascal(&s)
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);
