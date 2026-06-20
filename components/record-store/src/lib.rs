//! `record-store` — reference implementation of `record:store`.
//!
//! Typed JSON records in named COLLECTIONS, the primitive every app
//! reimplements as glue. Each record is an opaque JSON object string keyed by
//! an auto-minted ULID; the component owns the storage shape (collection
//! prefixes, id minting, index maintenance), the app owns the schema.
//!
//! Why ULIDs: a ULID's 48-bit time prefix makes its Crockford-base32 encoding
//! sort lexicographically by creation time. So the per-collection id index is
//! kept SORTED and is therefore time-ordered for free — `list` paginates over
//! it and `count` is just its length.
//!
//! Secondary INDEXES turn "all pets owned by X" into an O(matches) lookup
//! instead of an O(n) scan over every record. For each configured index field
//! `F` with JSON value `V`, a key `ix_{collection}_{F}_{sanitize(V)}` holds the
//! list of matching ids. Maintained on create / update / delete. Because `V` is
//! sanitized and length-capped into the key, distinct values *can* collide onto
//! one index key — that only ever OVER-matches, so `find-by` always re-verifies
//! the record's actual `field == value` before returning it.
//!
//! Optimistic locking: every record carries a monotonic `revision`; `update`
//! with a non-zero `expected-revision` that no longer matches yields
//! `revision-conflict(current)`.
//!
//! Storage is `wasi:keyvalue` + `wasi:clocks` (id time) + `wasi:random` (id
//! entropy). All index maintenance is read-modify-write, single-writer
//! best-effort: a tight concurrent interleaving on the same index key can drop
//! or duplicate an id, since wasi:keyvalue@0.2.0-draft exposes no
//! compare-and-swap. The record values themselves are authoritative; the
//! indexes are an acceleration layer that `find-by`/`query` re-verify against
//! the records.

#[allow(warnings)]
mod bindings;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use bindings::exports::records::store::store::{Entry, Filter, Guest, Page, StoreError};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::store as kv;
use bindings::wasi::random::random::get_random_bytes;

struct Component;

const BUCKET: &str = "default";

/// Default page size for `list` / `query` when `limit == 0`.
const DEFAULT_LIMIT: usize = 50;
/// Hard cap on a single `list` page.
const MAX_LIMIT: usize = 500;
/// Cap on the sanitized value embedded in a secondary-index key. Longer values
/// are truncated, which can only cause distinct values to share an index key
/// (over-matching), which the readers then re-filter away.
const MAX_INDEXED_VALUE: usize = 120;

// ---- stored shape -------------------------------------------------------

