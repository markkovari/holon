#[allow(warnings)]
mod bindings;
pub mod matcher;
use bindings::exports::demo::glob::matcher::Guest;
struct Component;
impl Guest for Component {
    fn matches(pattern: String, text: String) -> bool { matcher::matches(&pattern, &text) }
}
bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::matcher::matches;
    #[test] fn literal() { assert!(matches("abc", "abc")); assert!(!matches("abc", "abd")); }
    #[test] fn question() { assert!(matches("a?c", "abc")); assert!(!matches("a?c", "ac")); }
    #[test] fn star() { assert!(matches("a*c", "abbbc")); assert!(matches("a*c", "ac")); assert!(matches("*", "anything")); }
    #[test] fn suffix() { assert!(matches("*.txt", "file.txt")); assert!(!matches("*.txt", "file.md")); }
    #[test] fn class() { assert!(matches("[abc]d", "bd")); assert!(!matches("[abc]d", "dd")); }
    #[test] fn range() { assert!(matches("[a-z]", "m")); assert!(!matches("[a-z]", "M")); }
    #[test] fn many_stars() { assert!(matches("a*b*c", "axxbyyc")); assert!(!matches("a*b*c", "axxc")); }
    #[test] fn empties() { assert!(matches("", "")); assert!(!matches("a", "")); assert!(matches("*", "")); }
}
