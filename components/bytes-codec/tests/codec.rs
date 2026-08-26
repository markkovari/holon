//! The specification, held out: this file is not writable by the goal.
//!
//! The vectors are RFC 4648's own, because base64 is a thing with an answer and a
//! test that invents its own examples is testing the author's arithmetic. The cases
//! that decide the goal are the two alphabets producing DIFFERENT bytes from one
//! input, and padding being optional on the way in — everything else is a table.

use bytes_codec::{decode, encode, from_hex, to_hex, Alphabet, DecodeError};

const STD: Alphabet = Alphabet::Standard;
const URL: Alphabet = Alphabet::UrlSafe;

/// RFC 4648 §10, verbatim.
#[test]
fn the_rfc_vectors() {
    for (input, expected) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(encode(input.as_bytes(), STD), expected, "encode {input:?}");
        assert_eq!(decode(expected, STD).expect(expected), input.as_bytes(), "decode {expected:?}");
    }
}

/// THE case. `>` and `?` are the bytes that differ between the alphabets, and the
/// two tables produce different TEXT for one input — which means decoding with the
/// wrong one produces different BYTES and reports nothing.
#[test]
fn the_two_alphabets_are_not_interchangeable() {
    // 0xFB 0xFF encodes to `+/8=` in standard and `-_8` in URL-safe.
    let bytes = [0xFBu8, 0xFF];
    let std = encode(&bytes, STD);
    let url = encode(&bytes, URL);
    assert_eq!(std, "+/8=");
    assert_eq!(url, "-_8");
    assert_ne!(std, url, "the whole reason both exist");

    assert_eq!(decode(&std, STD).expect("std"), bytes);
    assert_eq!(decode(&url, URL).expect("url"), bytes);

    // And the wrong table REFUSES rather than quietly returning other bytes.
    assert!(matches!(decode(&std, URL), Err(DecodeError::NotInAlphabet { .. })), "+ and / are not URL-safe");
    assert!(matches!(decode(&url, STD), Err(DecodeError::NotInAlphabet { .. })), "- and _ are not standard");
}

/// URL-safe does not pad, because the specifications that use it forbid it.
#[test]
fn url_safe_does_not_pad() {
    assert_eq!(encode(b"f", URL), "Zg");
    assert_eq!(encode(b"fo", URL), "Zm8");
    assert_eq!(encode(b"foo", URL), "Zm9v");
    assert!(!encode(b"any length at all", URL).contains('='));
}

/// But it decodes padded input anyway: `=` on URL-safe text is wrong per the
/// specification and unambiguous in practice, and refusing it fails on real tokens.
#[test]
fn padding_is_optional_on_the_way_in() {
    assert_eq!(decode("Zg==", URL).expect("padded url-safe"), b"f");
    assert_eq!(decode("Zg", URL).expect("unpadded url-safe"), b"f");
    assert_eq!(decode("Zg", STD).expect("unpadded standard"), b"f");
    assert_eq!(decode("Zm8=", STD).expect("padded standard"), b"fo");
}

/// A round trip over every byte value, which is the only way to catch a table with
/// one character wrong in it.
#[test]
fn every_byte_survives_a_round_trip() {
    let all: Vec<u8> = (0..=255u8).collect();
    for alphabet in [STD, URL] {
        let text = encode(&all, alphabet);
        assert_eq!(decode(&text, alphabet).expect("round trip"), all, "{alphabet:?}");
    }
}

/// Length matters: three bytes are four characters, and the remainder decides how
/// many the last group has.
#[test]
fn the_encoded_length_follows_the_input() {
    for n in 0..40usize {
        let bytes = vec![0x5Au8; n];
        let std = encode(&bytes, STD);
        assert_eq!(std.len() % 4, 0, "standard pads to a multiple of four: {n} -> {std:?}");
        assert_eq!(std.len(), n.div_ceil(3) * 4, "{n}");
        // Unpadded is exactly the characters the bits need.
        assert_eq!(encode(&bytes, URL).len(), (n * 8).div_ceil(6), "{n}");
    }
}

/// A character in neither table is refused, and the error says where — the only
/// useful thing to report about a payload nobody should print.
#[test]
fn a_character_in_neither_alphabet_is_refused() {
    match decode("Zm9v!mFy", STD) {
        Err(DecodeError::NotInAlphabet { at, found }) => {
            assert_eq!(found, '!');
            assert_eq!(at, 4);
        }
        other => panic!("expected NotInAlphabet, got {other:?}"),
    }
    assert!(matches!(decode("Zm 9v", STD), Err(DecodeError::NotInAlphabet { .. })), "a space is not skipped");
}

/// A length that cannot be a whole number of bytes.
#[test]
fn an_orphan_character_is_refused() {
    // Five characters: one full group and one lone character, which encodes no byte.
    match decode("Zm9vYmFyZ", STD) {
        Err(DecodeError::TruncatedGroup { length }) => assert_eq!(length, 9),
        other => panic!("expected TruncatedGroup, got {other:?}"),
    }
}

/// Padding belongs at the end and nowhere else.
#[test]
fn padding_in_the_middle_is_refused() {
    assert!(matches!(decode("Zg==Zg==", STD), Err(DecodeError::MisplacedPadding { .. })));
    assert!(matches!(decode("Z===", STD), Err(DecodeError::MisplacedPadding { .. })), "three is too many");
}

/// Empty is empty, not an error — a zero-length payload is a real thing to encode.
#[test]
fn empty_round_trips() {
    for alphabet in [STD, URL] {
        assert_eq!(encode(&[], alphabet), "");
        assert_eq!(decode("", alphabet).expect("empty"), Vec::<u8>::new());
    }
}

// ---- hex ----------------------------------------------------------------

#[test]
fn hex_is_lowercase_out_and_either_case_in() {
    assert_eq!(to_hex(&[0xDE, 0xAD, 0xBE, 0xEF]), "deadbeef");
    assert_eq!(to_hex(&[0x00, 0x0F]), "000f", "and zero-padded per byte");
    assert_eq!(to_hex(&[]), "");

    for text in ["deadbeef", "DEADBEEF", "DeAdBeEf"] {
        assert_eq!(from_hex(text).expect(text), vec![0xDE, 0xAD, 0xBE, 0xEF], "{text}");
    }
}

#[test]
fn hex_refuses_what_is_not_hex() {
    match from_hex("dead beef") {
        Err(DecodeError::NotInAlphabet { at, found }) => {
            assert_eq!(found, ' ');
            assert_eq!(at, 4);
        }
        other => panic!("expected NotInAlphabet, got {other:?}"),
    }
    // An odd number of digits is half a byte.
    match from_hex("abc") {
        Err(DecodeError::TruncatedGroup { length }) => assert_eq!(length, 3),
        other => panic!("expected TruncatedGroup, got {other:?}"),
    }
}

#[test]
fn every_byte_survives_hex() {
    let all: Vec<u8> = (0..=255u8).collect();
    assert_eq!(from_hex(&to_hex(&all)).expect("round trip"), all);
}
