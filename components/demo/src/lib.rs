//! `demo` — paging, as a component. One half of a two-part goal (ADR-0086).
//!
//! A stub: the goal is to implement `paginate` against `wit/demo.wit`.

#[allow(warnings)]
mod bindings;

use bindings::exports::demo::shape::pager::{Guest, Page};

struct Component;

impl Guest for Component {
    fn paginate(_ids: Vec<String>, _size: u32, _offset: u32) -> Page {
        Page { hits: Vec::new(), has_more: false }
    }
}

bindings::export!(Component with_types_in bindings);
