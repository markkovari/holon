//! Chunked, sorted id lists over a store with per-key compare-and-set — the index
//! layer under `record-store`'s id index and every secondary index.
//!
//! # Layout (unchanged on disk)
//!
//! A list at `base` is a MANIFEST at `base` plus chunk values at
//! `{base}_c{seq:08}`, each a sorted JSON `Vec<String>`. The manifest names the
//! chunks in id order (`first` = smallest id, `count` = length) and is the only
//! thing a reader trusts to find them: an id is IN the list exactly when it sits in
//! a chunk the manifest names. A legacy whole-array value at `base` is still read,
//! and converted on the first write.
//!
//! Two fields are new and both are optional, so old manifests read unchanged and
//! old readers ignore them: `ChunkMeta::rev`, the chunk revision the entry was
//! computed from, and `Manifest::next_seq`, the next chunk sequence never handed
//! out.
//!
//! # The guarantee
//!
//! Every write to a manifest or a chunk is a compare-and-set against the revision
//! of the value the write was computed from — read, compute, CAS with that
//! revision, and on conflict re-read and recompute. Nothing here writes a list key
//! unconditionally and nothing here deletes one. From that:
//!
//! * An `insert` that returns `Ok` has its id in a chunk the manifest names, and no
//!   concurrent `insert`/`remove` of a DIFFERENT id can take it out again. Likewise
//!   a `remove` that returns `Ok` leaves the id in no named chunk it was routed to
//!   or found in, and cannot resurrect or drop anybody else's id.
//! * Once writers go quiet, every manifest entry agrees with its chunk (`count`,
//!   `first`, `rev`) — the last writer to touch a chunk re-derives its entry from
//!   a read taken after its own write, and the `rev` equality check stops an older
//!   derivation from landing on top of it.
//! * Chunk keys are never deleted, so a key's revision never resets and a CAS can
//!   never match a revision from a previous life of the key (ABA). An emptied chunk
//!   is dropped from the manifest and left behind as `[]`; new chunks always take
//!   a fresh key (`expected: 0`).
//!
//! What is NOT guaranteed:
//!
//! * Concurrent `insert` and `remove` of the SAME id have no defined winner. Index
//!   upkeep in `record-store` runs after the record commits, in no particular order
//!   against another request's upkeep, so the record is the arbiter — every reader
//!   re-verifies against it and `repair` rebuilds from it.
//! * A process that dies between writing a chunk and naming it leaves that id out
//!   of the list until `repair`. There is no multi-key transaction to prevent it.
//! * An id can transiently sit in the chunk BEFORE the one its value routes to (a
//!   writer routed on a manifest read just before a new chunk was named). Readers
//!   sort and dedupe, `page` also reads one chunk back, and `remove` falls back to
//!   searching every chunk, so this costs order-of-work, not membership.
//!
//! # A backend without CAS
//!
//! There is no fallback. This module needs `comp:store/cas`, the host links that
//! interface for every backend it ships (memory, sqlite, redis, nats, surreal,
//! turso — `KvBackend::set_if_revision` has no default so a backend cannot forget
//! it), and a host that lacks it fails to instantiate `record-store` at link time
//! rather than degrading to a racy read-modify-write.

use serde::{Deserialize, Serialize};

/// Soft cap on ids per chunk (~30 KB of ULIDs). Appending past it starts a new
/// chunk; an insert into the MIDDLE of a full chunk just grows it, because moving
/// ids between chunks cannot be made safe against a concurrent remove without a
/// multi-key transaction (see `insert`).
pub const CHUNK_MAX: usize = 1024;

/// How many CAS races one operation may lose before it gives up.
///
/// Every loss means another writer's CAS on the same key LANDED — nothing here
/// retries against a conflict nobody caused — so this bounds starvation, not
/// livelock. N writers on one key need up to about N rounds for the unluckiest of
/// them, and the treasury gate puts two dozen creates on one id index at once:
/// forty (what this was) lost all its races once in thirty runs. The cost of a
/// larger number is paid only while contended.
pub const CAS_TRIES: u32 = 200;

