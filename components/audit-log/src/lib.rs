//! `audit-log` — reference implementation of `audit:log`.
//!
//! Append-only audit trail, backed by `wasi:keyvalue` AND echoed to stderr (so
//! an existing OTel/log-scrape pipeline keeps working while the trail also
//! becomes queryable). Each event is stored as JSON at key
//! `al_{ts:020}_{id}` — the zero-padded timestamp makes a lexicographic key
//! scan chronological, so "newest first" is just a reverse.
//!
//! Records NO secrets — identifiers only (the contract's `event` shape).

#[allow(warnings)]
mod bindings;

use bindings::audit::log::types::{AuditError, Event};
use bindings::exports::audit::log::query::Guest as Query;
use bindings::exports::audit::log::recorder::Guest as Recorder;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::store as kv;
use bindings::wasi::random::random::get_random_bytes;

use serde::{Deserialize, Serialize};

struct Component;

const BUCKET: &str = "default";

// ---- serializable mirror of the WIT `event` -----------------------------

#[derive(Serialize, Deserialize)]
struct Stored {
    id: String,
    trace_id: String,
    span_id: String,
    timestamp: u64,
    event: String,
    outcome: String,
    tenant: String,
    subject: String,
    detail: String,
}

impl From<&Stored> for Event {
    fn from(s: &Stored) -> Self {
        Event {
            id: s.id.clone(),
            trace_id: s.trace_id.clone(),
            span_id: s.span_id.clone(),
            timestamp: s.timestamp,
            event: s.event.clone(),
            outcome: s.outcome.clone(),
            tenant: s.tenant.clone(),
            subject: s.subject.clone(),
            detail: s.detail.clone(),
        }
    }
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn hex(n: usize) -> String {
    get_random_bytes(n as u64)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn open() -> Result<kv::Bucket, AuditError> {
    kv::open(BUCKET).map_err(|e| AuditError::BackendUnavailable(format!("open: {e:?}")))
}

/// All stored events, parsed, sorted newest-first by (timestamp, key).
fn scan(bucket: &kv::Bucket) -> Result<Vec<Stored>, AuditError> {
    let mut out: Vec<(String, Stored)> = Vec::new();
    let mut cursor: Option<u64> = None;
    loop {
        let page = bucket
            .list_keys(cursor)
            .map_err(|e| AuditError::BackendUnavailable(format!("list-keys: {e:?}")))?;
        for key in &page.keys {
            if !key.starts_with("al_") {
                continue;
            }
            if let Ok(Some(bytes)) = bucket.get(key) {
                if let Ok(s) = serde_json::from_slice::<Stored>(&bytes) {
                    out.push((key.clone(), s));
                }
            }
        }
        match page.cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    // Keys are `al_{ts:020}_{id}`; descending key order == newest first.
    out.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(out.into_iter().map(|(_, s)| s).collect())
}

impl Recorder for Component {
    fn record_event(mut e: Event) -> Result<(), AuditError> {
        if e.id.is_empty() {
            e.id = hex(8);
        }
        if e.timestamp == 0 {
            e.timestamp = now();
        }
        let stored = Stored {
            id: e.id,
            trace_id: e.trace_id,
            span_id: e.span_id,
            timestamp: e.timestamp,
            event: e.event,
            outcome: e.outcome,
            tenant: e.tenant,
            subject: e.subject,
            detail: e.detail,
        };
        // Echo to stderr (preserves the existing OTel/scrape path).
        if let Ok(line) = serde_json::to_string(&stored) {
            eprintln!("{{\"audit\":true,{}}}", &line[1..line.len() - 1]);
        }
        let body = serde_json::to_vec(&stored)
            .map_err(|e| AuditError::BackendUnavailable(format!("encode: {e}")))?;
        let key = format!("al_{:020}_{}", stored.timestamp, stored.id);
        open()?
            .set(&key, &body)
            .map_err(|e| AuditError::BackendUnavailable(format!("set: {e:?}")))
    }
}

impl Query for Component {
    fn recent(limit: u32) -> Result<Vec<Event>, AuditError> {
        let all = scan(&open()?)?;
        Ok(all
            .iter()
            .take(limit as usize)
            .map(Event::from)
            .collect())
    }

    fn by_trace(trace_id: String) -> Result<Vec<Event>, AuditError> {
        let all = scan(&open()?)?;
        Ok(all
            .iter()
            .filter(|s| s.trace_id == trace_id)
            .map(Event::from)
            .collect())
    }
}

bindings::export!(Component with_types_in bindings);
