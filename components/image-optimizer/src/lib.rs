//! `image-optimizer` — a WIT contract with NO implementation behind it.
//!
//! Every export returns an `UNIMPLEMENTED:` marker, and that is the honest
//! state of this component rather than a placeholder someone forgot to fill
//! in. It cannot be filled in from here: to re-encode an image needs an image codec and CPU time,
//! and a wasm32-wasip2 component has none of those. The contract is the
//! useful part — it states what a host-side implementation must satisfy.
//!
//! It previously returned a plausible-looking constant, which is worse than
//! returning nothing: no caller could tell it apart from a component that
//! works, and neither could a reader of the catalogue. README says "nothing
//! is mocked on the path to a landed change"; this is that rule, applied here.

#[allow(warnings)]
mod bindings;
use bindings::exports::media::image::optimizer::Guest;
struct Component;
impl Guest for Component { fn optimize(img: String) -> String { format!("UNIMPLEMENTED: image-optimizer cannot re-encode an image from wasm ({})", img) } }
bindings::export!(Component with_types_in bindings);
