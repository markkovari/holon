//! `outbox` — reference implementation of `outbox:dispatch`.
//!
//! A durable, at-least-once event queue implementing the transactional-outbox
//! pattern, backed by `wasi:keyvalue`. The reliable-event problem: an app
//! mutates state and wants to emit an event, but a crash between the two loses
//! or double-fires it. The fix is to *enqueue the event in the same store* as
//! the work, then have a relay claim and dispatch it with explicit ack. This
//! component is the durable-queue half; dispatch (HTTP/NATS/notify) is the
//! relay's job and stays out of scope.
//!
//! Lifecycle:
//!   enqueue → pending → (claim, leased) → in-flight
//!     ├─ ack   → removed from queue
//!     └─ fail  → pending again (with backoff) … or → dead after max-attempts
//!   dead → (replay) → pending
//!
//! Leased claim: `claim` flips a due event to `in-flight` and stamps
//! `not_before = now + lease_seconds` as a lease deadline. Another claimer
//! won't see it until that deadline passes — so a crashed relay's events become
//! claimable again automatically (crash-safe, no stuck events). `ack` deletes
//! the record; `fail` reschedules with exponential backoff
//! (`base-backoff * 2^(attempts-1)`, shift capped to avoid overflow) until
//! `attempts` exceeds `max-attempts`, after which it dead-letters.
//!
//! Storage layout (BUCKET = "default"):
//!   ob_{id}   one tab-delimited line per event (state, attempts, created,
//!             not_before, base64(topic), base64(payload))
//!   ob_idx    newline-joined list of all live event ids, so `claim` /
//!             `dead-letters` can enumerate without a key-scan API.
//!
//! Config (wasi:config/runtime):
//!   max-attempts   retry cap before dead-lettering (default 8)
//!   base-backoff   backoff base seconds, doubled per attempt (default 5)
//!
//! RACE CAVEAT (best-effort, NOT a correctness guarantee): the `ob_idx` index
//! is maintained by read-modify-write. `wasi:keyvalue@0.2.0-draft` exposes no
//! compare-and-swap (only `increment`), so concurrent writers can clobber each
//! other's index updates and drop an id. This reference impl assumes a single
//! writer (the common relay topology). A true fix needs CAS on the index.

#[allow(warnings)]
mod bindings;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use bindings::exports::outbox::dispatch::queue::{Event, Guest, OutboxError, State};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::config::runtime as config;
use bindings::wasi::keyvalue::store as kv;
use bindings::wasi::random::random::get_random_bytes;

struct Component;

const BUCKET: &str = "default";
const INDEX_KEY: &str = "ob_idx";

// ---- config -------------------------------------------------------------

fn max_attempts() -> u32 {
    match config::get("max-attempts") {
        Ok(Some(v)) => v.parse().unwrap_or(8),
        _ => 8,
    }
}

fn base_backoff() -> u64 {
    match config::get("base-backoff") {
        Ok(Some(v)) => v.parse().unwrap_or(5),
        _ => 5,
    }
}

// ---- helpers ------------------------------------------------------------

fn now() -> u64 {
    wall_clock::now().seconds
}

/// A fresh 16-hex id from 8 random bytes (same scheme as idempotency-guard's
/// nonce).
fn new_id() -> String {
    get_random_bytes(8)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// Sanitize an opaque id to NATS-legal kv chars (same scheme as
/// idempotency-guard's `id_key`), prefixed with `ob_`.
fn ob_key(id: &str) -> String {
    let mut out = String::with_capacity(id.len() + 3);
    out.push_str("ob_");
    for b in id.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn open() -> Result<kv::Bucket, OutboxError> {
    kv::open(BUCKET).map_err(|e| OutboxError::BackendUnavailable(format!("open: {e:?}")))
}

fn state_char(s: State) -> char {
    match s {
        State::Pending => 'p',
        State::InFlight => 'i',
        State::Dead => 'd',
    }
}

fn char_state(c: char) -> Result<State, OutboxError> {
    match c {
        'p' => Ok(State::Pending),
        'i' => Ok(State::InFlight),
        'd' => Ok(State::Dead),
        _ => Err(OutboxError::BackendUnavailable("corrupt record: state".into())),
    }
}

// ---- record (de)serialization -------------------------------------------
//
// One tab-delimited line: `{state}\t{attempts}\t{created}\t{not_before}\t
// {b64(topic)}\t{b64(payload)}`. topic + payload are base64'd so arbitrary
// bytes (incl. tabs/newlines) round-trip safely.

fn serialize(ev: &Event) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        state_char(ev.state),
        ev.attempts,
        ev.created,
        ev.not_before,
        B64.encode(ev.topic.as_bytes()),
        B64.encode(&ev.payload),
    )
}

