//! `record-store` — store JSON records in named collections, query them by field, and index them
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

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 500;
/// Truncation can only over-match, which readers re-filter.
const MAX_INDEXED_VALUE: usize = 120;

/// `data` is kept verbatim so re-serialization can't reorder or normalize it.
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

/// Same byte scheme as config-store's `sanitize` / idempotency-guard's `id_key`.
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

fn sanitize_value(v: &str) -> String {
    let mut s = sanitize(v);
    if s.len() > MAX_INDEXED_VALUE {
        s.truncate(MAX_INDEXED_VALUE);
    }
    s
}

fn rec_key(collection: &str, id: &str) -> String {
    format!("rec_{}_{}", sanitize(collection), sanitize(id))
}

fn idx_key(collection: &str) -> String {
    format!("idx_{}", sanitize(collection))
}

fn ix_key(collection: &str, field: &str, value: &str) -> String {
    format!("ix_{}_{}_{}", sanitize(collection), sanitize(field), sanitize_value(value))
}

// ULID: 48-bit ms | 80-bit random in 26 Crockford chars. No intra-ms monotonicity: same-ms
// ids sort arbitrarily among themselves, which the id index tolerates.

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn mint_ulid() -> String {
    let ms = now_ms() & 0xFFFF_FFFF_FFFF;
    let rand = get_random_bytes(10);

    let mut value: u128 = ms;
    for &b in rand.iter() {
        value = (value << 8) | (b as u128);
    }

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

/// Instances are per-request (ADR-0037), so no counter: one line per occurrence.
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

/// The id index has its own larger bound (`idlist::CAS_TRIES`): every create contends on one chunk.
const CAS_TRIES: u32 = 40;

/// A corrupt stored record is `backend-unavailable`: our bug, not bad input.
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

/// A get-many error propagates: a missing batch interface fails at link time, and falling back
/// to sequential gets turned one transient error into a 20-second page.
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

// `idlist`'s `Kv`: the guarded half goes through `comp:store/cas` (never cached), the reader
// half through plain `wasi:keyvalue` (which a host may cache, ADR-0064).

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

/// Compact, so it matches the `value` callers pass to `find-by` / `filter`.
fn field_value(parsed: &Value, field: &str) -> Option<String> {
    parsed.as_object().and_then(|obj| obj.get(field)).map(|v| v.to_string())
}

fn secondary_keys(collection: &str, parsed: &Value, index_fields: &[String]) -> Vec<String> {
    let mut keys: Vec<String> = index_fields
        .iter()
        .filter_map(|field| field_value(parsed, field).map(|v| ix_key(collection, field, &v)))
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// Each attempt is a full CAS loop, so this only matters for a backend error.
const INDEX_TRIES: u32 = 3;

/// Must NOT fail the call: the record landed, and an `err` misled callers (`treasury:ledger`
/// refunded transfers whose debit had landed). Missed edits are left to `repair`.
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

/// Touch only keys whose value changed: rewriting unchanged ones made concurrent updates contend
/// and hid the record from `find-by` between the remove and the add.
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

        // `expected: 0`, so an id collision retries with a fresh id instead of overwriting.
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

        // Committed: an `err` from here makes callers create again, leaving a live unlisted duplicate.
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

        // Compared via `comp:store/cas`: comparing here raced other nodes and the host cache (ADR-0065).
        for _ in 0..CAS_TRIES {
            let (store_revision, bytes) = match cas::get(&bucket, &key) {
                Ok(Some(v)) => (v.revision, v.value),
                Ok(None) => return Err(StoreError::NotFound),
                Err(e) => return Err(StoreError::BackendUnavailable(format!("cas get: {e:?}"))),
            };
            let current: Stored = serde_json::from_slice(&bytes)
                .map_err(|e| StoreError::BackendUnavailable(format!("corrupt record {id}: {e}")))?;

            // The caller's revision is the record's own, not the backend sequence; both must hold.
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
                    // Landed: the index writes below are separate and must not report `err`.
                    let old_keys = serde_json::from_str::<Value>(&current.data)
                        .map(|v| secondary_keys(&collection, &v, &current.index_fields))
                        .unwrap_or_default();
                    let new_keys = secondary_keys(&collection, &parsed_new, &stored.index_fields);
                    reindex_after_commit(&bucket, &collection, &id, "update", &old_keys, &new_keys);
                    return Ok(entry_from(&id, stored));
                }
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
        let Some(stored) = load_record(&bucket, &collection, &id)? else {
            return Ok(());
        };

        // Record first, the only step that can fail; an interrupted delete leaves only dangling ids.
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

        let (window, more) =
            idlist::page(&BucketKv(&bucket), &idx_key(&collection), &after, limit).map_err(be)?;
        let entries: Vec<Entry> = load_records_many(&bucket, &collection, &window)?
            .into_iter()
            .map(|(id, stored)| entry_from(&id, stored))
            .collect();
        // The only drift a read can see (ADR-0075); a record the index never mentions needs `verify`.
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
            // Re-verify: the sanitized, capped index key can over-match.
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

        // The first filter's index only narrows candidates (it may over-match or be absent); every
        // record is re-checked against ALL filters.
        let candidates = match filters.first() {
            Some(f) => {
                let ix = read_ix(&bucket, &ix_key(&collection, &f.field, &f.value))?;
                // A non-indexed first filter has no index key, so scan when it is empty.
                if ix.is_empty() {
                    read_id_index(&bucket, &collection)?
                } else {
                    ix
                }
            }
            None => read_id_index(&bucket, &collection)?,
        };

        // Chunked so a big scan stops once `limit` matches are found.
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
        idlist::count(&BucketKv(&bucket), &idx_key(&collection)).map_err(be)
    }

    /// Scans the whole bucket, so an operator action, not something `list` does (ADR-0068).
    fn repair(collection: String) -> Result<RepairReport, StoreError> {
        repair_inner(&collection, true)
    }

    /// `repair` without writing (ADR-0075).
    fn verify(collection: String) -> Result<RepairReport, StoreError> {
        repair_inner(&collection, false)
    }
}

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
        // `sanitize` is identity for a ULID, so the stored suffix IS the id.
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

        // Patched id by id: a wholesale write erases ids inserted after the scan.
        let lists = BucketKv(&bucket);
        if write {
            let ix = idx_key(&collection);
            for id in &missing {
                idlist::insert(&lists, &ix, id).map_err(be)?;
            }
            for id in &dangling {
                // Re-checked: a record created after the scan must not be pruned.
                if load_record(&bucket, &collection, id)?.is_none() {
                    idlist::remove(&lists, &ix, id).map_err(be)?;
                }
            }
            idlist::heal(&lists, &ix).map_err(be)?;
        }

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

        // An orphaned index key over-matches until re-verified; counted only while it still names
        // something, so a second run reports zero.
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

    /// `_` is both the key joiner and the escape prefix, so it must never survive unescaped.
    #[test]
    fn the_joiner_cannot_be_forged_from_inside_a_segment() {
        assert_eq!(sanitize("a_b"), "a_5Fb", "an underscore must be escaped");
        assert_ne!(rec_key("a_b", "c"), rec_key("a", "b_c"));
        assert_ne!(rec_key("a", "b"), rec_key("a_b", ""));
        assert_ne!(ix_key("c", "f_g", "v"), ix_key("c", "f", "g_v"));
    }

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

    /// Pinned: widening the passthrough set would change every existing key and orphan records.
    #[test]
    fn only_nats_legal_bytes_survive_unescaped() {
        assert_eq!(sanitize("azAZ09-/="), "azAZ09-/=");
        assert_eq!(sanitize("."), "_2E");
        assert_eq!(sanitize(" "), "_20");
        assert_eq!(sanitize("%"), "_25");
        assert_eq!(sanitize("é"), "_C3_A9");
        assert!(sanitize("naïve").is_ascii());
    }

    /// The cap may over-match (readers re-verify) but never under-match.
    #[test]
    fn a_long_indexed_value_is_truncated_and_can_only_over_match() {
        let long = "x".repeat(MAX_INDEXED_VALUE + 50);
        assert_eq!(sanitize_value(&long).len(), MAX_INDEXED_VALUE);
        let a = format!("{long}aaa");
        let b = format!("{long}bbb");
        assert_eq!(ix_key("c", "f", &a), ix_key("c", "f", &b), "a shared prefix over-matches");
        assert_eq!(sanitize_value("short"), "short");
        // Escaping happens before the cap, so the cap bounds key bytes.
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

    #[test]
    fn an_update_that_changes_no_indexed_field_touches_no_index() {
        // The treasury bug: every balance update rewrote the unchanged `name` index.
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

    /// Prefix scans return string order, so without zero padding `_c10` sorts before `_c2`.
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
