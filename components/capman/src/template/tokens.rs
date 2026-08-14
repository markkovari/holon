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
    let mut out = Vec::new();
    let mut rest = s;

    while let Some(open) = rest.find("{{") {
        let after_open = &rest[open + 2..];
        match after_open.find("}}") {
            Some(close) => {
                if open > 0 {
                    out.push(Token::Literal(&rest[..open]));
                }
                out.push(Token::Placeholder(after_open[..close].trim()));
                rest = &after_open[close + 2..];
            }
            None => break,
        }
    }

    if !rest.is_empty() {
        out.push(Token::Literal(rest));
    }
    out
}
