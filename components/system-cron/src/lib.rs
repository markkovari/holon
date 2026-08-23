#[allow(warnings)]
mod bindings;
use bindings::exports::os::system::cron::Guest;
struct Component;
impl Guest for Component { fn list_jobs() -> String { "0 0 * * * backup.sh".to_string() } }
bindings::export!(Component with_types_in bindings);
