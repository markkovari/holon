//! `event-bus` — reference implementation of `event:bus`.
//!
//! A durable topic log with per-consumer-group offsets, backed by
//! `wasi:keyvalue`. Producers `publish` to a topic; independent consumer groups
//! `poll` at their own pace and `ack` what they processed. Per-group offsets
//! mean a slow/new group still sees past events and groups never steal each
//! other's events (fan-out, not a work queue). At-least-once: an event stays
//! visible to a group until that group acks it.
//!
//! Storage layout (BUCKET = "default"):
//!   eb_seq_{topic}        monotonic publish counter (via atomics.increment).
//!   eb_ev_{topic}_{seq}   one event: `at\tb64(payload)` (seq is the id).
//!   eb_off_{topic}_{grp}  highest seq this group has acked (0 = nothing yet).
//!   eb_topics             newline-joined topic names, for `topics()`.
//!
//! poll(topic, group, max): returns events with seq in (offset, max_seq],
//! oldest first, up to `max`. ack(topic, group, ids): moves the offset to the
//! MAX acked id (monotonic; acking lower ids is a no-op). pending = max_seq -
//! offset.
//!
//! RACE CAVEAT (best-effort): the topic index + offsets are read-modify-write;
//! the per-topic sequence uses atomics.increment so ids never collide, but
//! concurrent ack of the same group can clobber an offset advance. Single
//! writer per group is the assumed topology.

#[allow(warnings)]
mod bindings;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use bindings::exports::event::bus::bus::{BusError, Event, Guest};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::atomics;
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";
const TOPICS_KEY: &str = "eb_topics";

// ---- helpers ------------------------------------------------------------

fn now() -> u64 {
    wall_clock::now().seconds
}

/// Sanitize a topic/group to NATS-legal kv chars (so it composes into a key).
fn safe(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn seq_key(topic: &str) -> String {
    format!("eb_seq_{}", safe(topic))
}
fn ev_key(topic: &str, seq: u64) -> String {
    // zero-pad so lexical order matches numeric order (not strictly needed —
    // we iterate by number — but keeps the store browsable).
    format!("eb_ev_{}_{:020}", safe(topic), seq)
}
fn off_key(topic: &str, group: &str) -> String {
    format!("eb_off_{}_{}", safe(topic), safe(group))
}

fn open() -> Result<kv::Bucket, BusError> {
    kv::open(BUCKET).map_err(|e| BusError::BackendUnavailable(format!("open: {e:?}")))
}

fn get_u64(bucket: &kv::Bucket, key: &str) -> Result<u64, BusError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => Ok(String::from_utf8(bytes)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)),
        Ok(None) => Ok(0),
        Err(e) => Err(BusError::BackendUnavailable(format!("get {key}: {e:?}"))),
    }
}

fn set_u64(bucket: &kv::Bucket, key: &str, v: u64) -> Result<(), BusError> {
    bucket
        .set(key, v.to_string().as_bytes())
        .map_err(|e| BusError::BackendUnavailable(format!("set {key}: {e:?}")))
}

// ---- topic index --------------------------------------------------------

fn topics_read(bucket: &kv::Bucket) -> Result<Vec<String>, BusError> {
    match bucket.get(TOPICS_KEY) {
        Ok(Some(bytes)) => {
            let s = String::from_utf8(bytes)
                .map_err(|_| BusError::BackendUnavailable("topics not utf-8".into()))?;
            Ok(s.lines().filter(|l| !l.is_empty()).map(String::from).collect())
        }
        Ok(None) => Ok(Vec::new()),
        Err(e) => Err(BusError::BackendUnavailable(format!("get topics: {e:?}"))),
    }
}

fn topics_add(bucket: &kv::Bucket, topic: &str) -> Result<(), BusError> {
    let mut ts = topics_read(bucket)?;
    if !ts.iter().any(|t| t == topic) {
        ts.push(topic.to_string());
        bucket
            .set(TOPICS_KEY, ts.join("\n").as_bytes())
            .map_err(|e| BusError::BackendUnavailable(format!("set topics: {e:?}")))?;
    }
    Ok(())
}

// ---- event (de)serialization --------------------------------------------
// One line: `at\tb64(payload)`.

fn parse_event(topic: &str, seq: u64, s: &str) -> Result<Event, BusError> {
    let mut p = s.splitn(2, '\t');
    let at = p
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| BusError::BackendUnavailable("corrupt event: at".into()))?;
    let payload = B64
        .decode(p.next().unwrap_or(""))
        .map_err(|_| BusError::BackendUnavailable("corrupt event: payload".into()))?;
    Ok(Event {
        id: seq.to_string(),
        topic: topic.to_string(),
        payload,
        at,
    })
}

// ---- guest --------------------------------------------------------------

impl Guest for Component {
    fn publish(topic: String, payload: Vec<u8>) -> Result<String, BusError> {
        let bucket = open()?;
        // atomic per-topic sequence: increment returns the new value, so ids
        // never collide even under concurrent publish.
        let seq = atomics::increment(&bucket, &seq_key(&topic), 1)
            .map_err(|e| BusError::BackendUnavailable(format!("increment: {e:?}")))?;
        let line = format!("{}\t{}", now(), B64.encode(&payload));
        bucket
            .set(&ev_key(&topic, seq), line.as_bytes())
            .map_err(|e| BusError::BackendUnavailable(format!("set event: {e:?}")))?;
        topics_add(&bucket, &topic)?;
        Ok(seq.to_string())
    }

    fn poll(topic: String, group: String, max: u32) -> Result<Vec<Event>, BusError> {
        let bucket = open()?;
        let max_seq = get_u64(&bucket, &seq_key(&topic))?;
        let offset = get_u64(&bucket, &off_key(&topic, &group))?;
        let mut out = Vec::new();
        let mut seq = offset + 1;
        while seq <= max_seq && out.len() < max as usize {
            if let Ok(Some(bytes)) = bucket.get(&ev_key(&topic, seq)) {
                let s = String::from_utf8(bytes)
                    .map_err(|_| BusError::BackendUnavailable("event not utf-8".into()))?;
                out.push(parse_event(&topic, seq, &s)?);
            }
            seq += 1;
        }
        Ok(out)
    }

    fn ack(topic: String, group: String, ids: Vec<String>) -> Result<(), BusError> {
        let bucket = open()?;
        let key = off_key(&topic, &group);
        let current = get_u64(&bucket, &key)?;
        // advance to the highest acked id (monotonic; lower acks are no-ops).
        let highest = ids
            .iter()
            .filter_map(|i| i.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        if highest > current {
            set_u64(&bucket, &key, highest)?;
        }
        Ok(())
    }

    fn pending(topic: String, group: String) -> Result<u64, BusError> {
        let bucket = open()?;
        let max_seq = get_u64(&bucket, &seq_key(&topic))?;
        let offset = get_u64(&bucket, &off_key(&topic, &group))?;
        Ok(max_seq.saturating_sub(offset))
    }

    fn topics() -> Result<Vec<String>, BusError> {
        let bucket = open()?;
        topics_read(&bucket)
    }
}

bindings::export!(Component with_types_in bindings);
