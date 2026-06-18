//! `i18n-catalog` — reference implementation of `i18n:catalog`.
//!
//! Message catalog with `{name}` placeholder interpolation, English-style
//! plural selection, and Accept-Language-style locale negotiation, backed by
//! `wasi:keyvalue`.
//!
//! Storage (bucket "default"):
//!   message key: `i18n_{locale}_{key}`  -> the template string bytes.
//!   plural  key: `i18np_{locale}_{key}` -> newline-joined `{category}\t{template}`
//!                                          lines.
//! Both `locale` and `key` are sanitized to kv-legal bytes (same byte scheme as
//! `idempotency-guard`'s `id_key`).
//!
//! Lookup falls back: exact locale -> base language (split on '-', take [0]) ->
//! configured `default-locale`.
//!
//! Config (wasi:config/runtime):
//!   default-locale  fallback locale tag (default "en").

#[allow(warnings)]
mod bindings;

use bindings::exports::i18n::catalog::catalog::{Arg, Guest, I18nError};
use bindings::wasi::config::runtime as config;
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";

// ---- config -------------------------------------------------------------

fn default_locale() -> String {
    match config::get("default-locale") {
        Ok(Some(v)) if !v.is_empty() => v,
        _ => "en".to_string(),
    }
}

// ---- key sanitization ---------------------------------------------------

/// Append `s`, sanitized to kv-legal chars (same scheme as
/// `idempotency-guard`'s `id_key`).
fn push_safe(out: &mut String, s: &str) {
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
}

fn message_key(locale: &str, key: &str) -> String {
    let mut out = String::from("i18n_");
    push_safe(&mut out, locale);
    out.push('_');
    push_safe(&mut out, key);
    out
}

fn plural_key(locale: &str, key: &str) -> String {
    let mut out = String::from("i18np_");
    push_safe(&mut out, locale);
    out.push('_');
    push_safe(&mut out, key);
    out
}

/// Base language of a tag: "en-US" -> "en", "en" -> "en".
fn base_lang(locale: &str) -> &str {
    locale.split('-').next().unwrap_or(locale)
}

// ---- kv helpers ---------------------------------------------------------

fn open() -> Result<kv::Bucket, I18nError> {
    kv::open(BUCKET).map_err(|e| I18nError::BackendUnavailable(format!("open: {e:?}")))
}

fn get_string(bucket: &kv::Bucket, key: &str) -> Result<Option<String>, I18nError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| I18nError::BackendUnavailable("value not utf-8".into())),
        Ok(None) => Ok(None),
        Err(e) => Err(I18nError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

/// Ordered, de-duplicated locale candidates: exact -> base -> default.
fn candidates(locale: &str) -> Vec<String> {
    let mut out = vec![locale.to_string()];
    let base = base_lang(locale).to_string();
    if !out.contains(&base) {
        out.push(base);
    }
    let def = default_locale();
    if !out.contains(&def) {
        out.push(def);
    }
    out
}

// ---- interpolation ------------------------------------------------------

/// Replace `{name}` tokens with matching arg values; unknown placeholders are
/// left verbatim. A single scan, no allocation per non-placeholder char beyond
/// the output buffer.
fn interpolate(template: &str, args: &[Arg]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                match args.iter().find(|a| a.name == name) {
                    Some(arg) => out.push_str(&arg.value),
                    // unknown placeholder -> leave the literal `{name}`.
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[end + 1..];
            }
            // unterminated '{' -> emit the rest verbatim.
            None => {
                out.push_str(rest);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parse a plural blob (`{category}\t{template}` per line) into pairs.
fn parse_plural(blob: &str) -> Vec<(String, String)> {
    blob.lines()
        .filter_map(|line| {
            line.split_once('\t')
                .map(|(c, t)| (c.to_string(), t.to_string()))
        })
        .collect()
}

impl Guest for Component {
    fn set_message(locale: String, key: String, value: String) -> Result<(), I18nError> {
        let bucket = open()?;
        bucket
            .set(&message_key(&locale, &key), value.as_bytes())
            .map_err(|e| I18nError::BackendUnavailable(format!("set: {e:?}")))
    }

    fn set_plural(
        locale: String,
        key: String,
        forms: Vec<(String, String)>,
    ) -> Result<(), I18nError> {
        let bucket = open()?;
        let blob = forms
            .iter()
            .map(|(cat, tmpl)| format!("{cat}\t{tmpl}"))
            .collect::<Vec<_>>()
            .join("\n");
        bucket
            .set(&plural_key(&locale, &key), blob.as_bytes())
            .map_err(|e| I18nError::BackendUnavailable(format!("set: {e:?}")))
    }

    fn translate(locale: String, key: String, args: Vec<Arg>) -> Result<String, I18nError> {
        let bucket = open()?;
        for cand in candidates(&locale) {
            if let Some(tmpl) = get_string(&bucket, &message_key(&cand, &key))? {
                return Ok(interpolate(&tmpl, &args));
            }
        }
        Err(I18nError::MissingMessage)
    }

    fn translate_plural(
        locale: String,
        key: String,
        count: u64,
        args: Vec<Arg>,
    ) -> Result<String, I18nError> {
        let bucket = open()?;
        // Resolve the plural blob with the same fallback chain.
        let mut forms: Option<Vec<(String, String)>> = None;
        for cand in candidates(&locale) {
            if let Some(blob) = get_string(&bucket, &plural_key(&cand, &key))? {
                forms = Some(parse_plural(&blob));
                break;
            }
        }
        let forms = forms.ok_or(I18nError::MissingMessage)?;

        // English plural rule: 1 -> "one", else "other".
        let category = if count == 1 { "one" } else { "other" };
        let template = forms
            .iter()
            .find(|(c, _)| c == category)
            .or_else(|| forms.iter().find(|(c, _)| c == "other"))
            .map(|(_, t)| t.clone())
            .ok_or(I18nError::MissingMessage)?;

        // Auto-add a `count` arg unless the caller already supplied one.
        let mut args = args;
        if !args.iter().any(|a| a.name == "count") {
            args.push(Arg {
                name: "count".to_string(),
                value: count.to_string(),
            });
        }
        Ok(interpolate(&template, &args))
    }

    fn negotiate(preferred: Vec<String>, available: Vec<String>) -> String {
        for tag in &preferred {
            // exact match wins.
            if available.iter().any(|a| a == tag) {
                return tag.clone();
            }
            // else an available tag whose base language equals this preferred
            // tag's base language (covers "en-US" preferred -> "en" available
            // and "en" preferred -> "en-GB" available).
            let want_base = base_lang(tag);
            if let Some(hit) = available.iter().find(|a| base_lang(a) == want_base) {
                return hit.clone();
            }
        }
        // nothing matched -> default if available, else first available, else default.
        let def = default_locale();
        if available.iter().any(|a| *a == def) {
            def
        } else if let Some(first) = available.first() {
            first.clone()
        } else {
            def
        }
    }
}

bindings::export!(Component with_types_in bindings);
