//! Where quests will be scored. Nothing is, yet.
//!
//! Step one of photoquest is upload, renditions and a gallery; quests, XP and
//! levels come next, and this is the seam they plug into. ADR-0098 says why it
//! is here and not in a background job: the component has no background, so
//! scoring happens inside the callback request that delivers the evaluation.
//!
//! What a scorer can rely on when this runs:
//!
//! * it runs ONCE per photo — on the transition into `evaluated`, never for a
//!   retried callback (`photos::evaluated` answers those as duplicates first),
//!   so XP granted here is not granted twice;
//! * the result is already stored: `photo` is the record as just saved (owner,
//!   filename, metadata, sharpness, vision, colour, backend), and `result` is
//!   the callback body verbatim;
//! * `backend` says which stages ran. A quest that needs Vision (faces,
//!   aesthetics) must check `backend.vision` rather than read a missing field as
//!   a zero — a photo evaluated on a node with no Vision stage did not score
//!   badly, it was not looked at;
//! * `vision.horizon_deg` is unreliable indoors (ADR-0098) and should count only
//!   for landscape quests.
//!
//! A failure in here must not fail the callback: the evaluation is stored and a
//! non-2xx would only make `comp-media` send it again.

use serde_json::{Map, Value};

/// Score `photo` against the owner's active quests. Deliberately does nothing
/// until quests exist.
pub fn on_evaluated(photo: &Map<String, Value>, result: &Value) {
    let _ = (photo, result);
}
