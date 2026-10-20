//! `slug` — turn a title into a clean URL slug, transliterating accents and suffixing on collision

#[allow(warnings)]
mod bindings;

use std::collections::HashSet;

use bindings::exports::slug::generate::generator::{Guest, Options};

struct Component;

fn translit(c: char) -> Option<&'static str> {
    match c {
        'à' | 'â' | 'ä' | 'á' | 'ã' | 'å' => Some("a"),
        'é' | 'è' | 'ê' | 'ë' => Some("e"),
        'î' | 'ï' | 'í' | 'ì' => Some("i"),
        'ô' | 'ö' | 'ó' | 'ò' | 'õ' => Some("o"),
        'ù' | 'û' | 'ü' | 'ú' => Some("u"),
        'ç' => Some("c"),
        'ñ' => Some("n"),
        'ß' => Some("ss"),
        _ => None,
    }
}

fn slugify_sep(text: &str, sep: &str) -> String {
    // Every char that isn't ASCII alphanumeric (after transliteration) is a boundary.
    let mut tokens: Vec<char> = Vec::with_capacity(text.len());
    for c in text.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            tokens.push(c);
        } else if let Some(rep) = translit(c) {
            tokens.extend(rep.chars());
        } else {
            tokens.push(' ');
        }
    }

    let mut out = String::with_capacity(tokens.len());
    let mut pending_sep = false;
    let mut started = false;
    for c in tokens {
        if c == ' ' {
            pending_sep = true;
        } else {
            if started && pending_sep {
                out.push_str(sep);
            }
            out.push(c);
            started = true;
            pending_sep = false;
        }
    }
    out
}

fn bound_length(mut s: String, sep: &str, max: usize) -> String {
    if max == 0 || s.len() <= max {
        return s;
    }
    s.truncate(max);
    if !sep.is_empty() {
        if let Some(idx) = s.rfind(sep) {
            s.truncate(idx);
        }
    }
    if !sep.is_empty() {
        while s.ends_with(sep) {
            let n = s.len() - sep.len();
            s.truncate(n);
        }
    }
    s
}

impl Guest for Component {
    fn slugify(text: String) -> String {
        Self::slugify_with(text, Options { separator: String::new(), max_length: 0 })
    }

    fn slugify_with(text: String, opts: Options) -> String {
        let sep = if opts.separator.is_empty() { "-" } else { opts.separator.as_str() };
        let slug = slugify_sep(&text, sep);
        bound_length(slug, sep, opts.max_length as usize)
    }

    fn uniquify(desired: String, taken: Vec<String>) -> String {
        let set: HashSet<&str> = taken.iter().map(|s| s.as_str()).collect();
        if !set.contains(desired.as_str()) {
            return desired;
        }
        let mut n: u64 = 2;
        loop {
            let candidate = format!("{desired}-{n}");
            if !set.contains(candidate.as_str()) {
                return candidate;
            }
            n += 1;
        }
    }
}

bindings::export!(Component with_types_in bindings);
