#[allow(warnings)]
mod bindings;
use bindings::exports::media::image::optimizer;
use bindings::exports::media::image::optimizer::Guest;
struct Component;
impl Guest for Component { fn optimize(img: String) -> String { format!("optimized_{}", img) } }
bindings::export!(Component with_types_in bindings);
