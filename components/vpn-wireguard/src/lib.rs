#[allow(warnings)]
mod bindings;
use bindings::exports::net::vpn::wireguard;
use bindings::exports::net::vpn::wireguard::Guest;
struct Component;
impl Guest for Component { fn status() -> String { "wg0 is UP, 2 peers connected".to_string() } }
bindings::export!(Component with_types_in bindings);
