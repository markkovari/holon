//! `record-store` — store JSON records in named collections, query them by field, and index them
//!
//! Typed JSON records in named collections; the app owns the schema.
//! Ids are ULIDs, so the per-collection id index stays sorted in creation order (`list` pages
//! over it, `count` is its length).
//! Secondary indexes `ix_{collection}_{F}_{sanitize(V)}` hold matching ids. The value is
//! sanitized and capped, so keys can collide and only ever over-match; `find-by` re-verifies.
//! `update` with a non-zero `expected-revision` yields `revision-conflict(current)` on mismatch, comparing and writing through `comp:store/cas` (ADR-0065).
//! Index upkeep (`idlist`) is CAS-guarded but not atomic with the record write, so records stay
//! authoritative: readers re-verify, `list` reports dangling ids, `repair` rebuilds.

#[allow(warnings)]
mod bindings;
mod idlist;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use bindings::comp::store::cas;
use bindings::exports::records::store::store::{
    Entry, Filter, Guest, Page, RepairReport, StoreError,
};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::batch;
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
    format!("ix_{}_{}_{}", sanitize(collection), sanitize(field), sanitize_value(value))
}

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

fn open() -> Result<kv::Bucket, StoreError> {
    kv::open(BUCKET).map_err(|e| StoreError::BackendUnavailable(format!("open: {e:?}")))
}

/// Say the index disagreed with the records, in the shape `audit-log` uses.
/// Instances are per-request (ADR-0037), so there is no counter: one line per occurrence, nothing
/// when quiet.
fn drift(collection: &str, op: &str, missing: usize) {
    if missing == 0 {
        return;
    }
    eprintln!(
        "{{\"drift\":true,\"collection\":\"{}\",\"op\":\"{}\",\"unresolved\":{},\
         \"fix\":\"records:store repair\"}}",
        sanitize(collection),
        op,
        missing
    );
}

/// How many times a guarded RECORD write (`create`'s id placement, `update`)
/// re-reads and retries before giving up. The same bound `gate-domain` uses for
/// its own CAS loop. The index lists have their own, larger bound
/// (`idlist::CAS_TRIES`): every create in a collection contends on one id-index
/// chunk, where a single record rarely sees that many writers at once.
const CAS_TRIES: u32 = 40;

/// Load + deserialize the record at `id`, `None` if absent. A corrupt stored
/// record surfaces as `backend-unavailable` (it is our own bug, not bad input).
fn load_record(
    bucket: &kv::Bucket,
    collection: &str,
    id: &str,
) -> Result<Option<Stored>, StoreError> {
    match bucket.get(&rec_key(collection, id)) {
        Ok(Some(bytes)) => {
            let s = serde_json::from_slice::<Stored>(&bytes)
                .map_err(|e| StoreError::BackendUnavailable(format!("corrupt record {id}: {e}")))?;
            Ok(Some(s))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(StoreError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

/// Load many records in ONE backend round-trip via `wasi:keyvalue/batch` get-many. Returns (id,
/// record) pairs in input-id order, skipping absent ids.
/// A get-many error propagates: a host without the batch interface fails at LINK time, so a
/// runtime error is a real backend fault, and degrading to sequential gets turned one transient
/// error into a 20-second page.
fn load_records_many(
    bucket: &kv::Bucket,
    collection: &str,
    ids: &[String],
) -> Result<Vec<(String, Stored)>, StoreError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let keys: Vec<String> = ids.iter().map(|id| rec_key(collection, id)).collect();
    let found = batch::get_many(bucket, &keys)
        .map_err(|e| StoreError::BackendUnavailable(format!("get-many: {e:?}")))?;
    let mut by_key: std::collections::HashMap<String, Vec<u8>> =
        found.into_iter().flatten().collect();
    let mut out = Vec::with_capacity(ids.len());
    for (id, key) in ids.iter().zip(&keys) {
        if let Some(bytes) = by_key.remove(key) {
            let stored = serde_json::from_slice::<Stored>(&bytes)
                .map_err(|e| StoreError::BackendUnavailable(format!("corrupt record {id}: {e}")))?;
            out.push((id.clone(), stored));
        }
    }
    Ok(out)
}

//
// The chunked sorted lists live in `idlist`, written against a two-call `Kv`
// trait so their interleavings can be tested without a host. This is the
// component's implementation of that trait: the guarded half goes through
// `comp:store/cas` (never cached), the reader half through plain
// `wasi:keyvalue` (which a host may cache, ADR-0064).

struct BucketKv<'a>(&'a kv::Bucket);

impl idlist::Kv for BucketKv<'_> {
    fn get(&self, key: &str) -> Result<Option<(u64, Vec<u8>)>, String> {
        cas::get(self.0, key)
            .map(|o| o.map(|v| (v.revision, v.value)))
            .map_err(|e| format!("cas get {key}: {e:?}"))
    }
    fn cas(&self, key: &str, value: &[u8], expected: u64) -> Result<Result<u64, u64>, String> {
        match cas::set(self.0, key, value, expected) {
            Ok(cas::Outcome::Committed(r)) => Ok(Ok(r)),
            Ok(cas::Outcome::Conflict(r)) => Ok(Err(r)),
            Err(e) => Err(format!("cas set {key}: {e:?}")),
        }
    }
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        self.0.get(key).map_err(|e| format!("get {key}: {e:?}"))
    }
    fn read_many(&self, keys: &[String]) -> Result<Vec<Option<Vec<u8>>>, String> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let found = batch::get_many(self.0, keys).map_err(|e| format!("get-many: {e:?}"))?;
        let mut by_key: std::collections::HashMap<String, Vec<u8>> =
            found.into_iter().flatten().collect();
        Ok(keys.iter().map(|k| by_key.remove(k)).collect())
    }
}

