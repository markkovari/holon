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

/// Load many records in ONE backend round-trip via `wasi:keyvalue/batch`
/// get-many. Returns (id, record) pairs in input-id order, skipping absent
/// ids. A get-many error propagates: every supported host links the batch
/// interface (a host without it fails at LINK time), so a runtime error is a
/// real backend fault — degrading to N sequential per-key gets there turned
/// one transient error into a 20-second page.
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
    // get-many returns (key, bytes) pairs; map back to ids in input order.
    let mut by_key: std::collections::HashMap<String, Vec<u8>> =
        found.into_iter().flatten().collect();
    let mut out = Vec::with_capacity(ids.len());
    for (id, key) in ids.iter().zip(&keys) {
        if let Some(bytes) = by_key.remove(key) {
            let stored = serde_json::from_slice::<Stored>(&bytes).map_err(|e| {
                StoreError::BackendUnavailable(format!("corrupt record {id}: {e}"))
            })?;
            out.push((id.clone(), stored));
        }
    }
    Ok(out)
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

// ---- chunked sorted id lists ---------------------------------------------
//
// An id list (the per-collection id index AND every secondary index) is a
// small MANIFEST at `{base}` plus chunk values at `{base}_c{seq:08}`, each a
// sorted JSON Vec<String> of at most CHUNK_MAX ids. The old layout (one JSON
// array holding every id) made each insert an O(N) read-modify-write of an
// unboundedly growing value — ~400 KB per create at 13k records, with a hard
// wall at NATS's 1 MiB message cap. Chunks keep every write O(CHUNK_MAX)
// regardless of collection size; new ULIDs sort last, so inserts touch only
// the final chunk. A legacy whole-array value is still readable and is split
// into chunks on the first write. Same single-writer best-effort RMW caveat
// as before: no CAS in wasi:keyvalue@0.2.0-draft.

const CHUNK_MAX: usize = 1024; // ~30 KB of ULIDs per chunk value

#[derive(Serialize, Deserialize)]
struct ChunkMeta {
    /// chunk-key suffix (allocation order; position comes from the manifest's
    /// order, which is kept sorted by `first`).
    seq: u32,
    /// smallest id in the chunk.
    first: String,
    /// number of ids in the chunk.
    count: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct Manifest {
    chunks: Vec<ChunkMeta>,
}

enum IdList {
    Absent,
    /// pre-chunking layout: the whole sorted array in one value.
    Legacy(Vec<String>),
    Chunked(Manifest),
}

fn chunk_key(base: &str, seq: u32) -> String {
    format!("{base}_c{seq:08}")
}

fn enc<T: Serialize>(v: &T) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(v).map_err(|e| StoreError::BackendUnavailable(format!("encode: {e}")))
}

fn ids_set_many(bucket: &kv::Bucket, writes: Vec<(String, Vec<u8>)>) -> Result<(), StoreError> {
    batch::set_many(bucket, &writes)
        .map_err(|e| StoreError::BackendUnavailable(format!("set-many: {e:?}")))
}

fn ids_load(bucket: &kv::Bucket, base: &str) -> Result<IdList, StoreError> {
    match bucket.get(base) {
        Ok(Some(bytes)) => {
            // a manifest is a JSON object, the legacy layout a JSON array.
            if let Ok(m) = serde_json::from_slice::<Manifest>(&bytes) {
                Ok(IdList::Chunked(m))
            } else {
                serde_json::from_slice::<Vec<String>>(&bytes)
                    .map(IdList::Legacy)
                    .map_err(|e| {
                        StoreError::BackendUnavailable(format!("corrupt id list {base}: {e}"))
                    })
            }
        }
        Ok(None) => Ok(IdList::Absent),
        Err(e) => Err(StoreError::BackendUnavailable(format!("get id list: {e:?}"))),
    }
}

fn ids_read_chunk(bucket: &kv::Bucket, key: &str) -> Result<Vec<String>, StoreError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::BackendUnavailable(format!("corrupt chunk {key}: {e}"))),
        // manifest points at a missing chunk (best-effort drift): treat empty.
        Ok(None) => Ok(Vec::new()),
        Err(e) => Err(StoreError::BackendUnavailable(format!("get chunk: {e:?}"))),
    }
}

/// Batched fetch of the given chunk keys, concatenated in input order
/// (missing chunks skipped — index drift is best-effort, as before).
fn ids_fetch_chunks(
    bucket: &kv::Bucket,
    keys: &[String],
) -> Result<Vec<String>, StoreError> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let found = batch::get_many(bucket, keys)
        .map_err(|e| StoreError::BackendUnavailable(format!("get-many chunks: {e:?}")))?;
    let mut by_key: std::collections::HashMap<String, Vec<u8>> =
        found.into_iter().flatten().collect();
    let mut out = Vec::new();
    for key in keys {
        if let Some(bytes) = by_key.remove(key) {
            let ids: Vec<String> = serde_json::from_slice(&bytes).map_err(|e| {
                StoreError::BackendUnavailable(format!("corrupt chunk {key}: {e}"))
            })?;
            out.extend(ids);
        }
    }
    Ok(out)
}

