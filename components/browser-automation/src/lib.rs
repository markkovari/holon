#[allow(warnings)]
mod bindings;
use bindings::exports::web::browser::automation;
use bindings::exports::web::browser::automation::Guest;
struct Component;
impl Guest for Component { fn snapshot(url: String) -> String { format!("mock_pdf_base64_for_{}", url) } }
bindings::export!(Component with_types_in bindings);
