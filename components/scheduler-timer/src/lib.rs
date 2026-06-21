//! `scheduler-timer` — reference implementation of `sched:timer`.
//!
//! A durable store of future jobs, backed by `wasi:keyvalue`. The "do this
//! later" problem: send a reminder 24h before an appointment, retry in an hour,
//! sweep nightly. This component owns the *when* — eligibility, recurrence,
//! leasing — and a relay owns the *what* (it `due`-polls, does the work,
//! `ack`s one-shots). Dispatch stays out of scope, so the component is pure
//! WASI and composes with any sink (notify:dispatch, outbox, HTTP).
//!
//! Lifecycle:
//!   schedule-at / schedule-every  → job stored, keyed by app `key`
//!   due(now)                      → eligible jobs leased + returned:
//!       once : leased (lease_until = now + lease); awaits `ack` to be removed
//!       every: run_at advanced to the NEXT future slot at fire time, fires++
//!   ack(key)                      → removes a fired one-shot (no-op on recurring)
//!   cancel(key)                   → removes any job
//!
//! Leased due: a one-shot returned by `due` gets `lease_until = now + lease`,
//! and won't be returned again until that passes — so a crashed relay's job
//! becomes due again (crash-safe, at-least-once). Recurring jobs don't lease;
//! they advance run_at immediately, so they're naturally not re-returned until
//! the next interval.
//!
//! Recurrence catch-up: a recurring job whose run_at is far in the past
//! advances to the next slot STRICTLY AFTER `now` (run_at += period*k until
//! > now), so a relay that was down for a day fires the job once, not a burst
//! of a day's backlog.
//!
//! Idempotent keying: `schedule-*` REPLACES any existing job with the same key,
//! so "nightly-sweep" scheduled on every boot never accumulates duplicates.
//!
//! Storage layout (BUCKET = "default"):
//!   tm_{key}  one tab-delimited line per job (kind, run_at, period, fires,
//!             lease_until, base64(payload))
//!   tm_idx    newline-joined list of all live job keys, so `due` / `list-jobs`
//!             can enumerate without a key-scan API.
//!
//! RACE CAVEAT (best-effort, NOT a correctness guarantee): the `tm_idx` index
//! is maintained by read-modify-write. `wasi:keyvalue@0.2.0-draft` exposes no
//! compare-and-swap, so concurrent writers can clobber each other's index
//! updates. This reference impl assumes a single relay (the common topology).

#[allow(warnings)]
mod bindings;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use bindings::exports::sched::timer::timer::{Guest, Job, Kind, TimerError};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";
const INDEX_KEY: &str = "tm_idx";

// ---- helpers ------------------------------------------------------------

fn now_clock() -> u64 {
    wall_clock::now().seconds
}

/// Sanitize an app key to NATS-legal kv chars (same scheme as outbox's
/// `ob_key`), prefixed with `tm_`.
fn tm_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 3);
    out.push_str("tm_");
    for b in key.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn open() -> Result<kv::Bucket, TimerError> {
    kv::open(BUCKET).map_err(|e| TimerError::BackendUnavailable(format!("open: {e:?}")))
}

fn kind_char(k: Kind) -> char {
    match k {
        Kind::Once => 'o',
        Kind::Every => 'e',
    }
}

fn char_kind(c: char) -> Result<Kind, TimerError> {
    match c {
        'o' => Ok(Kind::Once),
        'e' => Ok(Kind::Every),
        _ => Err(TimerError::BackendUnavailable("corrupt record: kind".into())),
    }
}

// ---- record (de)serialization -------------------------------------------
//
// One tab-delimited line: `{kind}\t{run_at}\t{period}\t{fires}\t{lease_until}\t
// {b64(payload)}`. lease_until is the in-flight lease deadline for a one-shot
// (0 = not leased); payload is base64'd so arbitrary bytes round-trip safely.

struct Stored {
    job: Job,
    lease_until: u64,
}

