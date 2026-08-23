//! `rot13` — obscure text with the ROT13 letter substitution, and reverse it

#[allow(warnings)]
mod bindings;
use bindings::exports::demo::rot13::cipher::Guest;
struct Component;

/// ROT13 every ASCII letter (a↔n, A↔N), leaving digits, punctuation and spaces
/// unchanged. UNIMPLEMENTED — the goal. The tests are the spec.
pub fn rot13(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'a'..='z' => ((c as u8 - b'a' + 13) % 26 + b'a') as char,
            'A'..='Z' => ((c as u8 - b'A' + 13) % 26 + b'A') as char,
            _ => c,
        })
        .collect()
}

impl Guest for Component {
    fn rot13(text: String) -> String {
        rot13(&text)
    }
}
bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::rot13;
    #[test]
    fn shifts_lowercase_by_13() {
        assert_eq!(rot13("abc"), "nop");
    }
    #[test]
    fn wraps_past_z() {
        assert_eq!(rot13("nop"), "abc");
    }
    #[test]
    fn preserves_case() {
        assert_eq!(rot13("Hello"), "Uryyb");
    }
    #[test]
    fn leaves_non_letters_alone() {
        assert_eq!(rot13("a1 b!z"), "n1 o!m");
    }
    #[test]
    fn is_its_own_inverse() {
        assert_eq!(rot13(&rot13("The Quick Brown Fox 42!")), "The Quick Brown Fox 42!");
    }
}
