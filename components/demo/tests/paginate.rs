//! The gate for the `component` half — HELD OUT.
//!
//! It lives in `tests/`, which is not in that part's `writable` list, so the model
//! cannot make it pass by changing it (ADR-0081's held-out checks). It calls a
//! plain function rather than the component's `Guest` impl, because a `cdylib`
//! export is not something an integration test can reach — so the goal asks for
//! `paginate_ids`, and the WIT export delegates to it.
//!
//! Every case here failed against the stub, which is the point: a gate the base
//! tree already passes accepts anything, and the first real run of this goal
//! proved that the hard way.

use demo::paginate_ids;

fn ids(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("id-{i}")).collect()
}

#[test]
fn a_page_is_the_slice_and_says_whether_more_remain() {
    let (hits, has_more) = paginate_ids(ids(5), 2, 0);
    assert_eq!(hits, ["id-0", "id-1"]);
    assert!(has_more, "three ids remain after the first page of two");

    let (hits, has_more) = paginate_ids(ids(5), 2, 4);
    assert_eq!(hits, ["id-4"], "a short last page is still a page");
    assert!(!has_more, "nothing remains after the last id");
}

#[test]
fn an_offset_past_the_end_is_empty_rather_than_a_panic() {
    let (hits, has_more) = paginate_ids(ids(3), 2, 99);
    assert!(hits.is_empty());
    assert!(!has_more);
}

#[test]
fn a_size_of_zero_is_an_empty_page_rather_than_a_panic() {
    let (hits, has_more) = paginate_ids(ids(3), 0, 0);
    assert!(hits.is_empty(), "a page of nothing holds nothing");
    assert!(has_more, "and everything still remains");
}

#[test]
fn paging_an_empty_list_is_empty() {
    let (hits, has_more) = paginate_ids(Vec::new(), 10, 0);
    assert!(hits.is_empty());
    assert!(!has_more);
}
