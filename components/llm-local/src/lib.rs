//! `llm-local` — a WIT contract with NO implementation behind it.
//!
//! Every export returns an `UNIMPLEMENTED:` marker, and that is the honest
//! state of this component rather than a placeholder someone forgot to fill
//! in. It cannot be filled in from here: to run a local model needs a model runtime and weights on disk,
//! and a wasm32-wasip2 component has none of those. The contract is the
//! useful part — it states what a host-side implementation must satisfy.
//!
//! It previously returned a plausible-looking constant, which is worse than
//! returning nothing: no caller could tell it apart from a component that
//! works, and neither could a reader of the catalogue. README says "nothing
//! is mocked on the path to a landed change"; this is that rule, applied here.

#[allow(warnings)] mod bindings;
use bindings::exports::ai::local::local::Guest;
struct Component;
impl Guest for Component { fn infer(prompt: String) -> String { format!("UNIMPLEMENTED: llm-local cannot run a local model from wasm ({})", prompt) } }
bindings::export!(Component with_types_in bindings);
