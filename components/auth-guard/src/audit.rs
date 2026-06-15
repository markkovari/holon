//! Structured audit logging of auth decisions.
//!
//! Decisions are recorded against the `audit:log/recorder` capability — a
//! SEPARATE `audit-log` component (composed with wac) that persists the trail
//! durably AND echoes each event to stderr (so the existing OTel/scrape path is
//! unchanged). This module owns only the auth-specific shaping: outcome mapping
//! and W3C traceparent -> trace-id/span-id. Deliberately records NO secrets: no
//! tokens, no passwords, no refresh tokens. Identifiers (email, subject, tenant)
//! are logged so a decision can be traced to an actor.
//!
//! Toggle with config `audit-enabled` (default on).

use crate::bindings::audit::log::recorder;
use crate::bindings::audit::log::types::Event;
use crate::bindings::wasi::random::random::get_random_bytes;
use crate::config;

/// Outcome of an audited decision.
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

/// A 16-hex (8-byte) span id, minted fresh per emitted event.
fn span_id() -> String {
    let b = get_random_bytes(8);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Parse the trace-id out of a W3C traceparent
/// (`00-<32hex trace-id>-<16hex span-id>-<flags>`). Returns None if absent or
/// malformed, in which case a fresh 32-hex trace-id is minted instead.
fn trace_id_from(traceparent: &str) -> String {
    let parts: Vec<&str> = traceparent.split('-').collect();
    if parts.len() == 4 && parts[1].len() == 32 && parts[1].bytes().all(|c| c.is_ascii_hexdigit())
    {
        parts[1].to_string()
    } else {
        let b = get_random_bytes(16);
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
}

/// Emit an audit event with no caller trace context (mints its own ids).
pub fn emit(event: &str, outcome: Outcome, tenant: &str, subject: &str, detail: &str) {
    emit_traced(event, outcome, tenant, subject, detail, "");
}

/// Emit an audit event correlated to a caller's W3C `traceparent`. The line
/// carries `trace_id` (from the parent, or freshly minted) and a fresh
/// `span_id`, so an authz decision joins the originating request's trace.
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
    // id + timestamp are stamped by the recorder; we supply the auth-specific
    // shaping (trace correlation + outcome). Recording failures are non-fatal —
    // an audit-store hiccup must not break the auth decision itself.
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
