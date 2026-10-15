//! `demo` — paging, as a component. One half of a two-part goal (ADR-0086).
//!
//! A stub: the goal is to implement `paginate_ids` (which `tests/paginate.rs`
//! judges) and have the WIT export delegate to it.

// The component half compiles for wasm only. `tests/paginate.rs` is a HELD-OUT gate
// (ADR-0081); reaching a function from `tests/` needs an `rlib`, which makes cargo link
// this crate for the HOST too, where `demo:shape/pager@0.1.0` and its `cabi_post` partner
// are undefined symbols. Gating the bindings on the target fixes it: natively this crate
// is just `paginate_ids`, for wasm32 it is the whole component.
#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

#[cfg(target_arch = "wasm32")]
use bindings::exports::demo::shape::pager::{Guest, Page};

/// Page a list of ids: the slice, and whether more remain beyond it.
///
/// A plain function so the held-out test can reach it — a `cdylib` export cannot
/// be called from `tests/`.
pub fn paginate_ids(ids: Vec<String>, size: u32, offset: u32) -> (Vec<String>, bool) {
    let offset = offset as usize;
    let size = size as usize;

    if offset >= ids.len() {
        return (Vec::new(), false);
    }

    // If size is 0, return empty but has_more depends on whether list is non-empty
    if size == 0 {
        return (Vec::new(), !ids.is_empty());
    }

    let end = std::cmp::min(offset + size, ids.len());

    let hits = ids[offset..end].to_vec();

    let has_more = end < ids.len();

    (hits, has_more)
}

#[cfg(target_arch = "wasm32")]
struct Component;

#[cfg(target_arch = "wasm32")]
impl Guest for Component {
    fn paginate(ids: Vec<String>, size: u32, offset: u32) -> Page {
        let (hits, has_more) = paginate_ids(ids, size, offset);
        Page { hits, has_more }
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);