/// Every id in the list, in order — manifest read + ONE batched chunk fetch.
fn ids_read_all(bucket: &kv::Bucket, base: &str) -> Result<Vec<String>, StoreError> {
    match ids_load(bucket, base)? {
        IdList::Absent => Ok(Vec::new()),
        IdList::Legacy(v) => Ok(v),
        IdList::Chunked(m) => {
            let keys: Vec<String> = m.chunks.iter().map(|c| chunk_key(base, c.seq)).collect();
            ids_fetch_chunks(bucket, &keys)
        }
    }
}

/// Position of the first id strictly after `after` in a sorted list.
fn page_start(ids: &[String], after: &str) -> usize {
    if after.is_empty() {
        return 0;
    }
    match ids.binary_search_by(|x| x.as_str().cmp(after)) {
        Ok(pos) => pos + 1,
        Err(pos) => pos, // `after` not present: resume where it would be.
    }
}

/// Up to `want` ids strictly after `after` plus whether more remain — fetches
/// only the chunks the page touches, not the whole list.
fn ids_page(
    bucket: &kv::Bucket,
    base: &str,
    after: &str,
    want: usize,
) -> Result<(Vec<String>, bool), StoreError> {
    let m = match ids_load(bucket, base)? {
        IdList::Absent => return Ok((Vec::new(), false)),
        IdList::Legacy(ids) => {
            let start = page_start(&ids, after);
            let window: Vec<String> = ids.iter().skip(start).take(want).cloned().collect();
            let more = start + window.len() < ids.len();
            return Ok((window, more));
        }
        IdList::Chunked(m) => m,
    };
    if m.chunks.is_empty() {
        return Ok((Vec::new(), false));
    }
    // skip whole chunks that end at-or-before `after`: chunk i's ids are all
    // < chunks[i+1].first, so if chunks[i+1].first <= after none can qualify.
    let mut start_chunk = 0;
    if !after.is_empty() {
        while start_chunk + 1 < m.chunks.len()
            && m.chunks[start_chunk + 1].first.as_str() <= after
        {
            start_chunk += 1;
        }
    }
    // fetch chunks until their counts cover the worst-case skip within the
    // first chunk plus the page itself.
    let skip_bound = m.chunks[start_chunk].count as usize;
    let mut keys = Vec::new();
    let mut covered = 0usize;
    let mut fetched_chunks = 0usize;
    for c in &m.chunks[start_chunk..] {
        keys.push(chunk_key(base, c.seq));
        covered += c.count as usize;
        fetched_chunks += 1;
        if covered >= skip_bound + want {
            break;
        }
    }
    let ids = ids_fetch_chunks(bucket, &keys)?;
    let from = page_start(&ids, after);
    let window: Vec<String> = ids.iter().skip(from).take(want).cloned().collect();
    let more = ids.len() - from > window.len()
        || start_chunk + fetched_chunks < m.chunks.len();
    Ok((window, more))
}

fn ids_count(bucket: &kv::Bucket, base: &str) -> Result<u64, StoreError> {
    Ok(match ids_load(bucket, base)? {
        IdList::Absent => 0,
        IdList::Legacy(v) => v.len() as u64,
        IdList::Chunked(m) => m.chunks.iter().map(|c| c.count).sum(),
    })
}

/// Rewrite the whole list in chunked form (legacy conversion / first write).
fn ids_write_chunked(bucket: &kv::Bucket, base: &str, ids: &[String]) -> Result<(), StoreError> {
    let mut writes = Vec::new();
    let mut chunks = Vec::new();
    for (i, chunk) in ids.chunks(CHUNK_MAX).enumerate() {
        let seq = i as u32;
        chunks.push(ChunkMeta {
            seq,
            first: chunk[0].clone(),
            count: chunk.len() as u64,
        });
        writes.push((chunk_key(base, seq), enc(&chunk)?));
    }
    writes.push((base.to_string(), enc(&Manifest { chunks })?));
    ids_set_many(bucket, writes)
}

/// Which manifest chunk should hold `id`: the last chunk whose `first` <= id
/// (ids below every chunk go into the first).
fn chunk_index_for(m: &Manifest, id: &str) -> usize {
    let mut ci = 0;
    for (i, c) in m.chunks.iter().enumerate() {
        if c.first.as_str() <= id {
            ci = i;
        } else {
            break;
        }
    }
    ci
}