fn serialize(s: &Stored) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        kind_char(s.job.kind),
        s.job.run_at,
        s.job.period_seconds,
        s.job.fires,
        s.lease_until,
        B64.encode(&s.job.payload),
    )
}

fn parse(key: &str, s: &str) -> Result<Stored, TimerError> {
    let mut parts = s.splitn(6, '\t');
    let kind_c = parts
        .next()
        .and_then(|v| v.chars().next())
        .ok_or_else(|| TimerError::BackendUnavailable("corrupt record: empty".into()))?;
    let kind = char_kind(kind_c)?;
    let run_at = parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| TimerError::BackendUnavailable("corrupt record: run_at".into()))?;
    let period_seconds = parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| TimerError::BackendUnavailable("corrupt record: period".into()))?;
    let fires = parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| TimerError::BackendUnavailable("corrupt record: fires".into()))?;
    let lease_until = parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| TimerError::BackendUnavailable("corrupt record: lease".into()))?;
    let payload_b64 = parts.next().unwrap_or("");
    let payload = B64
        .decode(payload_b64)
        .map_err(|_| TimerError::BackendUnavailable("corrupt record: payload".into()))?;
    Ok(Stored {
        job: Job {
            key: key.to_string(),
            payload,
            kind,
            run_at,
            period_seconds,
            fires,
        },
        lease_until,
    })
}

