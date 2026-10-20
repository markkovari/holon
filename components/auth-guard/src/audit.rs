//! Records no secrets (tokens, passwords), only identifiers, so a decision traces to an actor.

use crate::bindings::audit::log::recorder;
use crate::bindings::audit::log::types::Event;
use crate::bindings::wasi::random::random::get_random_bytes;
use crate::config;

pub enum Outcome {
    Allow,
    Deny,
    Error,
}

impl Outcome {
    fn as_str(&self) -> &'static str {
        match self {
            Outcome::Allow => "allow",
            Outcome::Deny => "deny",
            Outcome::Error => "error",
        }
    }
}

fn span_id() -> String {
    let b = get_random_bytes(8);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Absent or malformed mints a fresh trace-id.
fn trace_id_from(traceparent: &str) -> String {
    let parts: Vec<&str> = traceparent.split('-').collect();
    if parts.len() == 4 && parts[1].len() == 32 && parts[1].bytes().all(|c| c.is_ascii_hexdigit()) {
        parts[1].to_string()
    } else {
        let b = get_random_bytes(16);
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
}

pub fn emit(event: &str, outcome: Outcome, tenant: &str, subject: &str, detail: &str) {
    emit_traced(event, outcome, tenant, subject, detail, "");
}

pub fn emit_traced(
    event: &str,
    outcome: Outcome,
    tenant: &str,
    subject: &str,
    detail: &str,
    traceparent: &str,
) {
    if !config::audit_enabled() {
        return;
    }
    // Non-fatal: an audit-store hiccup must not break the auth decision.
    let _ = recorder::record_event(&Event {
        id: String::new(),
        trace_id: trace_id_from(traceparent),
        span_id: span_id(),
        timestamp: 0,
        event: event.to_string(),
        outcome: outcome.as_str().to_string(),
        tenant: tenant.to_string(),
        subject: subject.to_string(),
        detail: detail.to_string(),
    });
}
