#[allow(warnings)] mod bindings;
use bindings::exports::ai::inference::local::Guest;
struct Component;
impl Guest for Component { fn infer(prompt: String) -> String { format!("AI response to: {}", prompt) } }
bindings::export!(Component with_types_in bindings);