fn load(bucket: &kv::Bucket, key: &str) -> Result<Option<Stored>, TimerError> {
    match bucket.get(&tm_key(key)) {
        Ok(Some(bytes)) => {
            let s = String::from_utf8(bytes)
                .map_err(|_| TimerError::BackendUnavailable("value not utf-8".into()))?;
            Ok(Some(parse(key, &s)?))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(TimerError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

fn store(bucket: &kv::Bucket, s: &Stored) -> Result<(), TimerError> {
    bucket
        .set(&tm_key(&s.job.key), serialize(s).as_bytes())
        .map_err(|e| TimerError::BackendUnavailable(format!("set: {e:?}")))
}

// ---- index (best-effort, single-writer) ---------------------------------

fn index_read(bucket: &kv::Bucket) -> Result<Vec<String>, TimerError> {
    match bucket.get(INDEX_KEY) {
        Ok(Some(bytes)) => {
            let s = String::from_utf8(bytes)
                .map_err(|_| TimerError::BackendUnavailable("index not utf-8".into()))?;
            Ok(s.lines().filter(|l| !l.is_empty()).map(String::from).collect())
        }
        Ok(None) => Ok(Vec::new()),
        Err(e) => Err(TimerError::BackendUnavailable(format!("get index: {e:?}"))),
    }
}

fn index_write(bucket: &kv::Bucket, keys: &[String]) -> Result<(), TimerError> {
    bucket
        .set(INDEX_KEY, keys.join("\n").as_bytes())
        .map_err(|e| TimerError::BackendUnavailable(format!("set index: {e:?}")))
}

fn index_add(bucket: &kv::Bucket, key: &str) -> Result<(), TimerError> {
    let mut keys = index_read(bucket)?;
    if !keys.iter().any(|x| x == key) {
        keys.push(key.to_string());
        index_write(bucket, &keys)?;
    }
    Ok(())
}

fn index_remove(bucket: &kv::Bucket, key: &str) -> Result<(), TimerError> {
    let mut keys = index_read(bucket)?;
    let before = keys.len();
    keys.retain(|x| x != key);
    if keys.len() != before {
        index_write(bucket, &keys)?;
    }
    Ok(())
}

/// Remove a job entirely (record + index entry).
fn remove(bucket: &kv::Bucket, key: &str) -> Result<(), TimerError> {
    bucket
        .delete(&tm_key(key))
        .map_err(|e| TimerError::BackendUnavailable(format!("delete: {e:?}")))?;
    index_remove(bucket, key)
}

// ---- guest --------------------------------------------------------------

impl Guest for Component {
    fn schedule_at(key: String, run_at: u64, payload: Vec<u8>) -> Result<(), TimerError> {
        let bucket = open()?;
        let stored = Stored {
            job: Job {
                key: key.clone(),
                payload,
                kind: Kind::Once,
                run_at,
                period_seconds: 0,
                fires: 0,
            },
            lease_until: 0,
        };
        store(&bucket, &stored)?;
        index_add(&bucket, &key)
    }

    fn schedule_every(
        key: String,
        period_seconds: u64,
        first_run_at: u64,
        payload: Vec<u8>,
    ) -> Result<(), TimerError> {
        if period_seconds == 0 {
            return Err(TimerError::InvalidPeriod);
        }
        let bucket = open()?;
        // first_run_at == 0 means "now + period".
        let run_at = if first_run_at == 0 {
            now_clock().saturating_add(period_seconds)
        } else {
            first_run_at
        };
        let stored = Stored {
            job: Job {
                key: key.clone(),
                payload,
                kind: Kind::Every,
                run_at,
                period_seconds,
                fires: 0,
            },
            lease_until: 0,
        };
        store(&bucket, &stored)?;
        index_add(&bucket, &key)
    }

    fn due(now: u64, max: u32, lease_seconds: u64) -> Result<Vec<Job>, TimerError> {
        let bucket = open()?;
        let keys = index_read(&bucket)?;
        let mut out = Vec::new();
        for key in keys {
            if out.len() >= max as usize {
                break;
            }
            let mut stored = match load(&bucket, &key)? {
                Some(s) => s,
                None => continue,
            };
            // Eligible when run_at has arrived AND (for a one-shot) no live
            // lease holds it.
            let leased = stored.lease_until > now;
            if now < stored.job.run_at || leased {
                continue;
            }
            match stored.job.kind {
                Kind::Once => {
                    // Lease it; it stays until `ack`. fires bumps so a re-fire
                    // after a lease lapse is observable.
                    stored.job.fires = stored.job.fires.saturating_add(1);
                    stored.lease_until = now.saturating_add(lease_seconds);
                    store(&bucket, &stored)?;
                    out.push(stored.job);
                }
                Kind::Every => {
                    // Advance run_at to the next slot STRICTLY after `now`
                    // (catch-up: skip a backlog of missed windows, fire once).
                    let period = stored.job.period_seconds.max(1);
                    let behind = now.saturating_sub(stored.job.run_at);
                    let steps = behind / period + 1;
                    stored.job.run_at = stored
                        .job
                        .run_at
                        .saturating_add(steps.saturating_mul(period));
                    stored.job.fires = stored.job.fires.saturating_add(1);
                    store(&bucket, &stored)?;
                    out.push(stored.job);
                }
            }
        }
        Ok(out)
    }

    fn ack(key: String) -> Result<(), TimerError> {
        let bucket = open()?;
        let stored = load(&bucket, &key)?.ok_or(TimerError::NotFound)?;
        match stored.job.kind {
            // A fired one-shot is removed on ack.
            Kind::Once => remove(&bucket, &key),
            // Recurring jobs need no ack — already advanced.
            Kind::Every => Ok(()),
        }
    }

    fn cancel(key: String) -> Result<(), TimerError> {
        let bucket = open()?;
        if load(&bucket, &key)?.is_none() {
            return Err(TimerError::NotFound);
        }
        remove(&bucket, &key)
    }

    fn peek(key: String) -> Result<Option<Job>, TimerError> {
        let bucket = open()?;
        Ok(load(&bucket, &key)?.map(|s| s.job))
    }

    fn list_jobs(max: u32) -> Result<Vec<Job>, TimerError> {
        let bucket = open()?;
        let keys = index_read(&bucket)?;
        let mut out = Vec::new();
        for key in keys {
            if out.len() >= max as usize {
                break;
            }
            if let Some(s) = load(&bucket, &key)? {
                out.push(s.job);
            }
        }
        Ok(out)
    }
}

bindings::export!(Component with_types_in bindings);
