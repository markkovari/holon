//! `demo` — paging, as a component. One half of a two-part goal (ADR-0086).
//!
//! A stub: the goal is to implement `paginate_ids` (which `tests/paginate.rs`
//! judges) and have the WIT export delegate to it.

// The component half compiles for wasm only.
//
// `tests/paginate.rs` is a HELD-OUT gate (ADR-0081): it lives in `tests/` because
// the goal's `writable` list does not include it, so a candidate cannot pass by
// editing it. Reaching a function from `tests/` needs an `rlib`, and an rlib makes
// cargo link this crate for the HOST too — where `demo:shape/pager@0.1.0` and its
// `cabi_post` partner are undefined symbols, so the link fails.
//
// That is why `just test` died on the first workspace before running a single
// test, and why the held-out gate could not be run at all: `cargo test -p demo
// --test paginate` builds the cdylib first and dies there too.
//
// Gating the bindings on the target fixes both. Natively this crate is just
// `paginate_ids`, which is exactly what the gate calls; for wasm32 it is the whole
// component. Nothing about the component's behaviour changes.
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
