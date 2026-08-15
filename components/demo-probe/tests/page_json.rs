//! The gate for the `probe` half — HELD OUT.
//!
//! `tests/` is not in that part's `writable` list, so the model cannot make this
//! pass by editing it (ADR-0081). It judges the one thing a probe can be judged on
//! without a host: the JSON it writes.
//!
//! What this does NOT check, and nothing cheap could: that the handler actually
//! calls `demo:shape/pager`. `cargo component check` and `cargo component build`
//! both pass on a crate that implements none of its world — measured, twice — so
//! neither is a gate. Proving the call happens needs the composed component on a
//! host, which is the composition gate's job and costs a run.

use demo_probe::page_json;

#[test]
fn a_page_is_json_with_the_hits_and_the_flag() {
    let out = page_json(&["a".into(), "b".into()], true);
    let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(v["hits"], serde_json::json!(["a", "b"]));
    assert_eq!(v["has_more"], serde_json::json!(true));
}

#[test]
fn an_empty_page_is_an_empty_array_and_not_null() {
    let v: serde_json::Value = serde_json::from_str(&page_json(&[], false)).expect("valid JSON");
    assert_eq!(v["hits"], serde_json::json!([]), "null is not an empty page");
    assert_eq!(v["has_more"], serde_json::json!(false));
}

#[test]
fn an_id_with_a_quote_in_it_does_not_break_the_json() {
    let out = page_json(&[r#"a"b"#.into()], false);
    let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON despite the quote");
    assert_eq!(v["hits"][0], serde_json::json!(r#"a"b"#));
}