fn parse(id: &str, s: &str) -> Result<Event, OutboxError> {
    let mut parts = s.splitn(6, '\t');
    let state_c = parts
        .next()
        .and_then(|v| v.chars().next())
        .ok_or_else(|| OutboxError::BackendUnavailable("corrupt record: empty".into()))?;
    let state = char_state(state_c)?;
    let attempts = parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| OutboxError::BackendUnavailable("corrupt record: attempts".into()))?;
    let created = parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| OutboxError::BackendUnavailable("corrupt record: created".into()))?;
    let not_before = parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| OutboxError::BackendUnavailable("corrupt record: not_before".into()))?;
    let topic_b64 = parts.next().unwrap_or("");
    let payload_b64 = parts.next().unwrap_or("");
    let topic_bytes = B64
        .decode(topic_b64)
        .map_err(|_| OutboxError::BackendUnavailable("corrupt record: topic".into()))?;
    let topic = String::from_utf8(topic_bytes)
        .map_err(|_| OutboxError::BackendUnavailable("corrupt record: topic utf-8".into()))?;
    let payload = B64
        .decode(payload_b64)
        .map_err(|_| OutboxError::BackendUnavailable("corrupt record: payload".into()))?;
    Ok(Event {
        id: id.to_string(),
        topic,
        payload,
        state,
        attempts,
        created,
        not_before,
    })
}

