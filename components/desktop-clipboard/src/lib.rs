#[allow(warnings)]
mod bindings;
use bindings::exports::os::desktop::clipboard::Guest;
struct Component;
impl Guest for Component { fn read() -> String { "mocked_clipboard_text_123".to_string() } }
bindings::export!(Component with_types_in bindings);
