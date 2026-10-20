const VALID: &[&str] = &[
    "79927398713",      // Wikipedia's own worked example
    "4111111111111111", // Visa test number
    "5500005555555559", // Mastercard test number
    "0",
];

const INVALID: &[&str] = &["79927398710", "79927398711", "79927398712", "4111111111111112"];

#[test]
fn known_valid_numbers_pass() {
    for n in VALID {
        assert!(luhn_checksum::is_valid(n), "{n} should be valid");
    }
}

#[test]
fn known_invalid_numbers_fail() {
    for n in INVALID {
        assert!(!luhn_checksum::is_valid(n), "{n} should be invalid");
    }
}

#[test]
fn empty_string_is_not_valid() {
    assert!(!luhn_checksum::is_valid(""));
    assert_eq!(luhn_checksum::checksum_digit(""), None);
    assert_eq!(luhn_checksum::append_checksum(""), None);
}

#[test]
fn a_non_digit_character_is_not_valid_and_not_a_panic() {
    assert!(!luhn_checksum::is_valid("799273987a3"));
    assert!(!luhn_checksum::is_valid("abc"));
    assert!(!luhn_checksum::is_valid("12 34"));
    assert_eq!(luhn_checksum::checksum_digit("abc"), None);
    assert_eq!(luhn_checksum::append_checksum("abc"), None);
}

#[test]
fn checksum_digit_makes_the_number_valid_once_appended() {
    // 7992739871 + check digit 3 = the Wikipedia example above.
    assert_eq!(luhn_checksum::checksum_digit("7992739871"), Some(3));
    assert_eq!(luhn_checksum::append_checksum("7992739871"), Some("79927398713".to_string()));
    assert!(luhn_checksum::is_valid(&luhn_checksum::append_checksum("7992739871").unwrap()));
}

#[test]
fn parity_is_counted_from_the_right_not_the_left() {
    // Parity is anchored to the RIGHT end: a leading zero must not shift it.
    let with_leading_zero = format!("0{}", "7992739871");
    assert_eq!(
        luhn_checksum::checksum_digit(&with_leading_zero),
        luhn_checksum::checksum_digit("7992739871")
    );
}

#[test]
fn doubling_that_overflows_nine_folds_by_subtracting_nine_not_modulo() {
    // "9" doubled is 18, folding to 9, so the check digit that brings the sum to a multiple of 10 is 1.
    assert_eq!(luhn_checksum::checksum_digit("9"), Some(1));
    assert!(luhn_checksum::is_valid("91"));
}

#[test]
fn a_single_digit_round_trips() {
    for d in 0..10u8 {
        let s = d.to_string();
        let with_check = luhn_checksum::append_checksum(&s).unwrap();
        assert!(luhn_checksum::is_valid(&with_check), "{with_check}");
    }
}

#[test]
fn zero_is_trivially_valid() {
    assert!(luhn_checksum::is_valid("0"));
}
