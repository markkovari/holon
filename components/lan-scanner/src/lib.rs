#[allow(warnings)]
mod bindings;
use bindings::exports::net::lan::scanner::Guest;
struct Component;
impl Guest for Component { fn scan() -> String { "192.168.1.1, 192.168.1.10".to_string() } }
bindings::export!(Component with_types_in bindings);
