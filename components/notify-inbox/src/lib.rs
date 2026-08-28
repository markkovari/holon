//! `notify-inbox` — the in-app channel: durable, per subject, with unread state.
//!
//! ## Two counters, because `kv:atomics` can only add
//!
//! `increment(bucket, key, delta: u64)` has no negative delta, so an unread count
//! kept as one number cannot be decremented when something is read. Instead there
//! are two monotonic counters, `d:` delivered and `r:` read, and the badge is
//! `delivered - read`. Both only ever go up, both are atomic, and no read-modify-write
//! races with a concurrent delivery.
//!
//! ## The cursor is the delivered counter
//!
//! A note's `seq` is its ordinal within one subject's inbox, which makes `since(after:
//! n)` both "the next page" and "everything since I last looked" — the same number
//! serves paging and a live SSE tail. There is no scan and no `list-keys`: the
//! highest seq is the delivered counter, so listing is a bounded walk of keys that
//! are known to exist.

#[allow(warnings)]
mod bindings;

use bindings::exports::notify::inbox::inbox::{Guest, InboxError, Note};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::atomics;
use bindings::wasi::keyvalue::store as kv;
use serde_json::json;

/// The bucket the platform assigned. Not a name of this component's choosing:
/// `comp-host` only opens buckets the scope granted, and a miss is `no-such-store`
/// rather than a fallback — "a guest naming its neighbour's bucket gets A bucket
/// rather than an error, which is the same class of bug wearing an apology". So
/// every kv component here opens `default` and separates itself by key prefix,
/// which is what `n:`/`d:`/`r:` below are for.
///
/// Separating TENANTS is the caller's job, inside the subject string — the same
/// choice `quota:meter` and `records:store` make, and the reason none of them
/// import an identity.
const BUCKET: &str = "default";

struct Component;

fn open() -> Result<kv::Bucket, InboxError> {
    kv::open(BUCKET).map_err(|e| InboxError::BackendUnavailable(format!("open: {e:?}")))
}

fn back(ctx: &str) -> impl Fn(kv::Error) -> InboxError + '_ {
    move |e| InboxError::BackendUnavailable(format!("{ctx}: {e:?}"))
}

/// Zero-padded so the keys sort lexically in the order they were written, which is
/// what makes a `list-keys` scan unnecessary but also keeps a store dump readable.
fn note_key(subject: &str, seq: u64) -> String {
    format!("n:{subject}:{seq:020}")
}
fn delivered_key(subject: &str) -> String {
    format!("d:{subject}")
}
fn read_key(subject: &str) -> String {
    format!("r:{subject}")
}

/// `increment(0)` reads a counter without changing it — there is no plain `get` for
/// an atomic, and reading the raw key would see whatever encoding the host chose.
fn counter(bucket: &kv::Bucket, key: &str) -> Result<u64, InboxError> {
    atomics::increment(bucket, key, 0).map_err(back("counter"))
}

fn now() -> u64 {
    wall_clock::now().seconds
}

impl Guest for Component {
    fn deliver(
        subject: String,
        kind: String,
        title: String,
        body: String,
        payload: String,
    ) -> Result<u64, InboxError> {
        if subject.is_empty() {
            return Err(InboxError::Invalid("subject is empty".into()));
        }
        let bucket = open()?;
        // The seq is claimed atomically FIRST. Two concurrent deliveries to one
        // subject get different numbers; writing the note then claiming a number
        // would let them collide on a key.
        let seq =
            atomics::increment(&bucket, &delivered_key(&subject), 1).map_err(back("seq"))?;
        let doc = json!({
            "seq": seq, "kind": kind, "title": title, "body": body,
            "payload": payload, "at": now(), "read": false,
        });
        kv::Bucket::set(&bucket, &note_key(&subject, seq), doc.to_string().as_bytes())
            .map_err(back("set"))?;
        Ok(seq)
    }

    fn since(subject: String, after: u64, limit: u32) -> Result<Vec<Note>, InboxError> {
        if subject.is_empty() {
            return Err(InboxError::Invalid("subject is empty".into()));
        }
        if limit == 0 {
            return Err(InboxError::Invalid("limit is zero".into()));
        }
        let bucket = open()?;
        let highest = counter(&bucket, &delivered_key(&subject))?;
        let mut out = Vec::new();
        // Oldest first from `after`, so a tail appends and a page continues. A gap
        // (a note deleted, or a seq claimed by a delivery that then failed to write)
        // is skipped rather than ending the walk.
        for seq in (after + 1)..=highest {
            if out.len() >= limit as usize {
                break;
            }
            let Ok(Some(raw)) = kv::Bucket::get(&bucket, &note_key(&subject, seq)) else {
                continue;
            };
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else { continue };
            out.push(Note {
                seq,
                kind: v["kind"].as_str().unwrap_or_default().to_string(),
                title: v["title"].as_str().unwrap_or_default().to_string(),
                body: v["body"].as_str().unwrap_or_default().to_string(),
                payload: v["payload"].as_str().unwrap_or_default().to_string(),
                at: v["at"].as_u64().unwrap_or(0),
                read: v["read"].as_bool().unwrap_or(false),
            });
        }
        Ok(out)
    }

    fn unread_count(subject: String) -> Result<u64, InboxError> {
        let bucket = open()?;
        let delivered = counter(&bucket, &delivered_key(&subject))?;
        let read = counter(&bucket, &read_key(&subject))?;
        // Saturating, not subtracting: if the two ever disagreed the badge should
        // read zero rather than four billion.
        Ok(delivered.saturating_sub(read))
    }

    fn mark_read(subject: String, seqs: Vec<u64>) -> Result<u64, InboxError> {
        let bucket = open()?;
        let mut newly = 0u64;
        for seq in seqs {
            let key = note_key(&subject, seq);
            let Ok(Some(raw)) = kv::Bucket::get(&bucket, &key) else { continue };
            let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(&raw) else { continue };
            // Already read is not an error and not a second decrement: a client that
            // retries must not drive the badge below what is actually unread.
            if v["read"].as_bool().unwrap_or(false) {
                continue;
            }
            v["read"] = json!(true);
            if kv::Bucket::set(&bucket, &key, v.to_string().as_bytes()).is_ok() {
                newly += 1;
            }
        }
        if newly > 0 {
            atomics::increment(&bucket, &read_key(&subject), newly).map_err(back("read"))?;
        }
        Ok(newly)
    }

    fn mark_all_read(subject: String, through: u64) -> Result<u64, InboxError> {
        let bucket = open()?;
        let highest = counter(&bucket, &delivered_key(&subject))?;
        let ceiling = if through == 0 { highest } else { through.min(highest) };
        let all: Vec<u64> = (1..=ceiling).collect();
        Self::mark_read(subject, all)
    }
}

bindings::export!(Component with_types_in bindings);
