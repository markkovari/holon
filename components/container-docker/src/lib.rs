//! `container-docker` — list and control containers running on the machine
//!
//! **There is NO implementation behind this contract.** Every export returns
//! an `UNIMPLEMENTED:` marker and `CATALOG.md` lists it as `contract only`.
//!
//! That is the honest state of this component rather than a placeholder
//! someone forgot to fill in, and it cannot be filled in from here: to talk to dockerd needs a container runtime socket,
//! and a wasm32-wasip2 component has none of those. The contract is the
//! useful part — it states what a host-side implementation must satisfy.
//!
//! It previously returned a plausible-looking constant, which is worse than
//! returning nothing: no caller could tell it apart from a component that
//! works, and neither could a reader of the catalogue. README says "nothing
//! is mocked on the path to a landed change"; this is that rule, applied here.

#[allow(warnings)]
mod bindings;
use bindings::exports::os::container::docker::Guest;
struct Component;
impl Guest for Component {
    fn ps() -> String {
        "UNIMPLEMENTED: container-docker cannot talk to dockerd from wasm".to_string()
    }
}
bindings::export!(Component with_types_in bindings);