fn enc<T: Serialize>(v: &T) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(v).map_err(|e| StoreError::BackendUnavailable(format!("encode: {e}")))
}

fn be(e: String) -> StoreError {
    StoreError::BackendUnavailable(e)
}

fn read_id_index(bucket: &kv::Bucket, collection: &str) -> Result<Vec<String>, StoreError> {
    idlist::read_all(&BucketKv(bucket), &idx_key(collection)).map_err(be)
}

fn id_index_insert(bucket: &kv::Bucket, collection: &str, id: &str) -> Result<(), StoreError> {
    idlist::insert(&BucketKv(bucket), &idx_key(collection), id).map_err(be)
}

fn id_index_remove(bucket: &kv::Bucket, collection: &str, id: &str) -> Result<(), StoreError> {
    idlist::remove(&BucketKv(bucket), &idx_key(collection), id).map_err(be)
}

fn read_ix(bucket: &kv::Bucket, key: &str) -> Result<Vec<String>, StoreError> {
    idlist::read_all(&BucketKv(bucket), key).map_err(be)
}

fn ix_add(bucket: &kv::Bucket, key: &str, id: &str) -> Result<(), StoreError> {
    idlist::insert(&BucketKv(bucket), key, id).map_err(be)
}

fn ix_remove(bucket: &kv::Bucket, key: &str, id: &str) -> Result<(), StoreError> {
    idlist::remove(&BucketKv(bucket), key, id).map_err(be)
}

/// The JSON-encoded value of a top-level field in `data`, or `None` if the
/// field is absent. Encodes compactly so a string field `acme` -> `"acme"`,
/// matching the `value` callers pass to `find-by` / `filter`.
fn field_value(parsed: &Value, field: &str) -> Option<String> {
    parsed.as_object().and_then(|obj| obj.get(field)).map(|v| v.to_string())
}

