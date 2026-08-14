#[allow(warnings)]
mod bindings;
use bindings::exports::demo::roman::numerals::Guest;
struct Component;

/// Integer -> Roman numeral, subtractive notation (4=IV, 9=IX, 40=XL, 900=CM),
/// for 1..=3999. UNIMPLEMENTED — the tests are the exact spec.
pub fn to_roman(n: u32) -> String {
    let values: [(u32, &str); 13] = [
        (1000, "M"), (900, "CM"), (500, "D"), (400, "CD"),
        (100, "C"), (90, "XC"), (50, "L"), (40, "XL"),
        (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I"),
    ];
    let mut n = n;
    let mut result = String::new();
    for (value, symbol) in values.iter() {
        while n >= *value {
            result.push_str(symbol);
            n -= value;
        }
    }
    result
}

/// Roman numeral -> integer.
pub fn from_roman(s: &str) -> u32 {
    let mut total = 0u32;
    let mut prev = 0u32;
    for c in s.chars().rev() {
        let value = match c {
            'I' => 1,
            'V' => 5,
            'X' => 10,
            'L' => 50,
            'C' => 100,
            'D' => 500,
            'M' => 1000,
            _ => 0,
        };
        if value < prev {
            total -= value;
        } else {
            total += value;
            prev = value;
        }
    }
    total
}

impl Guest for Component {
    fn to_roman(n: u32) -> String { to_roman(n) }
    fn from_roman(s: String) -> u32 { from_roman(&s) }
}
bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::{from_roman, to_roman};
    #[test] fn units() { assert_eq!(to_roman(1), "I"); assert_eq!(to_roman(3), "III"); }
    #[test] fn subtractive() {
        assert_eq!(to_roman(4), "IV"); assert_eq!(to_roman(9), "IX");
        assert_eq!(to_roman(40), "XL"); assert_eq!(to_roman(90), "XC");
        assert_eq!(to_roman(400), "CD"); assert_eq!(to_roman(900), "CM");
    }
    #[test] fn composite() {
        assert_eq!(to_roman(2024), "MMXXIV");
        assert_eq!(to_roman(3888), "MMMDCCCLXXXVIII");
    }
    #[test] fn parse_back() {
        assert_eq!(from_roman("IV"), 4);
        assert_eq!(from_roman("MMXXIV"), 2024);
        assert_eq!(from_roman("MMMDCCCLXXXVIII"), 3888);
    }
    #[test] fn round_trips() {
        for n in [1u32, 4, 9, 14, 40, 99, 444, 2024, 3999] {
            assert_eq!(from_roman(&to_roman(n)), n, "round trip {n}");
        }
    }
}