/// Insert `id`, keeping the list sorted and deduped. Touches one chunk (two
/// on a split) + the manifest, written in one set-many.
fn ids_insert(bucket: &kv::Bucket, base: &str, id: &str) -> Result<(), StoreError> {
    let mut m = match ids_load(bucket, base)? {
        IdList::Absent => Manifest::default(),
        IdList::Legacy(mut v) => {
            // one-time conversion: fold the insert into the chunked rewrite.
            match v.binary_search_by(|x| x.as_str().cmp(id)) {
                Ok(_) => return Ok(()),
                Err(pos) => v.insert(pos, id.to_string()),
            }
            return ids_write_chunked(bucket, base, &v);
        }
        IdList::Chunked(m) => m,
    };
    if m.chunks.is_empty() {
        return ids_write_chunked(bucket, base, &[id.to_string()]);
    }
    let ci = chunk_index_for(&m, id);
    let ckey = chunk_key(base, m.chunks[ci].seq);
    let mut ids = ids_read_chunk(bucket, &ckey)?;
    match ids.binary_search_by(|x| x.as_str().cmp(id)) {
        Ok(_) => return Ok(()), // already present
        Err(pos) => ids.insert(pos, id.to_string()),
    }
    let mut writes = Vec::new();
    if ids.len() > CHUNK_MAX {
        // split: right half moves to a fresh seq, manifest entry follows.
        let right = ids.split_off(ids.len() / 2);
        let new_seq = m.chunks.iter().map(|c| c.seq).max().unwrap_or(0) + 1;
        m.chunks[ci].first = ids[0].clone();
        m.chunks[ci].count = ids.len() as u64;
        m.chunks.insert(
            ci + 1,
            ChunkMeta { seq: new_seq, first: right[0].clone(), count: right.len() as u64 },
        );
        writes.push((chunk_key(base, new_seq), enc(&right)?));
    } else {
        m.chunks[ci].first = ids[0].clone();
        m.chunks[ci].count = ids.len() as u64;
    }
    writes.push((ckey, enc(&ids)?));
    writes.push((base.to_string(), enc(&m)?));
    ids_set_many(bucket, writes)
}

/// Remove `id`. Touches one chunk + the manifest; an emptied chunk is dropped.
fn ids_remove(bucket: &kv::Bucket, base: &str, id: &str) -> Result<(), StoreError> {
    let mut m = match ids_load(bucket, base)? {
        IdList::Absent => return Ok(()),
        IdList::Legacy(mut v) => {
            let before = v.len();
            v.retain(|x| x != id);
            if v.len() != before {
                return ids_write_chunked(bucket, base, &v);
            }
            return Ok(());
        }
        IdList::Chunked(m) => m,
    };
    if m.chunks.is_empty() {
        return Ok(());
    }
    let ci = chunk_index_for(&m, id);
    let ckey = chunk_key(base, m.chunks[ci].seq);
    let mut ids = ids_read_chunk(bucket, &ckey)?;
    let Ok(pos) = ids.binary_search_by(|x| x.as_str().cmp(id)) else {
        return Ok(());
    };
    ids.remove(pos);
    if ids.is_empty() {
        m.chunks.remove(ci);
        let _ = bucket.delete(&ckey); // best-effort; manifest no longer points at it
        ids_set_many(bucket, vec![(base.to_string(), enc(&m)?)])
    } else {
        m.chunks[ci].first = ids[0].clone();
        m.chunks[ci].count = ids.len() as u64;
        ids_set_many(bucket, vec![(ckey, enc(&ids)?), (base.to_string(), enc(&m)?)])
    }
}

// ---- id index + secondary indexes over the chunked lists ------------------

fn read_id_index(bucket: &kv::Bucket, collection: &str) -> Result<Vec<String>, StoreError> {
    ids_read_all(bucket, &idx_key(collection))
}

fn id_index_insert(bucket: &kv::Bucket, collection: &str, id: &str) -> Result<(), StoreError> {
    ids_insert(bucket, &idx_key(collection), id)
}

fn id_index_remove(bucket: &kv::Bucket, collection: &str, id: &str) -> Result<(), StoreError> {
    ids_remove(bucket, &idx_key(collection), id)
}

fn read_ix(bucket: &kv::Bucket, key: &str) -> Result<Vec<String>, StoreError> {
    ids_read_all(bucket, key)
}

// secondary indexes share the chunked list; entries are now ULID-sorted
// (== creation order) rather than append-order.
fn ix_add(bucket: &kv::Bucket, key: &str, id: &str) -> Result<(), StoreError> {
    ids_insert(bucket, key, id)
}

fn ix_remove(bucket: &kv::Bucket, key: &str, id: &str) -> Result<(), StoreError> {
    ids_remove(bucket, key, id)
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
        let limit = match limit as usize {
            0 => DEFAULT_LIMIT,
            n => n.min(MAX_LIMIT),
        };

        // Page over the chunked id index (fetches only the chunks the page
        // touches), then ONE batched record fetch; ids whose record vanished
        // (best-effort index drift) are skipped by load_records_many.
        let (window, more) = ids_page(&bucket, &idx_key(&collection), &after, limit)?;
        let entries: Vec<Entry> = load_records_many(&bucket, &collection, &window)?
            .into_iter()
            .map(|(id, stored)| entry_from(&id, stored))
            .collect();

        let next = if more {
            window.last().map(|s| s.to_string()).unwrap_or_default()
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
                // AND: every filter's top-level field must JSON-equal its value.
                let matches = filters.iter().all(|f| {
                    field_value(&parsed, &f.field).as_deref() == Some(f.value.as_str())
                });
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
        ids_count(&bucket, &idx_key(&collection))
    }
}

bindings::export!(Component with_types_in bindings);
