//! The hook for *automatic* reactions to an evaluation. It does nothing, on purpose: CONTRACT.md "Submitting, XP, levels,
//! badges" says a photo is judged against a quest only when its owner submits it (`POST /api/quests/{id}/submissions`, in
//! `progress.rs`); evaluating never auto-submits. Verdicts, XP, levels and badges live in `progress.rs`, the rules in `rules.rs`.
//!
//! What a future reaction here could rely on:
//!
//! * it runs ONCE per photo, on the transition into `evaluated` (`photos::evaluated` answers retried callbacks as duplicates first);
//! * the result is already stored: `photo` is the record as just saved and `result` is the callback body verbatim;
//! * `backend` says which stages ran (`rules.rs` reads a missing stage as "not looked at", `ok: null`, never zero);
//! * `vision.horizon_deg` is unreliable indoors (ADR-0098).
//!
//! A failure in here must not fail the callback: the evaluation is stored and a non-2xx would only make `comp-media` resend it.
//! non-2xx would only make `comp-media` send it again.

use serde_json::{Map, Value};

/// Deliberately a no-op: there is no automatic submission (see module docs).
pub fn on_evaluated(photo: &Map<String, Value>, result: &Value) {
    let _ = (photo, result);
}
