#[allow(warnings)]
mod bindings;
use bindings::exports::os::ui::notifications::Guest;
struct Component;
impl Guest for Component { fn notify(msg: String) -> String { format!("Notification sent: {}", msg) } }
bindings::export!(Component with_types_in bindings);
