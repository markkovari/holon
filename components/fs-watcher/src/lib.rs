#[allow(warnings)]
mod bindings;
use bindings::exports::os::fs::watcher;
use bindings::exports::os::fs::watcher::Guest;
struct Component;
impl Guest for Component { fn watch(dir: String) -> String { format!("Watching {} for changes...", dir) } }
bindings::export!(Component with_types_in bindings);
