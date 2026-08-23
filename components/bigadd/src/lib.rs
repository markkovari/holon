pub mod add;
#[allow(warnings)]
mod bindings;
use bindings::exports::demo::bigadd::bignum::Guest;
struct Component;
impl Guest for Component {
    fn add(a: String, b: String) -> String {
        add::add(&a, &b)
    }
}
bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::add::add;
    #[test]
    fn small() {
        assert_eq!(add("1", "2"), "3");
        assert_eq!(add("5", "5"), "10");
    }
    #[test]
    fn carry_ripples() {
        assert_eq!(add("999", "1"), "1000");
        assert_eq!(add("1", "9999"), "10000");
    }
    #[test]
    fn zero() {
        assert_eq!(add("0", "0"), "0");
    }
    #[test]
    fn different_lengths() {
        assert_eq!(add("12", "3456"), "3468");
    }
    #[test]
    fn beyond_u128() {
        assert_eq!(
            add("123456789012345678901234567890", "987654321098765432109876543210"),
            "1111111110111111111011111111100"
        );
    }
}
