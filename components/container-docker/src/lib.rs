#[allow(warnings)]
mod bindings;
use bindings::exports::os::container::docker;
use bindings::exports::os::container::docker::Guest;
struct Component;
impl Guest for Component { fn ps() -> String { "container1: running, container2: stopped".to_string() } }
bindings::export!(Component with_types_in bindings);
