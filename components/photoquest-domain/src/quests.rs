//! A no-op on purpose: CONTRACT.md judges a photo only when its owner submits it; evaluating never auto-submits.
//! A failure here must not fail the callback: a non-2xx would only make comp-media resend it.

use serde_json::{Map, Value};

pub fn on_evaluated(photo: &Map<String, Value>, result: &Value) {
    let _ = (photo, result);
}
