//! Structured audit logging of auth decisions.
//!
//! Emits one JSON object per line to stderr — the host captures component
//! stderr, so an OTel/log collector can scrape it. Deliberately records NO
//! secrets: no tokens, no passwords, no refresh tokens. Identifiers (email,
//! subject, tenant) are logged so a decision can be traced to an actor.
//!
//! Toggle with config `audit-enabled` (default on).

use crate::bindings::wasi::clocks::wall_clock;
use crate::bindings::wasi::random::random::get_random_bytes;
use crate::config;

/// A short random correlation id per event, so the lines emitted while serving
/// one request can be grouped in a log/trace backend (the component cannot see
/// the caller's trace context, so it mints its own handle).
fn event_id() -> String {
    let b = get_random_bytes(8);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

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

/// Escape a field value for embedding in our minimal JSON (quotes/backslashes).
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out
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
    let ts = wall_clock::now().seconds;
    let trace_id = trace_id_from(traceparent);
    let span = span_id();
    eprintln!(
        "{{\"audit\":true,\"id\":\"{}\",\"trace_id\":\"{}\",\"span_id\":\"{}\",\"ts\":{},\"event\":\"{}\",\"outcome\":\"{}\",\"tenant\":\"{}\",\"subject\":\"{}\",\"detail\":\"{}\"}}",
        event_id(),
        trace_id,
        span,
        ts,
        esc(event),
        outcome.as_str(),
        esc(tenant),
        esc(subject),
        esc(detail),
    );
}
