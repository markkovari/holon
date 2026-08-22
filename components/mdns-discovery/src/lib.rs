#[allow(warnings)]
mod bindings;
use bindings::exports::net::mdns::discovery::Guest;
struct Component;
impl Guest for Component { fn discover() -> String { "Apple TV, HomePrinter".to_string() } }
bindings::export!(Component with_types_in bindings);
