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

/// Emit an audit event. `event` is the action (e.g. "authorize", "login");
/// `subject`/`tenant` identify the actor ("" if unknown); `detail` is a short,
/// secret-free reason (e.g. "insufficient_scope", "orders:read").
pub fn emit(event: &str, outcome: Outcome, tenant: &str, subject: &str, detail: &str) {
    if !config::audit_enabled() {
        return;
    }
    let ts = wall_clock::now().seconds;
    // One compact JSON line. `audit:true` marks the line for log filters; `id`
    // is a correlation handle for grouping in a trace/log backend.
    eprintln!(
        "{{\"audit\":true,\"id\":\"{}\",\"ts\":{},\"event\":\"{}\",\"outcome\":\"{}\",\"tenant\":\"{}\",\"subject\":\"{}\",\"detail\":\"{}\"}}",
        event_id(),
        ts,
        esc(event),
        outcome.as_str(),
        esc(tenant),
        esc(subject),
        esc(detail),
    );
}
