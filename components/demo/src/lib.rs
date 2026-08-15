//! `demo` — paging, as a component. One half of a two-part goal (ADR-0086).
//!
//! A stub: the goal is to implement `paginate_ids` (which `tests/paginate.rs`
//! judges) and have the WIT export delegate to it.

#[allow(warnings)]
mod bindings;

use bindings::exports::demo::shape::pager::{Guest, Page};

/// Page a list of ids: the slice, and whether more remain beyond it.
///
/// A plain function so the held-out test can reach it — a `cdylib` export cannot
/// be called from `tests/`.
pub fn paginate_ids(_ids: Vec<String>, _size: u32, _offset: u32) -> (Vec<String>, bool) {
    (Vec::new(), false)
}

struct Component;

impl Guest for Component {
    fn paginate(ids: Vec<String>, size: u32, offset: u32) -> Page {
        let (hits, has_more) = paginate_ids(ids, size, offset);
        Page { hits, has_more }
    }
}

bindings::export!(Component with_types_in bindings);
