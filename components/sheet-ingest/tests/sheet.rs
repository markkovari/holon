//! The specification, held out.
//!
//! What this can judge natively is the part this component actually WRITES: the
//! OOXML shape, and the row/header discipline. The CSV path and the ZIP path are
//! delegated to `csv:codec` and `zip:archive`, so they are judged by their own
//! specifications and by the app's e2e over the composed artifact — testing them
//! again here would be testing a mock.
//!
//! The fixture is a real `.xlsx`, deflated, written by a tool that is not this one.

use sheet_ingest::{
    column_of, format_of, shape, shared_strings, worksheet_rows, xlsx_from_parts, Format,
    ImportError, Row,
};

/// The whole path, from the real file's parts.
fn parts() -> Vec<(String, Vec<u8>)> {
    let bytes = include_bytes!("fixtures/cards.xlsx");
    zip::extract(bytes)
        .expect("the fixture is a real xlsx")
        .into_iter()
        .map(|f| (f.name, f.data))
        .collect()
}

#[test]
fn a_real_xlsx_becomes_a_header_and_rows() {
    let sheet = xlsx_from_parts(&parts()).expect("read");
    assert_eq!(sheet.header, ["name", "set", "number", "quantity", "paid_minor", "currency"]);
    assert_eq!(sheet.sheet_name, "sheet1");
    assert_eq!(sheet.rows.len(), 4);
    assert_eq!(
        sheet.rows[0].cells,
        ["Charizard", "Base Set", "4/102", "1", "120000", "EUR"]
    );
    assert_eq!(sheet.rows[3].cells, ["Mewtwo", "Base Set", "10/102", "3", "18000", "EUR"]);
}

/// Numbers come back as their literal `<v>`, not as shared-string indices. Getting
/// this wrong turns a spreadsheet of prices into a spreadsheet of small integers
/// that all look plausible.
#[test]
fn a_numeric_cell_is_its_value_and_a_text_cell_is_looked_up() {
    let sheet = xlsx_from_parts(&parts()).expect("read");
    assert_eq!(sheet.rows[0].cells[4], "120000", "the price, not an index");
    assert_eq!(sheet.rows[0].cells[0], "Charizard", "the name, not an index");
}

/// THE case. `<c r="A1"/><c r="C1"/>` is three columns with the middle one empty.
/// A reader that appends in document order shifts every later value one place left
/// — a bulk insert writing the price into the quantity column, with nothing failing.
#[test]
fn a_gap_is_an_empty_column_not_a_missing_one() {
    let xml = r#"<worksheet><sheetData>
        <row r="1"><c r="A1" t="inlineStr"><is><t>a</t></is></c><c r="C1" t="inlineStr"><is><t>c</t></is></c></row>
        <row r="2"><c r="A2"><v>1</v></c><c r="B2"><v>2</v></c><c r="C2"><v>3</v></c></row>
    </sheetData></worksheet>"#;
    let rows = worksheet_rows(xml, &[]);
    assert_eq!(rows[0], ["a", "", "c"], "the gap at B must be a column");
    assert_eq!(rows[1], ["1", "2", "3"]);
}

#[test]
fn column_letters_are_base_26_from_one() {
    assert_eq!(column_of("A1"), Some(0));
    assert_eq!(column_of("Z9"), Some(25));
    assert_eq!(column_of("AA1"), Some(26), "not 0 — the alphabet has no zero");
    assert_eq!(column_of("AB1"), Some(27));
    assert_eq!(column_of("BA1"), Some(52));
    assert_eq!(column_of("1"), None);
}

/// A `<si>` may be several formatted runs, and the spreadsheet shows them joined.
#[test]
fn shared_strings_join_their_runs() {
    let xml = r#"<sst><si><t>plain</t></si><si><r><t>bold</t></r><r><t> and not</t></r></si><si><t/></si></sst>"#;
    assert_eq!(shared_strings(xml), ["plain", "bold and not", ""]);
}

#[test]
fn xml_entities_come_back_as_themselves() {
    let xml = r#"<sst><si><t>Farfetch&apos;d &amp; co &lt;3</t></si></sst>"#;
    assert_eq!(shared_strings(xml), ["Farfetch'd & co <3"]);
    // `&amp;lt;` is a literal `&lt;`, not a `<`.
    assert_eq!(shared_strings(r#"<sst><si><t>&amp;lt;</t></si></sst>"#), ["&lt;"]);
}

#[test]
fn a_short_row_is_padded_and_a_long_one_is_refused() {
    let head = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let short = shape(vec![head.clone(), vec!["1".into()]], "s").expect("short rows are fine");
    assert_eq!(short.rows[0], Row { cells: vec!["1".into(), String::new(), String::new()] });

    // Trailing blanks on a long row are the spreadsheet's used range, not data.
    let padded = shape(
        vec![head.clone(), vec!["1".into(), "2".into(), "3".into(), "".into(), "".into()]],
        "s",
    )
    .expect("trailing blanks are trimmed");
    assert_eq!(padded.rows[0].cells.len(), 3);

    // A real fourth value is a cell nobody asked to drop.
    match shape(vec![head, vec!["1".into(), "2".into(), "3".into(), "4".into()]], "s") {
        Err(ImportError::TooManyCells { row, expected, found }) => {
            assert_eq!((row, expected, found), (2, 3, 4), "row 2, counting the header");
        }
        other => panic!("expected TooManyCells, got {other:?}"),
    }
}

#[test]
fn a_duplicate_column_would_silently_win() {
    let r = shape(vec![vec!["a".into(), "b".into(), "a".into()]], "s");
    assert_eq!(r, Err(ImportError::DuplicateColumn("a".into())));
}

#[test]
fn trailing_blank_rows_and_columns_are_not_data() {
    let sheet = shape(
        vec![
            vec!["a".into(), "b".into(), "".into(), "".into()],
            vec!["1".into(), "2".into()],
            vec!["".into(), "".into()],
            vec![],
        ],
        "s",
    )
    .expect("shape");
    assert_eq!(sheet.header, ["a", "b"], "unnamed trailing columns are noise");
    assert_eq!(sheet.rows.len(), 1, "the blank rows a delete leaves behind");
}

#[test]
fn nothing_at_all() {
    assert_eq!(shape(vec![], "s"), Err(ImportError::Empty));
    assert_eq!(shape(vec![vec!["".into(), " ".into()]], "s"), Err(ImportError::Empty));
    assert_eq!(shape(vec![vec!["".into()], vec!["x".into()]], "s"), Err(ImportError::NoHeader));
}

/// By extension, never by sniffing bytes.
#[test]
fn the_format_comes_from_the_name() {
    assert_eq!(format_of("cards.csv"), Ok(Format::Csv));
    assert_eq!(format_of("CARDS.CSV"), Ok(Format::Csv));
    assert_eq!(format_of("cards.tsv"), Ok(Format::Tsv));
    assert_eq!(format_of("a/b/cards.xlsx"), Ok(Format::Xlsx));
    assert_eq!(format_of("cards.numbers"), Err(ImportError::UnknownFormat("cards.numbers".into())));
    assert_eq!(format_of("cards.xls"), Err(ImportError::UnknownFormat("cards.xls".into())));
}