/// The secondary-index keys `data` + `index_fields` imply, deduped.
fn secondary_keys(collection: &str, parsed: &Value, index_fields: &[String]) -> Vec<String> {
    let mut keys: Vec<String> = index_fields
        .iter()
        .filter_map(|field| field_value(parsed, field).map(|v| ix_key(collection, field, &v)))
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// How many times index maintenance that runs AFTER a record write has committed
/// is attempted before it is left to `repair`. Each attempt is itself a CAS loop
/// of up to `CAS_TRIES`, so this only matters for a backend error or a key under
/// pathological contention.
const INDEX_TRIES: u32 = 3;

/// Apply one index edit after the record has committed. This must NOT fail the call: the record
/// is authoritative, and reporting `err` misleads a caller about a write that landed
/// (`treasury:ledger` refunded transfers whose debit had landed).
/// A missed index edit is recoverable (readers re-verify, `repair` rebuilds), so retry, then say
/// so in the `drift` shape and leave it for `repair`.
fn index_after_commit(
    collection: &str,
    op: &str,
    key: &str,
    mut edit: impl FnMut() -> Result<(), StoreError>,
) {
    let mut last = None;
    for _ in 0..INDEX_TRIES {
        match edit() {
            Ok(()) => return,
            Err(e) => last = Some(e),
        }
    }
    let error = last.map(|e| format!("{e:?}")).unwrap_or_default();
    eprintln!(
        "{{\"drift\":true,\"collection\":\"{}\",\"op\":\"{}\",\"index\":{},\"error\":{},\
         \"fix\":\"records:store repair\"}}",
        sanitize(collection),
        op,
        Value::from(key),
        Value::from(error),
    );
}

/// Move `id` between the secondary indexes `old` implied and those `new` implies, touching ONLY
/// the keys whose value changed. Best-effort — see `index_after_commit`.
/// Rewriting unchanged keys made concurrent updates contend on one key and hid the record from
/// `find-by` between the remove and the add.
fn reindex_after_commit(
    bucket: &kv::Bucket,
    collection: &str,
    id: &str,
    op: &str,
    old: &[String],
    new: &[String],
) {
    for key in old.iter().filter(|k| !new.contains(k)) {
        index_after_commit(collection, op, key, || ix_remove(bucket, key, id));
    }
    for key in new.iter().filter(|k| !old.contains(k)) {
        index_after_commit(collection, op, key, || ix_add(bucket, key, id));
    }
}

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

impl Guest for Component {
    fn create(
        collection: String,
        data: String,
        index_fields: Vec<String>,
    ) -> Result<Entry, StoreError> {
        let parsed = parse_object(&data)?;
        let bucket = open()?;
        let ts = now();
        let stored = Stored { data, revision: 1, created: ts, updated: ts, index_fields };

        // CAS'd with `expected: 0` (must not exist yet), so an id collision retries with a fresh
        // id instead of silently overwriting the first record.
        let mut id = mint_ulid();
        let mut placed = false;
        for _ in 0..CAS_TRIES {
            match cas::set(&bucket, &rec_key(&collection, &id), &enc(&stored)?, 0) {
                Ok(cas::Outcome::Committed(_)) => {
                    placed = true;
                    break;
                }
                Ok(cas::Outcome::Conflict(_)) => id = mint_ulid(),
                Err(e) => {
                    return Err(StoreError::BackendUnavailable(format!("cas set record: {e:?}")))
                }
            }
        }
        if !placed {
            return Err(StoreError::BackendUnavailable(format!(
                "create {collection}: {CAS_TRIES} freshly minted ids all collided"
            )));
        }

        // The record is committed; from here on nothing may turn this into an
        // `err` (see `index_after_commit`). A caller told its create failed
        // creates again, and the first one is a live, unlisted duplicate.
        let ix = idx_key(&collection);
        index_after_commit(&collection, "create", &ix, || {
            id_index_insert(&bucket, &collection, &id)
        });
        let keys = secondary_keys(&collection, &parsed, &stored.index_fields);
        reindex_after_commit(&bucket, &collection, &id, "create", &[], &keys);

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
        let parsed_new = parse_object(&data)?;
        let key = rec_key(&collection, &id);

        // `cas::get` reports the revision the store is actually at and is never cached; `cas::set` only lands if the key is unchanged. A writer that lost the race re-reads and retries (comparing in the component raced with other nodes and the host cache, ADR-0065).
        for _ in 0..CAS_TRIES {
            let (store_revision, bytes) = match cas::get(&bucket, &key) {
                Ok(Some(v)) => (v.revision, v.value),
                Ok(None) => return Err(StoreError::NotFound),
                Err(e) => return Err(StoreError::BackendUnavailable(format!("cas get: {e:?}"))),
            };
            let current: Stored = serde_json::from_slice(&bytes)
                .map_err(|e| StoreError::BackendUnavailable(format!("corrupt record {id}: {e}")))?;

            // The CALLER's expectation is about the record's own revision, not the backend's
            // sequence. Both have to hold.
            if expected_revision != 0 && expected_revision != current.revision {
                return Err(StoreError::RevisionConflict(current.revision));
            }

            let stored = Stored {
                data: data.clone(),
                revision: current.revision + 1,
                created: current.created,
                updated: now(),
                index_fields: current.index_fields.clone(),
            };
            let body = serde_json::to_vec(&stored)
                .map_err(|e| StoreError::BackendUnavailable(format!("serialize record: {e}")))?;

            match cas::set(&bucket, &key, &body, store_revision) {
                Ok(cas::Outcome::Committed(_)) => {
                    // Indexes follow the record as separate writes; a crash between them leaves a
                    // stale index entry.
                    // The write has landed, so nothing below may report `err` (see
                    // `index_after_commit`).
                    let old_keys = serde_json::from_str::<Value>(&current.data)
                        .map(|v| secondary_keys(&collection, &v, &current.index_fields))
                        .unwrap_or_default();
                    let new_keys = secondary_keys(&collection, &parsed_new, &stored.index_fields);
                    reindex_after_commit(&bucket, &collection, &id, "update", &old_keys, &new_keys);
                    return Ok(entry_from(&id, stored));
                }
                // Someone else wrote between the read and the write. Re-read and retry.
                Ok(cas::Outcome::Conflict(_)) => continue,
                Err(e) => return Err(StoreError::BackendUnavailable(format!("cas set: {e:?}"))),
            }
        }
        Err(StoreError::BackendUnavailable(format!(
            "update {collection}/{id}: {CAS_TRIES} attempts all lost the race"
        )))
    }

    fn delete(collection: String, id: String) -> Result<(), StoreError> {
        let bucket = open()?;
        // Idempotent: absent -> Ok.
        let Some(stored) = load_record(&bucket, &collection, &id)? else {
            return Ok(());
        };

        // The record goes FIRST and is the only step that can fail the call; an interrupted
        // delete then leaves only dangling ids, which reads skip and `repair` prunes.
        bucket
            .delete(&rec_key(&collection, &id))
            .map_err(|e| StoreError::BackendUnavailable(format!("delete: {e:?}")))?;
        let ix = idx_key(&collection);
        index_after_commit(&collection, "delete", &ix, || {
            id_index_remove(&bucket, &collection, &id)
        });
        let keys = serde_json::from_str::<Value>(&stored.data)
            .map(|v| secondary_keys(&collection, &v, &stored.index_fields))
            .unwrap_or_default();
        reindex_after_commit(&bucket, &collection, &id, "delete", &keys, &[]);
        Ok(())
    }

    fn list_records(collection: String, limit: u32, after: String) -> Result<Page, StoreError> {
        let bucket = open()?;
        let limit = match limit as usize {
            0 => DEFAULT_LIMIT,
            n => n.min(MAX_LIMIT),
        };

        // Page over the chunked id index (fetches only the chunks the page
        // touches), then ONE batched record fetch; ids whose record vanished
        // (best-effort index drift) are skipped by load_records_many.
        let (window, more) =
            idlist::page(&BucketKv(&bucket), &idx_key(&collection), &after, limit).map_err(be)?;
        let entries: Vec<Entry> = load_records_many(&bucket, &collection, &window)?
            .into_iter()
            .map(|(id, stored)| entry_from(&id, stored))
            .collect();
        // The index named ids this page could not resolve (ADR-0075). This is the only drift a
        // read can see; a record the index never mentions needs `verify`.
        drift(&collection, "list", window.len().saturating_sub(entries.len()));

        let next = if more {
            window.last().map(|s| s.to_string()).unwrap_or_default()
        } else {
            String::new()
        };

        Ok(Page { entries, next })
    }

    fn find_by(collection: String, field: String, value: String) -> Result<Vec<Entry>, StoreError> {
        let bucket = open()?;
        let ids = read_ix(&bucket, &ix_key(&collection, &field, &value))?;

        let mut entries = Vec::new();
        for (id, stored) in load_records_many(&bucket, &collection, &ids)? {
            // RE-VERIFY: the sanitized+capped index key can over-match, so
            // confirm the record's actual top-level field == value.
            if let Ok(parsed) = serde_json::from_str::<Value>(&stored.data) {
                if field_value(&parsed, &field).as_deref() == Some(value.as_str()) {
                    entries.push(entry_from(&id, stored));
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
                // A non-indexed first filter has no index key, so `ix` is empty yet records may
                // match: scan when the index is absent.
                if ix.is_empty() {
                    read_id_index(&bucket, &collection)?
                } else {
                    ix
                }
            }
            None => read_id_index(&bucket, &collection)?,
        };

        // Batch-fetch candidates in chunks so a scan over a big collection
        // still early-exits once `limit` matches are found.
        let mut entries = Vec::new();
        for chunk in candidates.chunks(100) {
            if entries.len() >= limit {
                break;
            }
            for (id, stored) in load_records_many(&bucket, &collection, chunk)? {
                if entries.len() >= limit {
                    break;
                }
                let Ok(parsed) = serde_json::from_str::<Value>(&stored.data) else {
                    continue;
                };
                let matches = filters
                    .iter()
                    .all(|f| field_value(&parsed, &f.field).as_deref() == Some(f.value.as_str()));
                if matches {
                    entries.push(entry_from(&id, stored));
                }
            }
        }
        Ok(entries)
    }

    fn count(collection: String) -> Result<u64, StoreError> {
        let bucket = open()?;
        // manifest chunk counts sum — one kv read regardless of size.
        idlist::count(&BucketKv(&bucket), &idx_key(&collection)).map_err(be)
    }

    /// Rebuild the id index from the records (ADR-0068). Records are authoritative. It scans the
    /// whole bucket, so it is an operator action, not something `list` does.
    fn repair(collection: String) -> Result<RepairReport, StoreError> {
        repair_inner(&collection, true)
    }

    /// Report the same disagreement without touching anything (ADR-0075).
    fn verify(collection: String) -> Result<RepairReport, StoreError> {
        repair_inner(&collection, false)
    }
}

/// The scan behind both `repair` and `verify`. `write` is the only difference:
/// one of them fixes what it finds and the other only says so.
fn repair_inner(collection: &str, write: bool) -> Result<RepairReport, StoreError> {
    {
        let collection = collection.to_string();
        let bucket = open()?;
        let prefix = format!("rec_{}_", sanitize(&collection));

        let keys = bucket
            .list_keys(None)
            .map_err(|e| StoreError::BackendUnavailable(format!("list-keys: {e:?}")))?;
        let mut real: Vec<String> = keys
            .keys
            .iter()
            .filter_map(|k| k.strip_prefix(&prefix))
            .map(|id| id.to_string())
            .collect();
        // The index is sorted, and `sanitize` is identity for a ULID, so the
        // stored suffix IS the id. Sorting here makes the comparison below a
        // set difference rather than a quadratic scan.
        real.sort();
        real.dedup();

        let indexed = read_id_index(&bucket, &collection)?;
        let indexed_set: std::collections::BTreeSet<&String> = indexed.iter().collect();
        let real_set: std::collections::BTreeSet<&String> = real.iter().collect();

        let missing: Vec<&String> = real_set.difference(&indexed_set).copied().collect();
        let dangling: Vec<&String> = indexed_set.difference(&real_set).copied().collect();
        let (readded, pruned) = (missing.len() as u64, dangling.len() as u64);

        // Refuse to act on a scan that found nothing while the index is populated: a broken scan
        // (`list_keys` once returned corrupted names on NATS) would prune a good index to zero.
        if real.is_empty() && !indexed.is_empty() {
            return Err(StoreError::BackendUnavailable(format!(
                "repair {collection}: the scan found no records while the index names {}. \
                 Refusing to rewrite it — this is a broken scan, not an empty collection.",
                indexed.len()
            )));
        }

        // Patched id by id through the guarded insert/remove, not rewritten wholesale (a
        // wholesale write erases ids inserted after the scan). `heal` then re-derives every
        // manifest entry.
        let lists = BucketKv(&bucket);
        if write {
            let ix = idx_key(&collection);
            for id in &missing {
                idlist::insert(&lists, &ix, id).map_err(be)?;
            }
            for id in &dangling {
                // Re-checked: a record created after the scan is in the index and
                // not in `real`, and must not be pruned for being quick.
                if load_record(&bucket, &collection, id)?.is_none() {
                    idlist::remove(&lists, &ix, id).map_err(be)?;
                }
            }
            idlist::heal(&lists, &ix).map_err(be)?;
        }

        // Then the secondary indexes (ADR-0068 left them out): recompute what each should hold
        // from the records and apply the difference with the guarded writers.
        let mut wanted: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for (id, stored) in load_records_many(&bucket, &collection, &real)? {
            let Ok(parsed) = serde_json::from_str::<Value>(&stored.data) else { continue };
            for field in &stored.index_fields {
                if let Some(v) = field_value(&parsed, field) {
                    wanted.entry(ix_key(&collection, field, &v)).or_default().push(id.clone());
                }
            }
        }
        for ids in wanted.values_mut() {
            ids.sort();
            ids.dedup();
        }
        // Is `id` still, right now, a record whose fields put it under index `key`?
        // Asked before removing anything, for the same reason as above.
        let still_under = |key: &str, id: &str| -> Result<bool, StoreError> {
            Ok(load_record(&bucket, &collection, id)?.is_some_and(|s| {
                serde_json::from_str::<Value>(&s.data)
                    .map(|v| {
                        secondary_keys(&collection, &v, &s.index_fields).iter().any(|k| k == key)
                    })
                    .unwrap_or(false)
            }))
        };
        let fix_index = |key: &str, want: &[String]| -> Result<bool, StoreError> {
            let have = idlist::read_all(&lists, key).map_err(be)?;
            let want_set: std::collections::BTreeSet<&String> = want.iter().collect();
            let have_set: std::collections::BTreeSet<&String> = have.iter().collect();
            if !write {
                return Ok(have_set != want_set);
            }
            for id in want_set.difference(&have_set) {
                idlist::insert(&lists, key, id).map_err(be)?;
            }
            for id in have_set.difference(&want_set) {
                if !still_under(key, id)? {
                    idlist::remove(&lists, key, id).map_err(be)?;
                }
            }
            idlist::heal(&lists, key).map_err(be)?;
            Ok(have_set != want_set)
        };
        for (key, ids) in &wanted {
            fix_index(key, ids)?;
        }

        // An index key nothing points at any more (interrupted delete, changed value)
        // over-matches until re-verified. Counted only while it still names something, so a
        // second run reports zero.
        let ix_prefix = format!("ix_{}_", sanitize(&collection));
        let mut dropped = 0u64;
        for k in keys.keys.iter() {
            // Chunk keys hang off their base and are handled through it.
            if !k.starts_with(&ix_prefix) || idlist::is_chunk_key(k) || wanted.contains_key(k) {
                continue;
            }
            if fix_index(k, &[])? {
                dropped += 1;
            }
        }

        Ok(RepairReport {
            readded,
            pruned,
            total: real.len() as u64,
            indexes: wanted.len() as u64,
            indexes_dropped: dropped,
        })
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    /// The escape scheme has to be INJECTIVE: `_` is both the key joiner and the escape prefix,
    /// so it must never survive unescaped, or collection `a_b` and `a` with ids starting `b`
    /// would share keys.
    #[test]
    fn the_joiner_cannot_be_forged_from_inside_a_segment() {
        assert_eq!(sanitize("a_b"), "a_5Fb", "an underscore must be escaped");
        assert_ne!(rec_key("a_b", "c"), rec_key("a", "b_c"));
        assert_ne!(rec_key("a", "b"), rec_key("a_b", ""));
        assert_ne!(ix_key("c", "f_g", "v"), ix_key("c", "f", "g_v"));
    }

    ///
    #[test]
    fn distinct_segments_give_distinct_keys() {
        let segs = ["", "a", "a_", "_a", "a/b", "a=b", "a.b", "a b", "é", "a_5Fb", "A", "0"];
        let mut seen = std::collections::HashSet::new();
        for c in segs {
            for id in segs {
                assert!(seen.insert(rec_key(c, id)), "collision on ({c:?}, {id:?})");
            }
        }
    }

    /// The bytes that pass through untouched are the ones NATS accepts, and the
    /// rest become `_XX`. Pinned because widening this set later would silently
    /// change every existing key and orphan the records behind them.
    #[test]
    fn only_nats_legal_bytes_survive_unescaped() {
        assert_eq!(sanitize("azAZ09-/="), "azAZ09-/=");
        assert_eq!(sanitize("."), "_2E");
        assert_eq!(sanitize(" "), "_20");
        assert_eq!(sanitize("%"), "_25");
        // Multi-byte UTF-8 is escaped per BYTE, so a key stays ASCII.
        assert_eq!(sanitize("é"), "_C3_A9");
        assert!(sanitize("naïve").is_ascii());
    }

    /// An indexed value is capped, and the cap may OVER-match (readers re-verify) but must never
    /// under-match.
    #[test]
    fn a_long_indexed_value_is_truncated_and_can_only_over_match() {
        let long = "x".repeat(MAX_INDEXED_VALUE + 50);
        assert_eq!(sanitize_value(&long).len(), MAX_INDEXED_VALUE);
        let a = format!("{long}aaa");
        let b = format!("{long}bbb");
        assert_eq!(ix_key("c", "f", &a), ix_key("c", "f", &b), "a shared prefix over-matches");
        assert_eq!(sanitize_value("short"), "short");
        // Escaping happens BEFORE the cap, so the cap is on key bytes and the
        // key cannot exceed it however many escapes a value needs.
        assert!(sanitize_value(&"é".repeat(100)).len() <= MAX_INDEXED_VALUE);
    }

    /// The cursor is exclusive, and an absent cursor resumes at the insertion point, so a deleted
    /// cursor record does not restart paging.
    #[test]
    fn paging_is_exclusive_and_survives_a_deleted_cursor() {
        let ids: Vec<String> = ["a", "c", "e", "g"].iter().map(|s| s.to_string()).collect();
        assert_eq!(idlist::page_start(&ids, ""), 0, "no cursor starts at the beginning");
        assert_eq!(idlist::page_start(&ids, "a"), 1);
        assert_eq!(idlist::page_start(&ids, "g"), 4, "the last id yields an empty page");
        assert_eq!(idlist::page_start(&ids, "b"), 1, "a deleted cursor resumes after where it was");
        assert_eq!(idlist::page_start(&ids, "z"), 4, "a cursor past the end yields nothing");
        assert_eq!(idlist::page_start(&[], "a"), 0);
    }

    /// Chunk keys must sort in sequence order as STRINGS, because that is how
    /// they come back from a prefix scan. Without the zero padding `_c10` sorts
    /// before `_c2` and the id list silently reorders after the tenth chunk.
    #[test]
    fn an_update_that_changes_no_indexed_field_touches_no_index() {
        // The treasury bug: every balance update rewrote the `name` index, and
        let fields = vec!["name".to_string(), "name".to_string(), "absent".to_string()];
        let before = serde_json::json!({"name": "a", "units": 10});
        let after = serde_json::json!({"name": "a", "units": 0});
        let (old, new) = (
            secondary_keys("accounts", &before, &fields),
            secondary_keys("accounts", &after, &fields),
        );
        assert_eq!(old, new);
        assert_eq!(old.len(), 1, "a repeated field is one key, an absent one is none");
        let renamed = secondary_keys("accounts", &serde_json::json!({"name": "b"}), &fields);
        assert_ne!(old, renamed);
    }

    #[test]
    fn chunk_keys_sort_lexicographically_in_sequence_order() {
        let mut keys: Vec<String> = [0u32, 2, 9, 10, 11, 100, 12345]
            .iter()
            .map(|n| idlist::chunk_key("idx_c", *n))
            .collect();
        let ordered = keys.clone();
        keys.sort();
        assert_eq!(keys, ordered, "string order must match numeric order");
        assert_eq!(idlist::chunk_key("idx_c", 0), "idx_c_c00000000");
    }
}
