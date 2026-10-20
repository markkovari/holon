//! Acronym boundary: `HTTPServer` splits as `HTTP`, `Server`; camel/pascal normalise acronyms (`Http`).

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

pub fn to_snake(s: &str) -> String {
    words(s).iter().map(|w| w.to_lowercase()).collect::<Vec<_>>().join("_")
}

pub fn to_kebab(s: &str) -> String {
    words(s).iter().map(|w| w.to_lowercase()).collect::<Vec<_>>().join("-")
}

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

pub fn to_pascal(s: &str) -> String {
    words(s).iter().map(|w| capitalize(w)).collect()
}

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
