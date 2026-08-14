//! Render a template by resolving each placeholder against `vars`.

use super::tokens::{tokenize, Token};

/// Render `tmpl`, replacing `{{key}}` with its value from `vars` (first match).
/// An unknown key is left VERBATIM as `{{key}}` (with its original spacing lost —
/// re-emitted as `{{key}}` trimmed). Unmatched `{{` stays literal.
pub fn render(tmpl: &str, vars: &[(&str, &str)]) -> String {
    let _ = (tokenize, tmpl, vars);
    let _: Option<Token> = None;
    unimplemented!("goal: the conformance in capman is the spec")
}