/// The store the lists live in. Implemented over `wasi:keyvalue` +
/// `comp:store/cas` in the component and over a map in the tests.
pub trait Kv {
    /// Value and revision, `None` when absent. Must never be served from a cache.
    fn get(&self, key: &str) -> Result<Option<(u64, Vec<u8>)>, String>;
    /// Write only if the key is at `expected` (0 = absent). `Ok(Ok(rev))` landed at
    /// `rev`; `Ok(Err(current))` did not.
    fn cas(&self, key: &str, value: &[u8], expected: u64) -> Result<Result<u64, u64>, String>;
    /// A plain read for readers. May be cached.
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>, String>;
    /// Plain batched read, answers in input order.
    fn read_many(&self, keys: &[String]) -> Result<Vec<Option<Vec<u8>>>, String>;
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChunkMeta {
    /// Chunk-key suffix. Allocation order only; position is the manifest's order.
    pub seq: u32,
    /// Smallest id in the chunk.
    pub first: String,
    /// Number of ids in the chunk.
    pub count: u64,
    /// The chunk's revision when this entry was derived from it. 0 in manifests
    /// written before it existed, which simply never matches and gets re-derived.
    #[serde(default)]
    pub rev: u64,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Manifest {
    pub chunks: Vec<ChunkMeta>,
    /// Lowest chunk seq not yet handed out. Allocation still probes with
    /// `expected: 0`, so an old manifest without it is only slower, not wrong.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub next_seq: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

pub fn chunk_key(base: &str, seq: u32) -> String {
    format!("{base}_c{seq:08}")
}

/// Is `key` a chunk key (`…_c` + eight digits)? Unambiguous for the keys
/// record-store builds: `sanitize` only ever emits `_` followed by UPPERCASE hex,
/// so `_c` cannot come out of a sanitized segment.
pub fn is_chunk_key(key: &str) -> bool {
    let b = key.as_bytes();
    b.len() > 10
        && &b[b.len() - 10..b.len() - 8] == b"_c"
        && b[b.len() - 8..].iter().all(u8::is_ascii_digit)
}

fn enc<T: Serialize>(v: &T) -> Result<Vec<u8>, String> {
    serde_json::to_vec(v).map_err(|e| format!("encode: {e}"))
}

enum Head {
    Chunked(Manifest),
    /// Pre-chunking layout: the whole sorted array in one value.
    Legacy(Vec<String>),
}

fn parse_head(base: &str, bytes: &[u8]) -> Result<Head, String> {
    // A manifest is a JSON object, the legacy layout a JSON array.
    if let Ok(m) = serde_json::from_slice::<Manifest>(bytes) {
        return Ok(Head::Chunked(m));
    }
    serde_json::from_slice::<Vec<String>>(bytes)
        .map(Head::Legacy)
        .map_err(|e| format!("corrupt id list {base}: {e}"))
}

/// The head with its revision, for a writer. Absent reads as an empty manifest at
/// revision 0, which is what a CAS wants for "must not exist yet".
fn load(kv: &dyn Kv, base: &str) -> Result<(u64, Head), String> {
    match kv.get(base)? {
        None => Ok((0, Head::Chunked(Manifest::default()))),
        Some((rev, bytes)) => Ok((rev, parse_head(base, &bytes)?)),
    }
}

/// A chunk with its revision. Absent reads as `(0, [])`.
fn read_chunk(kv: &dyn Kv, key: &str) -> Result<(u64, Vec<String>), String> {
    match kv.get(key)? {
        None => Ok((0, Vec::new())),
        Some((rev, bytes)) => serde_json::from_slice(&bytes)
            .map(|ids| (rev, ids))
            .map_err(|e| format!("corrupt chunk {key}: {e}")),
    }
}

/// Which chunk should hold `id`: the last whose `first` <= id (ids below every
/// chunk go into the first).
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

fn meta_for(seq: u32, ids: &[String], rev: u64, old_first: &str) -> ChunkMeta {
    ChunkMeta {
        seq,
        // An empty chunk (only ever the sole one) keeps its old `first`; it routes
        // nothing either way.
        first: ids.first().cloned().unwrap_or_else(|| old_first.to_string()),
        count: ids.len() as u64,
        rev,
    }
}

/// Create a chunk under a key nobody has used, holding `ids`. `expected: 0` is what
/// makes the key ours: a key that exists — even as a leftover `[]` — conflicts and
/// the next seq is tried, so no CAS anywhere can confuse two lives of one key.
fn new_chunk(kv: &dyn Kv, base: &str, m: &Manifest, ids: &[String]) -> Result<(u32, u64), String> {
    let start = m.chunks.iter().map(|c| c.seq + 1).max().unwrap_or(0).max(m.next_seq);
    let body = enc(&ids)?;
    for seq in start..start + CAS_TRIES {
        if let Ok(rev) = kv.cas(&chunk_key(base, seq), &body, 0)? {
            return Ok((seq, rev));
        }
    }
    Err(format!("id list {base}: no free chunk key in {CAS_TRIES} probes from {start}"))
}

/// Whether a chunk this call wrote is still part of the list.
#[derive(Debug, PartialEq)]
enum Named {
    Yes,
    /// The manifest no longer names it (dropped as empty while we were writing).
    No,
}

/// Bring the manifest's entry for chunk `seq` in line with the chunk, after a
/// write to it. The manifest is read FIRST and the chunk second, and the manifest
/// is CAS'd against the revision read — so whatever lands was derived from a chunk
/// read taken after the manifest it replaces, and a derivation from an older chunk
/// read cannot overwrite a newer one (its CAS fails, or the `rev` check below
/// makes the newer writer rewrite it).
///
/// Also where chunks leave the list: an empty chunk is dropped when others remain.
/// The drop is a CAS on the manifest taken after reading the chunk empty; an
/// insert racing it either lands its own reconcile first (the drop's CAS fails and
/// it re-reads a non-empty chunk) or finds its chunk un-named and moves its id.
fn reconcile(kv: &dyn Kv, base: &str, seq: u32) -> Result<Named, String> {
    for _ in 0..CAS_TRIES {
        let (mrev, mut m) = match load(kv, base)? {
            (r, Head::Chunked(m)) => (r, m),
            // A chunk never belongs to a legacy list; the caller re-reads.
            (_, Head::Legacy(_)) => return Ok(Named::No),
        };
        let (crev, ids) = read_chunk(kv, &chunk_key(base, seq))?;
        match m.chunks.iter().position(|c| c.seq == seq) {
            // First chunk of an empty list: name it.
            None if m.chunks.is_empty() && !ids.is_empty() => {
                m.chunks.push(meta_for(seq, &ids, crev, ""));
                m.next_seq = m.next_seq.max(seq + 1);
            }
            None => return Ok(Named::No),
            Some(p) if ids.is_empty() && m.chunks.len() > 1 => {
                m.chunks.remove(p);
            }
            Some(p) => {
                let want = meta_for(seq, &ids, crev, &m.chunks[p].first);
                if m.chunks[p] == want {
                    return Ok(Named::Yes);
                }
                m.chunks[p] = want;
            }
        }
        if kv.cas(base, &enc(&m)?, mrev)?.is_ok() {
            return Ok(Named::Yes);
        }
    }
    Err(format!("id list {base}: manifest update lost {CAS_TRIES} races"))
}

/// Name a freshly created chunk `seq` (holding `first`) right after the chunk
/// `routed` that `first` was routed to. `false` if routing for `first` changed in
/// the meantime — someone else started a chunk covering it — and the caller
/// should retry into that one instead.
fn name_new_chunk(
    kv: &dyn Kv,
    base: &str,
    seq: u32,
    first: &str,
    routed: u32,
) -> Result<bool, String> {
    for _ in 0..CAS_TRIES {
        let (mrev, mut m) = match load(kv, base)? {
            (r, Head::Chunked(m)) => (r, m),
            (_, Head::Legacy(_)) => return Ok(false),
        };
        if m.chunks.iter().any(|c| c.seq == seq) {
            return Ok(true);
        }
        if m.chunks.is_empty() {
            return Ok(false);
        }
        let ci = chunk_index_for(&m, first);
        if m.chunks[ci].seq != routed {
            return Ok(false);
        }
        let (crev, ids) = read_chunk(kv, &chunk_key(base, seq))?;
        if ids.is_empty() {
            return Ok(false);
        }
        let at = if m.chunks[ci].first.as_str() <= first { ci + 1 } else { ci };
        m.chunks.insert(at, meta_for(seq, &ids, crev, first));
        m.next_seq = m.next_seq.max(seq + 1);
        if kv.cas(base, &enc(&m)?, mrev)?.is_ok() {
            return Ok(true);
        }
    }
    Err(format!("id list {base}: naming a new chunk lost {CAS_TRIES} races"))
}

/// Take `id` back out of a chunk the manifest does not name. Hygiene only — an
/// un-named chunk is invisible — so failures are ignored.
fn scrub(kv: &dyn Kv, base: &str, seq: u32, id: &str) {
    let key = chunk_key(base, seq);
    for _ in 0..CAS_TRIES {
        let Ok((rev, mut ids)) = read_chunk(kv, &key) else { return };
        let Ok(pos) = ids.binary_search_by(|x| x.as_str().cmp(id)) else { return };
        ids.remove(pos);
        let Ok(body) = enc(&ids) else { return };
        match kv.cas(&key, &body, rev) {
            Ok(Ok(_)) | Err(_) => return,
            Ok(Err(_)) => continue,
        }
    }
}

/// Convert a legacy whole-array list to the chunked layout, guarded like
/// everything else: fresh chunk keys, then a CAS of the head against the revision
/// the array was read at. A loser's chunks are un-named and emptied; the caller
/// re-reads and finds the winner's manifest.
fn convert_legacy(
    kv: &dyn Kv,
    base: &str,
    head_rev: u64,
    ids: &[String],
    cap: usize,
) -> Result<(), String> {
    let mut m = Manifest::default();
    let mut made = Vec::new();
    for part in ids.chunks(cap.max(1)) {
        let (seq, rev) = new_chunk(kv, base, &m, part)?;
        made.push((seq, rev));
        m.chunks.push(ChunkMeta { seq, first: part[0].clone(), count: part.len() as u64, rev });
        m.next_seq = seq + 1;
    }
    if kv.cas(base, &enc(&m)?, head_rev)?.is_err() {
        let empty = enc(&Vec::<String>::new())?;
        for (seq, rev) in made {
            let _ = kv.cas(&chunk_key(base, seq), &empty, rev);
        }
    }
    Ok(())
}

// ---- writers ----------------------------------------------------------------

/// Insert `id`, keeping the list sorted and deduplicated.
pub fn insert(kv: &dyn Kv, base: &str, id: &str) -> Result<(), String> {
    insert_capped(kv, base, id, CHUNK_MAX)
}

/// `insert` with the chunk cap as a parameter, so tests can reach the new-chunk
/// path without a thousand ids.
///
/// A full chunk is never split in half. That would copy ids into a new chunk and
/// then remove them from the old one, and a `remove` landing between the two
/// resurrects its id from the copy — there is no way to close that without a
/// multi-key transaction. Instead an APPEND to a full chunk starts a new chunk
/// holding just the new id (ULIDs are time-ordered, so that is where nearly every
/// insert lands), and an insert into the middle of a full chunk grows it.
pub fn insert_capped(kv: &dyn Kv, base: &str, id: &str, cap: usize) -> Result<(), String> {
    for _ in 0..CAS_TRIES {
        let m = match load(kv, base)? {
            (rev, Head::Legacy(mut v)) => {
                if let Err(pos) = v.binary_search_by(|x| x.as_str().cmp(id)) {
                    v.insert(pos, id.to_string());
                }
                convert_legacy(kv, base, rev, &v, cap)?;
                continue;
            }
            (_, Head::Chunked(m)) => m,
        };

        if m.chunks.is_empty() {
            // First id: a fresh chunk, named by `reconcile`. Two callers racing an
            // empty list each get their own chunk; one is named, the other finds
            // itself un-named, scrubs, and retries into the winner's.
            let (seq, _) = new_chunk(kv, base, &m, &[id.to_string()])?;
            if reconcile(kv, base, seq)? == Named::Yes {
                return Ok(());
            }
            scrub(kv, base, seq, id);
            continue;
        }

        let ci = chunk_index_for(&m, id);
        let seq = m.chunks[ci].seq;
        let key = chunk_key(base, seq);
        let (crev, mut ids) = read_chunk(kv, &key)?;
        let pos = match ids.binary_search_by(|x| x.as_str().cmp(id)) {
            Ok(_) => {
                // Already there — but an earlier attempt may have died before the
                // manifest caught up, so make sure it has.
                if m.chunks[ci].rev == crev {
                    return Ok(());
                }
                match reconcile(kv, base, seq)? {
                    Named::Yes => return Ok(()),
                    Named::No => continue,
                }
            }
            Err(pos) => pos,
        };

        if ids.len() >= cap && pos == ids.len() {
            let (nseq, _) = new_chunk(kv, base, &m, &[id.to_string()])?;
            if name_new_chunk(kv, base, nseq, id, seq)? {
                return Ok(());
            }
            scrub(kv, base, nseq, id);
            continue;
        }

        ids.insert(pos, id.to_string());
        if kv.cas(&key, &enc(&ids)?, crev)?.is_err() {
            continue;
        }
        match reconcile(kv, base, seq)? {
            Named::Yes => return Ok(()),
            // The chunk was dropped under us: the id went somewhere nobody reads.
            // Take it back out and insert again through the current manifest.
            Named::No => {
                scrub(kv, base, seq, id);
                continue;
            }
        }
    }
    Err(format!("id list {base}: {CAS_TRIES} attempts all lost the race"))
}

enum Removed {
    Done,
    Absent,
    Retry,
}

fn remove_in(kv: &dyn Kv, base: &str, seq: u32, id: &str) -> Result<Removed, String> {
    let key = chunk_key(base, seq);
    let (crev, mut ids) = read_chunk(kv, &key)?;
    let Ok(pos) = ids.binary_search_by(|x| x.as_str().cmp(id)) else {
        return Ok(Removed::Absent);
    };
    ids.remove(pos);
    if kv.cas(&key, &enc(&ids)?, crev)?.is_err() {
        return Ok(Removed::Retry);
    }
    // An emptied chunk is not deleted: it is dropped from the manifest here (when
    // it is not the last one) and its key stays behind as `[]`.
    match reconcile(kv, base, seq)? {
        Named::Yes => Ok(Removed::Done),
        // Dropped while we were in it; look again through the current manifest.
        Named::No => Ok(Removed::Retry),
    }
}

/// Remove `id`. Idempotent.
pub fn remove(kv: &dyn Kv, base: &str, id: &str) -> Result<(), String> {
    remove_capped(kv, base, id, CHUNK_MAX)
}

pub fn remove_capped(kv: &dyn Kv, base: &str, id: &str, cap: usize) -> Result<(), String> {
    for _ in 0..CAS_TRIES {
        let m = match load(kv, base)? {
            (rev, Head::Legacy(v)) => {
                if !v.iter().any(|x| x == id) {
                    return Ok(());
                }
                let v: Vec<String> = v.into_iter().filter(|x| x != id).collect();
                convert_legacy(kv, base, rev, &v, cap)?;
                continue;
            }
            (_, Head::Chunked(m)) => m,
        };
        if m.chunks.is_empty() {
            return Ok(());
        }
        let routed = m.chunks[chunk_index_for(&m, id)].seq;
        match remove_in(kv, base, routed, id)? {
            Removed::Done => return Ok(()),
            Removed::Retry => continue,
            Removed::Absent => {}
        }
        // Not where it routes. It may sit one chunk early (a stale writer), so look
        // through the rest before calling it absent.
        let others: Vec<u32> = m.chunks.iter().map(|c| c.seq).filter(|s| *s != routed).collect();
        let keys: Vec<String> = others.iter().map(|s| chunk_key(base, *s)).collect();
        let found = kv.read_many(&keys)?.into_iter().zip(&others).find_map(|(v, s)| {
            let ids: Vec<String> = serde_json::from_slice(v.as_deref()?).ok()?;
            ids.binary_search_by(|x| x.as_str().cmp(id)).is_ok().then_some(*s)
        });
        let Some(seq) = found else { return Ok(()) };
        match remove_in(kv, base, seq, id)? {
            Removed::Done => return Ok(()),
            Removed::Retry | Removed::Absent => continue,
        }
    }
    Err(format!("id list {base}: {CAS_TRIES} attempts all lost the race"))
}

/// Re-derive every manifest entry from its chunk and drop empty chunks. What a
/// `repair` runs so a manifest left inconsistent by an older version (or a crash)
/// converges; a no-op on a consistent list.
pub fn heal(kv: &dyn Kv, base: &str) -> Result<(), String> {
    let seqs: Vec<u32> = match load(kv, base)? {
        (_, Head::Chunked(m)) => m.chunks.iter().map(|c| c.seq).collect(),
        (_, Head::Legacy(_)) => return Ok(()),
    };
    for seq in seqs {
        reconcile(kv, base, seq)?;
    }
    Ok(())
}

// ---- readers ----------------------------------------------------------------

fn read_head(kv: &dyn Kv, base: &str) -> Result<Option<Head>, String> {
    match kv.read(base)? {
        None => Ok(None),
        Some(bytes) => parse_head(base, &bytes).map(Some),
    }
}

/// The named chunks' ids, concatenated, sorted and deduplicated (an id can
/// transiently sit in two chunks, or one chunk early). Missing chunks are skipped.
fn fetch(kv: &dyn Kv, keys: &[String]) -> Result<Vec<String>, String> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for (key, v) in keys.iter().zip(kv.read_many(keys)?) {
        if let Some(bytes) = v {
            let ids: Vec<String> =
                serde_json::from_slice(&bytes).map_err(|e| format!("corrupt chunk {key}: {e}"))?;
            out.extend(ids);
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Every id in the list, in order — a manifest read plus ONE batched chunk fetch.
pub fn read_all(kv: &dyn Kv, base: &str) -> Result<Vec<String>, String> {
    match read_head(kv, base)? {
        None => Ok(Vec::new()),
        Some(Head::Legacy(v)) => Ok(v),
        Some(Head::Chunked(m)) => {
            let keys: Vec<String> = m.chunks.iter().map(|c| chunk_key(base, c.seq)).collect();
            fetch(kv, &keys)
        }
    }
}

/// Position of the first id strictly after `after` in a sorted list.
pub fn page_start(ids: &[String], after: &str) -> usize {
    if after.is_empty() {
        return 0;
    }
    match ids.binary_search_by(|x| x.as_str().cmp(after)) {
        Ok(pos) => pos + 1,
        Err(pos) => pos, // `after` not present: resume where it would be.
    }
}

/// Up to `want` ids strictly after `after`, plus whether more remain — fetches
/// only the chunks the page touches.
pub fn page(
    kv: &dyn Kv,
    base: &str,
    after: &str,
    want: usize,
) -> Result<(Vec<String>, bool), String> {
    let m = match read_head(kv, base)? {
        None => return Ok((Vec::new(), false)),
        Some(Head::Legacy(ids)) => {
            let start = page_start(&ids, after);
            let window: Vec<String> = ids.iter().skip(start).take(want).cloned().collect();
            let more = start + window.len() < ids.len();
            return Ok((window, more));
        }
        Some(Head::Chunked(m)) => m,
    };
    if m.chunks.is_empty() {
        return Ok((Vec::new(), false));
    }
    // Skip whole chunks that end at-or-before `after`: chunk i's ids are all
    // < chunks[i+1].first. Then step ONE chunk back, because an id can sit one
    // chunk early (see the module doc) and would otherwise be skipped.
    let mut start_chunk = 0;
    if !after.is_empty() {
        while start_chunk + 1 < m.chunks.len() && m.chunks[start_chunk + 1].first.as_str() <= after
        {
            start_chunk += 1;
        }
    }
    let from_chunk = start_chunk.saturating_sub(1);
    // Worst case every id in the chunks up to the start is <= `after`.
    let skip_bound: usize =
        m.chunks[from_chunk..=start_chunk].iter().map(|c| c.count as usize).sum();
    let mut upto = from_chunk;
    let mut covered = 0usize;
    loop {
        // Take chunks until their counts cover the skip plus the page...
        while upto < m.chunks.len() {
            covered += m.chunks[upto].count as usize;
            upto += 1;
            if covered >= skip_bound + want {
                break;
            }
        }
        let keys: Vec<String> =
            m.chunks[from_chunk..upto].iter().map(|c| chunk_key(base, c.seq)).collect();
        let ids = fetch(kv, &keys)?;
        let from = page_start(&ids, after);
        let window: Vec<String> = ids.iter().skip(from).take(want).cloned().collect();
        // ...and if a count was stale and the page came up short, take more rather
        // than end a listing early.
        if window.len() < want && upto < m.chunks.len() {
            covered = 0;
            continue;
        }
        let more = ids.len() - from > window.len() || upto < m.chunks.len();
        return Ok((window, more));
    }
}

/// Number of ids — the manifest's counts, one read regardless of size.
pub fn count(kv: &dyn Kv, base: &str) -> Result<u64, String> {
    Ok(match read_head(kv, base)? {
        None => 0,
        Some(Head::Legacy(v)) => v.len() as u64,
        Some(Head::Chunked(m)) => m.chunks.iter().map(|c| c.count).sum(),
    })
}

#[cfg(test)]
#[path = "idlist_tests.rs"]
mod tests;
