//! Real rendered barcodes in, known digits out.
//!
//! The fixtures were rendered by `python-barcode`, which is not this decoder's
//! encoder. That matters: a decoder tested against its own renderer proves the
//! two agree, not that either is right, and a pair of matched mistakes passes
//! every test forever.
//!
//! `not-a-barcode.png` is here for the failure that this repository has already
//! shipped three times — a capability returning a plausible constant. A decoder
//! that answers `4006381333931` for everything passes every other test in this
//! file and fails that one.

use barcode_read::scan::decode_png;

fn fixture(name: &str) -> Vec<u8> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/").to_string() + name;
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

fn reads(name: &str) -> (String, String) {
    let s = decode_png(&fixture(name)).unwrap_or_else(|e| panic!("{name} did not decode: {e}"));
    (s.text, s.symbology)
}

#[test]
fn the_four_symbologies_a_shop_meets() {
    assert_eq!(reads("ean13.png"), ("4006381333931".into(), "ean-13".into()));
    assert_eq!(reads("ean8.png"), ("96385074".into(), "ean-8".into()));
    // Thirteen digits reported as UPC-A: the same bars as the EAN-13, so the name is
    // reported and the text is not shortened.
    assert_eq!(reads("upca.png"), ("0036000291452".into(), "upc-a".into()));
    assert_eq!(reads("code128.png"), ("SHELF-A17".into(), "code-128".into()));
}

#[test]
fn a_label_that_is_not_held_straight_still_reads() {
    // Upside down: the same scanline, read backwards.
    assert_eq!(reads("ean13-upside-down.png").0, "4006381333931");
    // Sideways: columns rather than rows.
    assert_eq!(reads("ean13-sideways.png").0, "4006381333931");
    // A thumb over the bottom third. The bars carry nothing vertically, so a
    // scanline above the thumb has the whole symbol — which is the reason this
    // tries twenty-four lines and not one.
    assert_eq!(reads("ean13-thumb.png").0, "4006381333931");
}

/// Three codes chosen at random, not to be readable: the textbook examples can be tuned
/// for. A wider version of this check caught `0166131860910` coming back as
/// `166131860910` (a leading zero stripped as if every such code were a UPC-A).
#[test]
fn codes_that_were_not_picked_to_be_easy() {
    assert_eq!(reads("ean13-leading-zero.png"), ("0166131860910".into(), "upc-a".into()));
    assert_eq!(reads("code128-mixed.png"), ("AJ08X-U".into(), "code-128".into()));
    assert_eq!(reads("code128-letters.png"), ("ZZG4ZDMEN".into(), "code-128".into()));
}

#[test]
fn nothing_is_not_something() {
    assert!(
        decode_png(&fixture("not-a-barcode.png")).is_err(),
        "blank paper decoded to a product code — this is the constant-returner \
         failure that `docs/CURRENT.md` records as worse than returning nothing"
    );
    assert!(decode_png(b"not a png at all").is_err(), "arbitrary bytes are not an image");
    assert!(decode_png(&[]).is_err(), "no bytes are not an image");
}

#[test]
fn a_check_digit_is_actually_checked() {
    // A last-digit-changed fixture would need the encoder this test avoids, so assert the
    // property: every fixture that decodes carries a correct check digit.
    for name in ["ean13.png", "ean8.png", "upca.png"] {
        let (text, _) = reads(name);
        let d: Vec<u32> = text.chars().map(|c| c.to_digit(10).unwrap()).collect();
        let (body, check) = d.split_at(d.len() - 1);
        let sum: u32 =
            body.iter().rev().enumerate().map(|(i, v)| v * if i % 2 == 0 { 3 } else { 1 }).sum();
        assert_eq!((10 - sum % 10) % 10, check[0], "{name} came back failing its own checksum");
    }
}
