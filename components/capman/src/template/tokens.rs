//! Split a template into literal text and `{{ key }}` placeholders.

#[derive(Debug, PartialEq)]
pub enum Token<'a> {
    /// Verbatim text (includes any `{{` that never closed).
    Literal(&'a str),
    /// A placeholder; the key is already trimmed of surrounding spaces.
    Placeholder(&'a str),
}

/// Tokenize `s`. A `{{` with no matching `}}` is literal text, not a placeholder.
pub fn tokenize(s: &str) -> Vec<Token<'_>> {
    let _ = s;
    unimplemented!("goal: the conformance in capman is the spec")
}