fn load(bucket: &kv::Bucket, id: &str) -> Result<Option<Event>, OutboxError> {
    let k = ob_key(id);
    match bucket.get(&k) {
        Ok(Some(bytes)) => {
            let s = String::from_utf8(bytes)
                .map_err(|_| OutboxError::BackendUnavailable("value not utf-8".into()))?;
            Ok(Some(parse(id, &s)?))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(OutboxError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

fn store(bucket: &kv::Bucket, ev: &Event) -> Result<(), OutboxError> {
    bucket
        .set(&ob_key(&ev.id), serialize(ev).as_bytes())
        .map_err(|e| OutboxError::BackendUnavailable(format!("set: {e:?}")))
}

// ---- index (best-effort, single-writer) ---------------------------------

fn index_read(bucket: &kv::Bucket) -> Result<Vec<String>, OutboxError> {
    match bucket.get(INDEX_KEY) {
        Ok(Some(bytes)) => {
            let s = String::from_utf8(bytes)
                .map_err(|_| OutboxError::BackendUnavailable("index not utf-8".into()))?;
            Ok(s.lines().filter(|l| !l.is_empty()).map(String::from).collect())
        }
        Ok(None) => Ok(Vec::new()),
        Err(e) => Err(OutboxError::BackendUnavailable(format!("get index: {e:?}"))),
    }
}

fn index_write(bucket: &kv::Bucket, ids: &[String]) -> Result<(), OutboxError> {
    bucket
        .set(INDEX_KEY, ids.join("\n").as_bytes())
        .map_err(|e| OutboxError::BackendUnavailable(format!("set index: {e:?}")))
}

fn index_add(bucket: &kv::Bucket, id: &str) -> Result<(), OutboxError> {
    let mut ids = index_read(bucket)?;
    if !ids.iter().any(|x| x == id) {
        ids.push(id.to_string());
        index_write(bucket, &ids)?;
    }
    Ok(())
}

fn index_remove(bucket: &kv::Bucket, id: &str) -> Result<(), OutboxError> {
    let mut ids = index_read(bucket)?;
    let before = ids.len();
    ids.retain(|x| x != id);
    if ids.len() != before {
        index_write(bucket, &ids)?;
    }
    Ok(())
}

impl Guest for Component {
    fn enqueue(topic: String, payload: Vec<u8>, delay_seconds: u64) -> Result<String, OutboxError> {
        let bucket = open()?;
        let id = new_id();
        let now = now();
        let ev = Event {
            id: id.clone(),
            topic,
            payload,
            state: State::Pending,
            attempts: 0,
            created: now,
            not_before: now.saturating_add(delay_seconds),
        };
        store(&bucket, &ev)?;
        index_add(&bucket, &id)?;
        Ok(id)
    }

    fn claim(max: u32, lease_seconds: u64) -> Result<Vec<Event>, OutboxError> {
        let bucket = open()?;
        let now = now();
        let ids = index_read(&bucket)?;
        let mut claimed = Vec::new();
        for id in ids {
            if claimed.len() >= max as usize {
                break;
            }
            let mut ev = match load(&bucket, &id)? {
                Some(ev) => ev,
                None => continue,
            };
            // Eligible: a due pending event, or an in-flight event whose lease
            // (stored as not_before while in-flight) has expired.
            let eligible = match ev.state {
                State::Pending => now >= ev.not_before,
                State::InFlight => now >= ev.not_before,
                State::Dead => false,
            };
            if !eligible {
                continue;
            }
            ev.state = State::InFlight;
            ev.not_before = now.saturating_add(lease_seconds);
            store(&bucket, &ev)?;
            claimed.push(ev);
        }
        Ok(claimed)
    }

    fn ack(id: String) -> Result<(), OutboxError> {
        let bucket = open()?;
        if load(&bucket, &id)?.is_none() {
            return Err(OutboxError::NotFound);
        }
        bucket
            .delete(&ob_key(&id))
            .map_err(|e| OutboxError::BackendUnavailable(format!("delete: {e:?}")))?;
        index_remove(&bucket, &id)?;
        Ok(())
    }

    fn fail(id: String) -> Result<State, OutboxError> {
        let bucket = open()?;
        let mut ev = load(&bucket, &id)?.ok_or(OutboxError::NotFound)?;
        let now = now();
        ev.attempts = ev.attempts.saturating_add(1);
        if ev.attempts > max_attempts() {
            // Exhausted retries — dead-letter it (kept in the index for
            // inspection / replay).
            ev.state = State::Dead;
        } else {
            // Exponential backoff: base * 2^(attempts-1), shift capped so the
            // multiply can't overflow.
            ev.state = State::Pending;
            let shift = (ev.attempts - 1).min(16);
            let backoff = base_backoff().saturating_mul(1u64 << shift);
            ev.not_before = now.saturating_add(backoff);
        }
        store(&bucket, &ev)?;
        Ok(ev.state)
    }

    fn dead_letters(max: u32) -> Result<Vec<Event>, OutboxError> {
        let bucket = open()?;
        let ids = index_read(&bucket)?;
        let mut out = Vec::new();
        for id in ids {
            if out.len() >= max as usize {
                break;
            }
            if let Some(ev) = load(&bucket, &id)? {
                if ev.state == State::Dead {
                    out.push(ev);
                }
            }
        }
        Ok(out)
    }

    fn replay(id: String) -> Result<(), OutboxError> {
        let bucket = open()?;
        let mut ev = load(&bucket, &id)?.ok_or(OutboxError::NotFound)?;
        if ev.state != State::Dead {
            return Err(OutboxError::NotFound);
        }
        ev.state = State::Pending;
        ev.not_before = now();
        store(&bucket, &ev)?;
        Ok(())
    }
}

bindings::export!(Component with_types_in bindings);
