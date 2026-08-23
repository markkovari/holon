//! `fs-watcher` — a WIT contract with NO implementation behind it.
//!
//! Every export returns an `UNIMPLEMENTED:` marker, and that is the honest
//! state of this component rather than a placeholder someone forgot to fill
//! in. It cannot be filled in from here: to watch a directory needs a filesystem watch syscall (inotify/FSEvents),
//! and a wasm32-wasip2 component has none of those. The contract is the
//! useful part — it states what a host-side implementation must satisfy.
//!
//! It previously returned a plausible-looking constant, which is worse than
//! returning nothing: no caller could tell it apart from a component that
//! works, and neither could a reader of the catalogue. README says "nothing
//! is mocked on the path to a landed change"; this is that rule, applied here.

#[allow(warnings)]
mod bindings;
use bindings::exports::os::fs::watcher::Guest;
struct Component;
impl Guest for Component { fn watch(dir: String) -> String { format!("UNIMPLEMENTED: fs-watcher cannot watch a directory from wasm ({})", dir) } }
bindings::export!(Component with_types_in bindings);
