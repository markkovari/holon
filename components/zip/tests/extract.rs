//! The specification for reading an archive, held out.
//!
//! The fixture is a REAL `.xlsx` written by Python's `zipfile` with
//! `ZIP_DEFLATED` — every entry is method 8, with sizes and CRCs from a writer
//! that is not this one. A round trip through our own `archive()` would only
//! prove the writer and reader agree with each other, which is the one thing that
//! cannot go wrong in a way anybody notices.

use zip::{archive, extract, File, ZipError};

const XLSX: &[u8] = include_bytes!("fixtures/cards.xlsx");

#[test]
fn a_real_deflated_xlsx_reads_back() {
    let files = extract(XLSX).expect("a real .xlsx");
    let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"xl/worksheets/sheet1.xml"), "{names:?}");
    assert!(names.contains(&"xl/sharedStrings.xml"), "{names:?}");
    assert!(names.contains(&"[Content_Types].xml"), "{names:?}");

    // Inflated, not merely located: the bytes have to be the XML.
    let sheet = files.iter().find(|f| f.name == "xl/worksheets/sheet1.xml").unwrap();
    let text = String::from_utf8(sheet.data.clone()).expect("utf-8 xml");
    assert!(text.starts_with("<?xml"), "{}", &text[..40.min(text.len())]);
    assert!(text.contains("<sheetData>"), "the sheet body");

    let sst = files.iter().find(|f| f.name == "xl/sharedStrings.xml").unwrap();
    let sst = String::from_utf8(sst.data.clone()).unwrap();
    assert!(sst.contains("<t>Charizard</t>"), "the shared strings");
}

/// Catches an inflate that stops early — the failure mode that still produces
/// plausible-looking XML.
#[test]
fn inflated_entries_are_whole() {
    for f in &extract(XLSX).expect("a real .xlsx") {
        assert!(!f.data.is_empty(), "{} inflated to nothing", f.name);
        assert!(
            f.data.ends_with(b">"),
            "{} does not end with a closing tag — inflate stopped early",
            f.name
        );
    }
}

/// Our own STORE archives still read back, so writer and reader agree.
#[test]
fn store_round_trips() {
    let files = vec![
        File { name: "a.txt".into(), data: b"hello".to_vec() },
        File { name: "nested/b.bin".into(), data: (0..=255u8).collect() },
        File { name: "empty.txt".into(), data: Vec::new() },
    ];
    let bytes = archive(&files);
    assert_eq!(extract(&bytes).expect("round trip"), files);
}

#[test]
fn directory_entries_are_not_returned() {
    let files = extract(XLSX).expect("a real .xlsx");
    assert!(!files.iter().any(|f| f.name.ends_with('/')));
}

#[test]
fn something_that_is_not_a_zip() {
    assert_eq!(extract(b"not a zip at all"), Err(ZipError::NotAZip));
    assert_eq!(extract(&[]), Err(ZipError::NotAZip));
    assert_eq!(
        extract(&[0x89, b'P', b'N', b'G', 13, 10, 26, 10, 0, 0, 0, 13]),
        Err(ZipError::NotAZip)
    );
}

/// A byte flipped inside the data. The archive still parses, the entry still
/// yields something, and the CRC says it is not the file.
#[test]
fn a_corrupted_entry_is_refused_rather_than_returned() {
    let files = vec![File { name: "a.txt".into(), data: b"the original contents".to_vec() }];
    let mut bytes = archive(&files);
    let at = 30 + "a.txt".len() + 3;
    bytes[at] ^= 0xFF;
    match extract(&bytes) {
        Err(ZipError::BadChecksum { name }) => assert_eq!(name, "a.txt"),
        other => panic!("expected a checksum failure, got {other:?}"),
    }
}

#[test]
fn a_truncated_archive_says_so() {
    let files = vec![File { name: "a.txt".into(), data: vec![b'x'; 400] }];
    let bytes = archive(&files);
    let mut cut = bytes.clone();
    cut.drain(40..200);
    let got = extract(&cut);
    assert!(
        matches!(got, Err(ZipError::Truncated { .. }) | Err(ZipError::BadChecksum { .. })),
        "expected truncated or bad-checksum, got {got:?}"
    );
}
