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
pub fn paginate_ids(ids: Vec<String>, size: u32, offset: u32) -> (Vec<String>, bool) {
    let offset = offset as usize;
    let size = size as usize;
    
    // If offset is past the end, return empty
    if offset >= ids.len() {
        return (Vec::new(), false);
    }
    
    // If size is 0, return empty but has_more depends on whether list is non-empty
    if size == 0 {
        return (Vec::new(), !ids.is_empty());
    }
    
    // Calculate the end index for this page
    let end = std::cmp::min(offset + size, ids.len());
    
    // Extract the slice for this page
    let hits = ids[offset..end].to_vec();
    
    // Check if more items exist beyond this page
    let has_more = end < ids.len();
    
    (hits, has_more)
}

struct Component;

impl Guest for Component {
    fn paginate(ids: Vec<String>, size: u32, offset: u32) -> Page {
        let (hits, has_more) = paginate_ids(ids, size, offset);
        Page { hits, has_more }
    }
}

bindings::export!(Component with_types_in bindings);
