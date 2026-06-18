//! `validate` — reference implementation of `validate:schema`.
//!
//! Declarative input validation: pure compute, no host imports. The caller
//! hands over a JSON object string plus a list of field `rule`s; we parse the
//! JSON once and check every rule against it, collecting *all* failures into a
//! list of `field-error`s. An empty result means the input is valid.
//!
//! Checks per rule, in order: presence (`required`), kind/type, string length
//! (`min-len`/`max-len`, char counts, only when non-zero), format classes
//! (email shape / ASCII-alphanumeric / UUID 8-4-4-4-12), numeric range
//! (`min-value`/`max-value`, inclusive), and enum membership (`one-of`).
//! Unknown extra keys in the JSON are ignored. A non-object or unparseable
//! `json` yields a single `format` error on the empty field.

#[allow(warnings)]
mod bindings;

use bindings::exports::validate::schema::validator::{FieldError, Guest, Kind, Rule};

use serde_json::Value;

struct Component;

/// Build a `field-error` with the given field, machine code, and message.
fn err(field: &str, code: &str, message: impl Into<String>) -> FieldError {
    FieldError {
        field: field.to_string(),
        code: code.to_string(),
        message: message.into(),
    }
}

/// Basic email shape: exactly one '@', non-empty local part, domain contains a
/// '.', and no whitespace anywhere.
fn is_email_shape(s: &str) -> bool {
    if s.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    let mut parts = s.split('@');
    let local = match parts.next() {
        Some(l) => l,
        None => return false,
    };
    let domain = match parts.next() {
        Some(d) => d,
        None => return false,
    };
    // More than one '@' -> a third part exists.
    if parts.next().is_some() {
        return false;
    }
    !local.is_empty() && domain.contains('.')
}

/// 8-4-4-4-12 hex groups separated by hyphens, case-insensitive.
fn is_uuid_shape(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    if groups.len() != 5 {
        return false;
    }
    let expected = [8usize, 4, 4, 4, 12];
    for (g, &len) in groups.iter().zip(expected.iter()) {
        if g.len() != len || !g.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
    }
    true
}

/// Render a JSON scalar to the canonical string used for `one-of` comparison.
/// Returns `None` for values we don't render (objects/arrays/null).
fn one_of_repr(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(if *b { "true".to_string() } else { "false".to_string() }),
        _ => None,
    }
}

/// Check one rule against the field's value (known present and non-null).
/// Pushes any failures onto `out`.
fn check_present(rule: &Rule, value: &Value, out: &mut Vec<FieldError>) {
    let field = rule.field.as_str();

    match rule.kind {
        Kind::Text | Kind::Email | Kind::Alphanumeric | Kind::Uuid => {
            let s = match value.as_str() {
                Some(s) => s,
                None => {
                    out.push(err(field, "type", "expected a string"));
                    return;
                }
            };

            let char_len = s.chars().count() as u32;
            if rule.min_len != 0 && char_len < rule.min_len {
                out.push(err(
                    field,
                    "min-len",
                    format!("must be at least {} characters", rule.min_len),
                ));
            }
            if rule.max_len != 0 && char_len > rule.max_len {
                out.push(err(
                    field,
                    "max-len",
                    format!("must be at most {} characters", rule.max_len),
                ));
            }

            match rule.kind {
                Kind::Email => {
                    if !is_email_shape(s) {
                        out.push(err(field, "format", "not a valid email address"));
                    }
                }
                Kind::Alphanumeric => {
                    if !s.chars().all(|c| c.is_ascii_alphanumeric()) {
                        out.push(err(
                            field,
                            "format",
                            "must contain only ASCII letters and digits",
                        ));
                    }
                }
                Kind::Uuid => {
                    if !is_uuid_shape(s) {
                        out.push(err(field, "format", "not a valid UUID"));
                    }
                }
                _ => {}
            }
        }
        Kind::Integer => {
            let is_int = match value {
                Value::Number(n) => n.is_i64() || n.is_u64() || n.as_f64().map_or(false, |f| f.fract() == 0.0),
                _ => false,
            };
            if !is_int {
                out.push(err(field, "type", "expected an integer"));
                return;
            }
            check_numeric_range(rule, value, out);
            check_one_of(rule, value, out);
            return;
        }
        Kind::Number => {
            if !value.is_number() {
                out.push(err(field, "type", "expected a number"));
                return;
            }
            check_numeric_range(rule, value, out);
            check_one_of(rule, value, out);
            return;
        }
        Kind::Boolean => {
            if !value.is_boolean() {
                out.push(err(field, "type", "expected a boolean"));
                return;
            }
            check_one_of(rule, value, out);
            return;
        }
    }

    // Reaches here only for string-like kinds.
    check_one_of(rule, value, out);
}

/// Inclusive numeric range check for integer/number kinds.
fn check_numeric_range(rule: &Rule, value: &Value, out: &mut Vec<FieldError>) {
    let n = match value.as_f64() {
        Some(n) => n,
        None => return,
    };
    if let Some(min) = rule.min_value {
        if n < min {
            out.push(err(
                rule.field.as_str(),
                "min-value",
                format!("must be at least {min}"),
            ));
        }
    }
    if let Some(max) = rule.max_value {
        if n > max {
            out.push(err(
                rule.field.as_str(),
                "max-value",
                format!("must be at most {max}"),
            ));
        }
    }
}

/// Enum-membership check (only when `one-of` is non-empty).
fn check_one_of(rule: &Rule, value: &Value, out: &mut Vec<FieldError>) {
    if rule.one_of.is_empty() {
        return;
    }
    let repr = one_of_repr(value);
    let ok = match &repr {
        Some(r) => rule.one_of.iter().any(|allowed| allowed == r),
        None => false,
    };
    if !ok {
        out.push(err(
            rule.field.as_str(),
            "one-of",
            format!("must be one of: {}", rule.one_of.join(", ")),
        ));
    }
}

impl Guest for Component {
    fn validate(json: String, rules: Vec<Rule>) -> Vec<FieldError> {
        let root = match serde_json::from_str::<Value>(&json) {
            Ok(v) => v,
            Err(_) => {
                return vec![err("", "format", "not a valid JSON object")];
            }
        };
        let obj = match root.as_object() {
            Some(o) => o,
            None => {
                return vec![err("", "format", "not a valid JSON object")];
            }
        };

        let mut out = Vec::new();
        for rule in &rules {
            match obj.get(&rule.field) {
                None | Some(Value::Null) => {
                    if rule.required {
                        out.push(err(rule.field.as_str(), "required", "field is required"));
                    }
                    // Not required + absent/null -> skip remaining checks.
                }
                Some(value) => {
                    check_present(rule, value, &mut out);
                }
            }
        }
        out
    }
}

bindings::export!(Component with_types_in bindings);