/// What we persist per record at `rec_{collection}_{id}`. `data` is the JSON
/// object body verbatim (so re-serialization can't reorder/normalize it).
#[derive(Serialize, Deserialize)]
struct Stored {
    data: String,
    revision: u64,
    created: u64,
    updated: u64,
    index_fields: Vec<String>,
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn now_ms() -> u128 {
    let t = wall_clock::now();
    (t.seconds as u128) * 1000 + (t.nanoseconds as u128) / 1_000_000
}

// ---- key naming ---------------------------------------------------------

/// Sanitize one opaque segment to NATS-legal kv chars (same byte scheme as
/// config-store's `sanitize` / idempotency-guard's `id_key`).
fn sanitize(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for b in seg.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

/// Sanitize an indexed value, capping its length so arbitrarily long values
/// still fit in a key. Truncation can only over-match (readers re-verify).
fn sanitize_value(v: &str) -> String {
    let mut s = sanitize(v);
    if s.len() > MAX_INDEXED_VALUE {
        s.truncate(MAX_INDEXED_VALUE);
    }
    s
}

/// Storage key for a record: `rec_{collection}_{id}`.
fn rec_key(collection: &str, id: &str) -> String {
    format!("rec_{}_{}", sanitize(collection), sanitize(id))
}

/// Storage key for a collection's sorted id index: `idx_{collection}`.
fn idx_key(collection: &str) -> String {
    format!("idx_{}", sanitize(collection))
}

/// Storage key for a secondary index: `ix_{collection}_{field}_{sanitize(value)}`.
fn ix_key(collection: &str, field: &str, value: &str) -> String {
    format!(
        "ix_{}_{}_{}",
        sanitize(collection),
        sanitize(field),
        sanitize_value(value)
    )
}

// ---- ULID minting -------------------------------------------------------
//
// 128 bits = [48-bit ms timestamp big-endian | 80-bit random], rendered as 26
// Crockford-base32 chars. The top char encodes the high 2 bits; the remaining
// 25 chars encode 5 bits each (2 + 25*5 = 127, the spec pads the top bit to 0,
// which is why a ULID's first char is never above '7'). Monotonic-within-ms is
// intentionally skipped: every id draws fresh random, so ids minted in the same
// millisecond still sort by their ms prefix (their intra-ms order is arbitrary,
// which is acceptable for the id-index).

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn mint_ulid() -> String {
    let ms = now_ms() & 0xFFFF_FFFF_FFFF; // low 48 bits
    let rand = get_random_bytes(10);

    // Assemble the full 128-bit value as a u128: 48-bit time then 80-bit random.
    let mut value: u128 = ms;
    for &b in rand.iter() {
        value = (value << 8) | (b as u128);
    }

    // Encode 26 Crockford chars, most-significant first.
    let mut buf = [0u8; 26];
    for i in (0..26).rev() {
        let idx = (value & 0x1F) as usize;
        buf[i] = CROCKFORD[idx];
        value >>= 5;
    }
    String::from_utf8(buf.to_vec()).expect("crockford alphabet is ascii")
}

// ---- kv plumbing --------------------------------------------------------

fn open() -> Result<kv::Bucket, StoreError> {
    kv::open(BUCKET).map_err(|e| StoreError::BackendUnavailable(format!("open: {e:?}")))
}

/// Load + deserialize the record at `id`, `None` if absent. A corrupt stored
/// record surfaces as `backend-unavailable` (it is our own bug, not bad input).
fn load_record(
    bucket: &kv::Bucket,
    collection: &str,
    id: &str,
) -> Result<Option<Stored>, StoreError> {
    match bucket.get(&rec_key(collection, id)) {
        Ok(Some(bytes)) => {
            let s = serde_json::from_slice::<Stored>(&bytes).map_err(|e| {
                StoreError::BackendUnavailable(format!("corrupt record {id}: {e}"))
            })?;
            Ok(Some(s))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(StoreError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

fn put_record(
    bucket: &kv::Bucket,
    collection: &str,
    id: &str,
    rec: &Stored,
) -> Result<(), StoreError> {
    let body = serde_json::to_vec(rec)
        .map_err(|e| StoreError::BackendUnavailable(format!("serialize record: {e}")))?;
    bucket
        .set(&rec_key(collection, id), &body)
        .map_err(|e| StoreError::BackendUnavailable(format!("set: {e:?}")))
}

// ---- sorted id index ----------------------------------------------------
//
// `idx_{collection}` -> JSON Vec<String> of ids, kept SORTED. Since ids are
// ULIDs, lexicographic order == creation-time order, so `list` reads it
// directly. Same single-writer best-effort RMW caveat as config-store's
// namespace index: no CAS in wasi:keyvalue@0.2.0-draft.

fn read_id_index(bucket: &kv::Bucket, collection: &str) -> Result<Vec<String>, StoreError> {
    match bucket.get(&idx_key(collection)) {
        Ok(Some(bytes)) => serde_json::from_slice::<Vec<String>>(&bytes)
            .map_err(|e| StoreError::BackendUnavailable(format!("corrupt id index: {e}"))),
        Ok(None) => Ok(Vec::new()),
        Err(e) => Err(StoreError::BackendUnavailable(format!("get id index: {e:?}"))),
    }
}

fn write_id_index(bucket: &kv::Bucket, collection: &str, ids: &[String]) -> Result<(), StoreError> {
    let body = serde_json::to_vec(ids)
        .map_err(|e| StoreError::BackendUnavailable(format!("serialize id index: {e}")))?;
    bucket
        .set(&idx_key(collection), &body)
        .map_err(|e| StoreError::BackendUnavailable(format!("set id index: {e:?}")))
}

fn id_index_insert(bucket: &kv::Bucket, collection: &str, id: &str) -> Result<(), StoreError> {
    let mut ids = read_id_index(bucket, collection)?;
    // keep sorted; ULIDs sort lexicographically by time.
    match ids.binary_search_by(|x| x.as_str().cmp(id)) {
        Ok(_) => {} // already present
        Err(pos) => {
            ids.insert(pos, id.to_string());
            write_id_index(bucket, collection, &ids)?;
        }
    }
    Ok(())
}

fn id_index_remove(bucket: &kv::Bucket, collection: &str, id: &str) -> Result<(), StoreError> {
    let mut ids = read_id_index(bucket, collection)?;
    if let Ok(pos) = ids.binary_search_by(|x| x.as_str().cmp(id)) {
        ids.remove(pos);
        write_id_index(bucket, collection, &ids)?;
    }
    Ok(())
}

// ---- secondary indexes --------------------------------------------------

fn read_ix(bucket: &kv::Bucket, key: &str) -> Result<Vec<String>, StoreError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => serde_json::from_slice::<Vec<String>>(&bytes)
            .map_err(|e| StoreError::BackendUnavailable(format!("corrupt index {key}: {e}"))),
        Ok(None) => Ok(Vec::new()),
        Err(e) => Err(StoreError::BackendUnavailable(format!("get index: {e:?}"))),
    }
}

fn write_ix(bucket: &kv::Bucket, key: &str, ids: &[String]) -> Result<(), StoreError> {
    let body = serde_json::to_vec(ids)
        .map_err(|e| StoreError::BackendUnavailable(format!("serialize index: {e}")))?;
    bucket
        .set(key, &body)
        .map_err(|e| StoreError::BackendUnavailable(format!("set index: {e:?}")))
}

fn ix_add(bucket: &kv::Bucket, key: &str, id: &str) -> Result<(), StoreError> {
    let mut ids = read_ix(bucket, key)?;
    if !ids.iter().any(|x| x == id) {
        ids.push(id.to_string());
        write_ix(bucket, key, &ids)?;
    }
    Ok(())
}

fn ix_remove(bucket: &kv::Bucket, key: &str, id: &str) -> Result<(), StoreError> {
    let mut ids = read_ix(bucket, key)?;
    let before = ids.len();
    ids.retain(|x| x != id);
    if ids.len() != before {
        write_ix(bucket, key, &ids)?;
    }
    Ok(())
}

/// The JSON-encoded value of a top-level field in `data`, or `None` if the
/// field is absent. Encodes compactly so a string field `acme` -> `"acme"`,
/// matching the `value` callers pass to `find-by` / `filter`.
fn field_value(parsed: &Value, field: &str) -> Option<String> {
    parsed
        .as_object()
        .and_then(|obj| obj.get(field))
        .map(|v| v.to_string())
}

/// Add `id` to every secondary index implied by `data` + `index_fields`.
fn add_secondary_indexes(
    bucket: &kv::Bucket,
    collection: &str,
    id: &str,
    parsed: &Value,
    index_fields: &[String],
) -> Result<(), StoreError> {
    for field in index_fields {
        if let Some(v) = field_value(parsed, field) {
            ix_add(bucket, &ix_key(collection, field, &v), id)?;
        }
    }
    Ok(())
}

/// Remove `id` from every secondary index implied by `data` + `index_fields`.
fn remove_secondary_indexes(
    bucket: &kv::Bucket,
    collection: &str,
    id: &str,
    parsed: &Value,
    index_fields: &[String],
) -> Result<(), StoreError> {
    for field in index_fields {
        if let Some(v) = field_value(parsed, field) {
            ix_remove(bucket, &ix_key(collection, field, &v), id)?;
        }
    }
    Ok(())
}

// ---- helpers ------------------------------------------------------------

fn entry_from(id: &str, s: Stored) -> Entry {
    Entry {
        id: id.to_string(),
        data: s.data,
        revision: s.revision,
        created: s.created,
        updated: s.updated,
    }
}

/// Parse caller `data`, requiring a JSON object. Bad input -> `invalid-json`.
fn parse_object(data: &str) -> Result<Value, StoreError> {
    let v = serde_json::from_str::<Value>(data)
        .map_err(|e| StoreError::InvalidJson(format!("not valid JSON: {e}")))?;
    if !v.is_object() {
        return Err(StoreError::InvalidJson("data must be a JSON object".into()));
    }
    Ok(v)
}

// ---- guest --------------------------------------------------------------

impl Guest for Component {
    fn create(
        collection: String,
        data: String,
        index_fields: Vec<String>,
    ) -> Result<Entry, StoreError> {
        let parsed = parse_object(&data)?;
        let bucket = open()?;
        let id = mint_ulid();
        let ts = now();

        let stored = Stored {
            data,
            revision: 1,
            created: ts,
            updated: ts,
            index_fields,
        };
        put_record(&bucket, &collection, &id, &stored)?;
        id_index_insert(&bucket, &collection, &id)?;
        add_secondary_indexes(&bucket, &collection, &id, &parsed, &stored.index_fields)?;

        Ok(entry_from(&id, stored))
    }

    fn get(collection: String, id: String) -> Result<Entry, StoreError> {
        let bucket = open()?;
        let stored = load_record(&bucket, &collection, &id)?.ok_or(StoreError::NotFound)?;
        Ok(entry_from(&id, stored))
    }

    fn update(
        collection: String,
        id: String,
        data: String,
        expected_revision: u64,
    ) -> Result<Entry, StoreError> {
        let bucket = open()?;
        let current = load_record(&bucket, &collection, &id)?.ok_or(StoreError::NotFound)?;

        if expected_revision != 0 && expected_revision != current.revision {
            return Err(StoreError::RevisionConflict(current.revision));
        }

        let parsed_new = parse_object(&data)?;

        // Re-index: drop the old field values, add the new ones. Same set of
        // index fields as the existing record.
        let old_parsed = serde_json::from_str::<Value>(&current.data).map_err(|e| {
            StoreError::BackendUnavailable(format!("corrupt record {id} data: {e}"))
        })?;
        remove_secondary_indexes(&bucket, &collection, &id, &old_parsed, &current.index_fields)?;

        let stored = Stored {
            data,
            revision: current.revision + 1,
            created: current.created,
            updated: now(),
            index_fields: current.index_fields,
        };
        put_record(&bucket, &collection, &id, &stored)?;
        add_secondary_indexes(&bucket, &collection, &id, &parsed_new, &stored.index_fields)?;

        Ok(entry_from(&id, stored))
    }

    fn delete(collection: String, id: String) -> Result<(), StoreError> {
        let bucket = open()?;
        // Idempotent: absent -> Ok.
        let Some(stored) = load_record(&bucket, &collection, &id)? else {
            return Ok(());
        };

        id_index_remove(&bucket, &collection, &id)?;
        if let Ok(parsed) = serde_json::from_str::<Value>(&stored.data) {
            remove_secondary_indexes(&bucket, &collection, &id, &parsed, &stored.index_fields)?;
        }
        bucket
            .delete(&rec_key(&collection, &id))
            .map_err(|e| StoreError::BackendUnavailable(format!("delete: {e:?}")))?;
        Ok(())
    }

    fn list_records(collection: String, limit: u32, after: String) -> Result<Page, StoreError> {
        let bucket = open()?;
        let ids = read_id_index(&bucket, &collection)?;

        let limit = match limit as usize {
            0 => DEFAULT_LIMIT,
            n => n.min(MAX_LIMIT),
        };

        // Find the start position. `after` empty -> from the start; else the
        // position just past `after` in the sorted list.
        let start = if after.is_empty() {
            0
        } else {
            match ids.binary_search_by(|x| x.as_str().cmp(after.as_str())) {
                Ok(pos) => pos + 1,
                Err(pos) => pos, // `after` not present: resume where it would be.
            }
        };

        let window: Vec<&String> = ids.iter().skip(start).take(limit).collect();
        let mut entries = Vec::with_capacity(window.len());
        for id in &window {
            if let Some(stored) = load_record(&bucket, &collection, id)? {
                entries.push(entry_from(id, stored));
            }
            // Skip ids whose record vanished (best-effort index drift).
        }

        // More remain iff there is an id past the end of this window.
        let consumed = start + window.len();
        let next = if consumed < ids.len() {
            window
                .last()
                .map(|s| s.to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };

        Ok(Page { entries, next })
    }

    fn find_by(
        collection: String,
        field: String,
        value: String,
    ) -> Result<Vec<Entry>, StoreError> {
        let bucket = open()?;
        // Missing index key -> empty list, not an error.
        let ids = read_ix(&bucket, &ix_key(&collection, &field, &value))?;

        let mut entries = Vec::new();
        for id in &ids {
            if let Some(stored) = load_record(&bucket, &collection, id)? {
                // RE-VERIFY: the sanitized+capped index key can over-match, so
                // confirm the record's actual top-level field == value.
                if let Ok(parsed) = serde_json::from_str::<Value>(&stored.data) {
                    if field_value(&parsed, &field).as_deref() == Some(value.as_str()) {
                        entries.push(entry_from(id, stored));
                    }
                }
            }
        }
        Ok(entries)
    }

    fn query(
        collection: String,
        filters: Vec<Filter>,
        limit: u32,
    ) -> Result<Vec<Entry>, StoreError> {
        let bucket = open()?;

        let limit = match limit as usize {
            0 => DEFAULT_LIMIT,
            n => n,
        };

        // Candidate ids: if there are filters, use the FIRST filter's secondary
        // index (cheap, may over-match) as the candidate set; otherwise the full
        // sorted id index. Either way every record is re-checked against ALL
        // filters below, so a non-indexed first filter still yields correct
        // results (it just won't have narrowed the candidates).
        let candidates = match filters.first() {
            Some(f) => {
                let ix = read_ix(&bucket, &ix_key(&collection, &f.field, &f.value))?;
                // If the first filter's field isn't indexed there's no index key,
                // so `ix` is empty — but the field may still match records. Fall
                // back to a full scan (the per-record re-check below filters it).
                // Only an indexed field with a genuine zero matches stays empty,
                // which `find-by` semantics would also give. To distinguish, scan
                // when the index is absent: treat empty index as "scan".
                if ix.is_empty() {
                    read_id_index(&bucket, &collection)?
                } else {
                    ix
                }
            }
            None => read_id_index(&bucket, &collection)?,
        };

        let mut entries = Vec::new();
        for id in &candidates {
            if entries.len() >= limit {
                break;
            }
            let Some(stored) = load_record(&bucket, &collection, id)? else {
                continue;
            };
            let Ok(parsed) = serde_json::from_str::<Value>(&stored.data) else {
                continue;
            };
            // AND: every filter's top-level field must JSON-equal its value.
            let matches = filters.iter().all(|f| {
                field_value(&parsed, &f.field).as_deref() == Some(f.value.as_str())
            });
            if matches {
                entries.push(entry_from(id, stored));
            }
        }
        Ok(entries)
    }

    fn count(collection: String) -> Result<u64, StoreError> {
        let bucket = open()?;
        Ok(read_id_index(&bucket, &collection)?.len() as u64)
    }
}

bindings::export!(Component with_types_in bindings);
