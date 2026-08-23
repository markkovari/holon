//! `lan-scanner` — find which hosts are reachable on the local network
//!
//! **There is NO implementation behind this contract.** Every export returns
//! an `UNIMPLEMENTED:` marker and `CATALOG.md` lists it as `contract only`.
//!
//! That is the honest state of this component rather than a placeholder
//! someone forgot to fill in, and it cannot be filled in from here: to scan a LAN needs raw sockets on the host network,
//! and a wasm32-wasip2 component has none of those. The contract is the
//! useful part — it states what a host-side implementation must satisfy.
//!
//! It previously returned a plausible-looking constant, which is worse than
//! returning nothing: no caller could tell it apart from a component that
//! works, and neither could a reader of the catalogue. README says "nothing
//! is mocked on the path to a landed change"; this is that rule, applied here.

#[allow(warnings)]
mod bindings;
use bindings::exports::net::lan::scanner::Guest;
struct Component;
impl Guest for Component {
    fn scan() -> String {
        "UNIMPLEMENTED: lan-scanner cannot scan a LAN from wasm".to_string()
    }
}
bindings::export!(Component with_types_in bindings);
