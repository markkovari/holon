//! Render a template by resolving each placeholder against `vars`.

use super::tokens::{tokenize, Token};

/// Render `tmpl`, replacing `{{key}}` with its value from `vars` (first match).
/// An unknown key is left VERBATIM as `{{key}}` (with its original spacing lost —
/// re-emitted as `{{key}}` trimmed). Unmatched `{{` stays literal.
pub fn render(tmpl: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for tok in tokenize(tmpl) {
        match tok {
            Token::Literal(text) => out.push_str(text),
            Token::Placeholder(key) => match vars.iter().find(|(k, _)| *k == key) {
                Some((_, v)) => out.push_str(v),
                None => {
                    out.push_str("{{");
                    out.push_str(key);
                    out.push_str("}}");
                }
            },
        }
    }
    out
}
