//! Deterministic interleavings of the id-list writers, against the algorithm this
//! replaced and against the current one.
//!
//! `MemKv` behaves like the host's memory backend (a revision per key, bumped on
//! every write, compare-and-set under one lock) and can run a second operation
//! to completion immediately before the N-th store call of the first. `explore`
//! tries that at EVERY call boundary of the first operation, so each test below
//! covers all single-preemption schedules of its pair, not a sample of them.

use super::*;
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

type Hook = Box<dyn FnOnce(&MemKv) + Send>;

#[derive(Default)]
struct MemKv {
    map: Mutex<HashMap<String, (u64, Vec<u8>)>>,
    calls: Mutex<u64>,
    hook: Mutex<Option<(u64, Hook)>>,
    yield_each: bool,
}

impl MemKv {
    fn new() -> Self {
        Self::default()
    }

    /// Count a store call; run the hook first if this is the call it waits for.
    fn tick(&self) {
        let fire = {
            let mut c = self.calls.lock().unwrap();
            *c += 1;
            let mut h = self.hook.lock().unwrap();
            match h.as_ref() {
                Some((at, _)) if *at == *c => h.take(),
                _ => None,
            }
        };
        if let Some((_, f)) = fire {
            f(self);
        }
        if self.yield_each {
            std::thread::yield_now();
        }
    }

    fn reset_calls(&self) {
        *self.calls.lock().unwrap() = 0;
    }

    fn calls(&self) -> u64 {
        *self.calls.lock().unwrap()
    }

    /// Run `f` right before the `n`-th store call from now.
    fn at(&self, n: u64, f: impl FnOnce(&MemKv) + Send + 'static) {
        self.reset_calls();
        *self.hook.lock().unwrap() = Some((n, Box::new(f)));
    }

    fn take_hook(&self) -> Option<Hook> {
        self.hook.lock().unwrap().take().map(|(_, f)| f)
    }

    // The unguarded writes the old algorithm used.
    fn set(&self, key: &str, value: &[u8]) {
        self.tick();
        let mut m = self.map.lock().unwrap();
        let rev = m.get(key).map(|(r, _)| *r).unwrap_or(0) + 1;
        m.insert(key.to_string(), (rev, value.to_vec()));
    }

    fn delete(&self, key: &str) {
        self.tick();
        self.map.lock().unwrap().remove(key);
    }

    fn raw(&self, key: &str) -> Option<(u64, Vec<u8>)> {
        self.map.lock().unwrap().get(key).cloned()
    }

    fn put_json<T: Serialize>(&self, key: &str, v: &T) {
        let mut m = self.map.lock().unwrap();
        let rev = m.get(key).map(|(r, _)| *r).unwrap_or(0) + 1;
        m.insert(key.to_string(), (rev, serde_json::to_vec(v).unwrap()));
    }
}

impl Kv for MemKv {
    fn get(&self, key: &str) -> Result<Option<(u64, Vec<u8>)>, String> {
        self.tick();
        Ok(self.raw(key))
    }
    fn cas(&self, key: &str, value: &[u8], expected: u64) -> Result<Result<u64, u64>, String> {
        self.tick();
        let mut m = self.map.lock().unwrap();
        let cur = m.get(key).map(|(r, _)| *r).unwrap_or(0);
        if cur != expected {
            return Ok(Err(cur));
        }
        m.insert(key.to_string(), (cur + 1, value.to_vec()));
        Ok(Ok(cur + 1))
    }
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        self.tick();
        Ok(self.raw(key).map(|(_, v)| v))
    }
    fn read_many(&self, keys: &[String]) -> Result<Vec<Option<Vec<u8>>>, String> {
        self.tick();
        let m = self.map.lock().unwrap();
        Ok(keys.iter().map(|k| m.get(k).map(|(_, v)| v.clone())).collect())
    }
}

const B: &str = "ix_t_state_p";

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn set_of(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Every manifest entry agrees with its chunk, no empty chunk is named beside
/// others, and every read path returns exactly `want`.
fn assert_consistent(kv: &MemKv, want: &BTreeSet<String>) {
    let got: BTreeSet<String> = read_all(kv, B).unwrap().into_iter().collect();
    assert_eq!(&got, want, "membership");
    assert_eq!(count(kv, B).unwrap(), want.len() as u64, "count");
    for size in [1usize, 2, 3, 50] {
        let (mut after, mut paged) = (String::new(), Vec::new());
        loop {
            let (window, more) = page(kv, B, &after, size).unwrap();
            paged.extend(window.iter().cloned());
            match (more, window.last()) {
                (true, Some(last)) => after = last.clone(),
                _ => break,
            }
        }
        let want_v: Vec<String> = want.iter().cloned().collect();
        assert_eq!(paged, want_v, "paging by {size}");
    }
    let Some((_, bytes)) = kv.raw(B) else {
        assert!(want.is_empty());
        return;
    };
    let m: Manifest = serde_json::from_slice(&bytes).unwrap();
    for c in &m.chunks {
        let (rev, bytes) = kv.raw(&chunk_key(B, c.seq)).expect("a named chunk exists");
        let ids: Vec<String> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(c.count, ids.len() as u64, "count of chunk {} in {m:?}", c.seq);
        assert_eq!(c.rev, rev, "rev of chunk {} in {m:?}", c.seq);
        if let Some(f) = ids.first() {
            assert_eq!(&c.first, f, "first of chunk {}", c.seq);
        }
        assert!(
            !(ids.is_empty() && m.chunks.len() > 1),
            "empty chunk {} still named: {m:?}",
            c.seq
        );
    }
}

/// Run `second` before every store call of `first` in turn (and once after it),
/// each time from a fresh `setup`, and hand every final state to `check`.
fn explore(
    setup: &dyn Fn(&MemKv),
    first: &dyn Fn(&MemKv),
    second: fn(&MemKv),
    check: &dyn Fn(&MemKv),
) -> u64 {
    let n = {
        let kv = MemKv::new();
        setup(&kv);
        kv.reset_calls();
        first(&kv);
        kv.calls()
    };
    for k in 1..=n + 1 {
        let kv = MemKv::new();
        setup(&kv);
        kv.at(k, second);
        first(&kv);
        if let Some(f) = kv.take_hook() {
            f(&kv);
        }
        check(&kv);
    }
    n
}

// ---- the algorithm this replaced, verbatim in shape ---------------------------

mod old {
    use super::*;

    pub fn load(kv: &MemKv, base: &str) -> Option<Manifest> {
        kv.read(base).unwrap().map(|b| serde_json::from_slice(&b).unwrap())
    }

    fn guarded(kv: &MemKv, base: &str, m: &Manifest) -> bool {
        // The revision read JUST BEFORE the write — not the one `m` came from.
        let expected = kv.get(base).unwrap().map(|(r, _)| r).unwrap_or(0);
        kv.cas(base, &serde_json::to_vec(m).unwrap(), expected).unwrap().is_ok()
    }

    pub fn insert(kv: &MemKv, base: &str, id: &str, cap: usize) -> Result<(), String> {
        for _ in 0..40 {
            let mut m = load(kv, base).unwrap_or_default();
            if m.chunks.is_empty() {
                let body = serde_json::to_vec(&ids(&[id])).unwrap();
                if kv.cas(&chunk_key(base, 0), &body, 0).unwrap().is_err() {
                    continue;
                }
                let m = Manifest {
                    chunks: vec![ChunkMeta { seq: 0, first: id.into(), count: 1, rev: 0 }],
                    next_seq: 0,
                };
                if !guarded(kv, base, &m) {
                    continue;
                }
                return Ok(());
            }
            let ci = chunk_index_for(&m, id);
            let ckey = chunk_key(base, m.chunks[ci].seq);
            let (crev, mut v) = read_chunk(kv, &ckey).unwrap();
            match v.binary_search_by(|x| x.as_str().cmp(id)) {
                Ok(_) => return Ok(()),
                Err(pos) => v.insert(pos, id.to_string()),
            }
            let mut extra = None;
            if v.len() > cap {
                let right = v.split_off(v.len() / 2);
                let new_seq = m.chunks.iter().map(|c| c.seq).max().unwrap_or(0) + 1;
                m.chunks[ci].first = v[0].clone();
                m.chunks[ci].count = v.len() as u64;
                m.chunks.insert(
                    ci + 1,
                    ChunkMeta {
                        seq: new_seq,
                        first: right[0].clone(),
                        count: right.len() as u64,
                        rev: 0,
                    },
                );
                extra = Some((chunk_key(base, new_seq), serde_json::to_vec(&right).unwrap()));
            } else {
                m.chunks[ci].first = v[0].clone();
                m.chunks[ci].count = v.len() as u64;
            }
            if kv.cas(&ckey, &serde_json::to_vec(&v).unwrap(), crev).unwrap().is_err() {
                continue;
            }
            if let Some((k, body)) = extra {
                kv.set(&k, &body); // plain
            }
            if !guarded(kv, base, &m) {
                continue;
            }
            return Ok(());
        }
        Err(format!("id index {base}: 40 attempts all lost the race"))
    }

    pub fn remove(kv: &MemKv, base: &str, id: &str) -> Result<(), String> {
        for _ in 0..40 {
            let Some(mut m) = load(kv, base) else { return Ok(()) };
            if m.chunks.is_empty() {
                return Ok(());
            }
            let ci = chunk_index_for(&m, id);
            let ckey = chunk_key(base, m.chunks[ci].seq);
            let (crev, mut v) = read_chunk(kv, &ckey).unwrap();
            let Ok(pos) = v.binary_search_by(|x| x.as_str().cmp(id)) else { return Ok(()) };
            v.remove(pos);
            if v.is_empty() {
                m.chunks.remove(ci);
                kv.delete(&ckey); // unguarded
                kv.set(base, &serde_json::to_vec(&m).unwrap()); // unguarded
                return Ok(());
            }
            if kv.cas(&ckey, &serde_json::to_vec(&v).unwrap(), crev).unwrap().is_err() {
                continue;
            }
            m.chunks[ci].first = v[0].clone();
            m.chunks[ci].count = v.len() as u64;
            kv.set(base, &serde_json::to_vec(&m).unwrap()); // unguarded
            return Ok(());
        }
        Err("lost".into())
    }

    /// What a reader of the old layout saw.
    pub fn members(kv: &MemKv, base: &str) -> BTreeSet<String> {
        read_all(kv, base).unwrap().into_iter().collect()
    }
}

// ---- the old bugs, reproduced ------------------------------------------------

/// A `remove` that empties a chunk deletes it and rewrites the manifest with no
/// guard at all. Run an `insert` inside that window and, depending on exactly
/// where, the insert is lost (its CAS'd chunk deleted under it), or lost AND the
/// list is wedged: the insert recreated chunk 0 and named it, the remove's plain
/// manifest write then un-named it, and every later insert takes the bootstrap
/// path, CASes chunk 0 with `expected: 0`, conflicts with the chunk that is still
/// there, and loses all forty "races" — against nobody. That is the shape of the
/// `ix_transfers_state_"pending"` drift line PR #280 saw: a pending index empties
/// every time the last pending transfer settles.
#[test]
fn old_remove_emptying_a_chunk_loses_a_racing_insert_and_can_wedge_the_list() {
    let (mut lost, mut wedged) = (0, 0);
    let n = {
        let kv = MemKv::new();
        old::insert(&kv, B, "p", 1024).unwrap();
        kv.reset_calls();
        old::remove(&kv, B, "p").unwrap();
        kv.calls()
    };
    for k in 1..=n {
        let kv = MemKv::new();
        old::insert(&kv, B, "p", 1024).unwrap();
        kv.at(k, |kv| old::insert(kv, B, "x", 1024).unwrap());
        old::remove(&kv, B, "p").unwrap();
        if !old::members(&kv, B).contains("x") {
            lost += 1;
            if old::insert(&kv, B, "y", 1024).is_err() {
                wedged += 1;
            }
        }
    }
    assert!(lost > 0, "the old remove never lost a racing insert");
    assert!(wedged > 0, "the old remove never wedged the list");
}

/// The same schedules against the current code: nothing lost, nothing wedged.
#[test]
fn remove_emptying_the_only_chunk_keeps_a_racing_insert() {
    explore(
        &|kv| insert(kv, B, "p").unwrap(),
        &|kv| remove(kv, B, "p").unwrap(),
        |kv| insert(kv, B, "x").unwrap(),
        &|kv| {
            assert_consistent(kv, &set_of(&["x"]));
            insert(kv, B, "y").unwrap();
            assert_consistent(kv, &set_of(&["x", "y"]));
        },
    );
}

/// The old manifest guard compared against the revision read just before the
/// write, so a manifest computed from a STALE read went through whenever nobody
/// wrote in that last instant. With a split in between, the stale manifest does
/// not name the split's new chunk, and half a chunk of ids vanishes from the list.
#[test]
fn old_stale_manifest_write_unnames_a_split_chunk() {
    let setup = |kv: &MemKv| {
        for id in ["a", "b", "c", "d"] {
            old::insert(kv, B, id, 4).unwrap();
        }
    };
    let n = {
        let kv = MemKv::new();
        setup(&kv);
        kv.reset_calls();
        old::insert(&kv, B, "b5", 4).unwrap();
        kv.calls()
    };
    let mut lost = 0;
    for k in 1..=n {
        let kv = MemKv::new();
        setup(&kv);
        kv.at(k, |kv| old::insert(kv, B, "e", 4).unwrap());
        old::insert(&kv, B, "b5", 4).unwrap();
        if old::members(&kv, B) != set_of(&["a", "b", "b5", "c", "d", "e"]) {
            lost += 1;
        }
    }
    assert!(lost > 0, "the stale manifest write never lost ids");
}

/// Two splits of DIFFERENT chunks both picked `max(seq) + 1` from their own read
/// and wrote the new chunk with a plain set, so one right half overwrote the other.
#[test]
fn old_concurrent_splits_clobber_one_new_chunk() {
    let setup = |kv: &MemKv| two_full_chunks(kv, true);
    let n = {
        let kv = MemKv::new();
        setup(&kv);
        kv.reset_calls();
        old::insert(&kv, B, "e", 4).unwrap();
        kv.calls()
    };
    let mut lost = 0;
    for k in 1..=n {
        let kv = MemKv::new();
        setup(&kv);
        kv.at(k, |kv| old::insert(kv, B, "q", 4).unwrap());
        old::insert(&kv, B, "e", 4).unwrap();
        let want = set_of(&["a", "b", "c", "d", "e", "m", "n", "o", "p", "q"]);
        if old::members(&kv, B) != want {
            lost += 1;
        }
    }
    assert!(lost > 0, "the old concurrent splits never clobbered each other");
}

/// `repair` emptying an index left chunk 0 behind under an empty manifest, and
/// the old bootstrap (`expected: 0` on chunk 0) could then never succeed again.
#[test]
fn an_emptied_manifest_over_a_leftover_chunk_no_longer_wedges_inserts() {
    let kv = MemKv::new();
    kv.put_json(&chunk_key(B, 0), &ids(&["stale"]));
    kv.put_json(B, &Manifest::default());
    assert!(old::insert(&kv, B, "x", 1024).is_err(), "the old code wedged here");
    insert(&kv, B, "x").unwrap();
    assert_consistent(&kv, &set_of(&["x"]));
}

// ---- the current code under every single-preemption schedule -------------------

fn insert4(kv: &MemKv, id: &str) {
    insert_capped(kv, B, id, 4).unwrap()
}
fn remove4(kv: &MemKv, id: &str) {
    remove_capped(kv, B, id, 4).unwrap()
}

/// c0 = [a b c d], c1 = [m n o p], written directly. `with_revs: false` writes
/// the manifest as the old code did — no `rev`, no `next_seq`.
fn two_full_chunks(kv: &MemKv, with_revs: bool) {
    kv.put_json(&chunk_key(B, 0), &ids(&["a", "b", "c", "d"]));
    kv.put_json(&chunk_key(B, 1), &ids(&["m", "n", "o", "p"]));
    let rev = |seq| if with_revs { kv.raw(&chunk_key(B, seq)).unwrap().0 } else { 0 };
    kv.put_json(
        B,
        &Manifest {
            chunks: vec![
                ChunkMeta { seq: 0, first: "a".into(), count: 4, rev: rev(0) },
                ChunkMeta { seq: 1, first: "m".into(), count: 4, rev: rev(1) },
            ],
            next_seq: if with_revs { 2 } else { 0 },
        },
    );
}

#[test]
fn two_inserts_into_one_chunk_both_land() {
    explore(&|kv| insert4(kv, "a"), &|kv| insert4(kv, "b"), |kv| insert4(kv, "c"), &|kv| {
        assert_consistent(kv, &set_of(&["a", "b", "c"]))
    });
}

#[test]
fn two_first_inserts_into_an_empty_list_both_land() {
    explore(&|_| {}, &|kv| insert4(kv, "a"), |kv| insert4(kv, "b"), &|kv| {
        assert_consistent(kv, &set_of(&["a", "b"]))
    });
}

#[test]
fn a_stale_manifest_cannot_unname_a_new_chunk() {
    // The current code's version of the old split race: "e" appends to a full
    // chunk and starts a new one while "b5" lands in the middle of the old one.
    explore(
        &|kv| ["a", "b", "c", "d"].iter().for_each(|i| insert4(kv, i)),
        &|kv| insert4(kv, "b5"),
        |kv| insert4(kv, "e"),
        &|kv| assert_consistent(kv, &set_of(&["a", "b", "b5", "c", "d", "e"])),
    );
}

#[test]
fn two_new_chunks_started_at_once_get_different_keys() {
    explore(
        &|kv| two_full_chunks(kv, true),
        &|kv| insert4(kv, "e"),
        |kv| insert4(kv, "q"),
        &|kv| assert_consistent(kv, &set_of(&["a", "b", "c", "d", "e", "m", "n", "o", "p", "q"])),
    );
}

#[test]
fn two_appends_racing_to_start_the_same_new_chunk_both_land() {
    explore(
        &|kv| ["a", "b", "c", "d"].iter().for_each(|i| insert4(kv, i)),
        &|kv| insert4(kv, "e"),
        |kv| insert4(kv, "f"),
        &|kv| assert_consistent(kv, &set_of(&["a", "b", "c", "d", "e", "f"])),
    );
}

/// The chunk-drop race: "m".."p" are removed down to one, the last remove
/// empties c1 and drops it, and an insert routed into c1 on the old manifest runs
/// inside that window. It must end up in a named chunk.
#[test]
fn dropping_an_emptied_chunk_keeps_an_insert_routed_into_it() {
    explore(
        &|kv| {
            two_full_chunks(kv, true);
            for id in ["m", "n", "o"] {
                remove4(kv, id);
            }
        },
        &|kv| remove4(kv, "p"),
        |kv| insert4(kv, "x"),
        &|kv| assert_consistent(kv, &set_of(&["a", "b", "c", "d", "x"])),
    );
}

#[test]
fn two_removes_emptying_one_chunk_both_take_effect() {
    explore(
        &|kv| {
            two_full_chunks(kv, true);
            remove4(kv, "m");
            remove4(kv, "n");
        },
        &|kv| remove4(kv, "o"),
        |kv| remove4(kv, "p"),
        &|kv| assert_consistent(kv, &set_of(&["a", "b", "c", "d"])),
    );
}

#[test]
fn a_remove_racing_a_new_chunk_takes_effect() {
    explore(
        &|kv| ["a", "b", "c", "d"].iter().for_each(|i| insert4(kv, i)),
        &|kv| insert4(kv, "e"),
        |kv| remove4(kv, "d"),
        &|kv| assert_consistent(kv, &set_of(&["a", "b", "c", "e"])),
    );
}

#[test]
fn converting_a_legacy_list_keeps_a_racing_insert() {
    explore(
        &|kv| kv.put_json(B, &ids(&["a", "b", "c", "d", "e"])),
        &|kv| insert4(kv, "f"),
        |kv| insert4(kv, "g"),
        &|kv| assert_consistent(kv, &set_of(&["a", "b", "c", "d", "e", "f", "g"])),
    );
}

#[test]
fn converting_a_legacy_list_keeps_a_racing_remove() {
    explore(
        &|kv| kv.put_json(B, &ids(&["a", "b", "c", "d", "e"])),
        &|kv| insert4(kv, "f"),
        |kv| remove4(kv, "a"),
        &|kv| assert_consistent(kv, &set_of(&["b", "c", "d", "e", "f"])),
    );
}

/// Two preemptions deep: a third operation injected at every call boundary of
/// the second, itself injected at every call boundary of the first.
#[test]
fn three_way_drop_insert_insert_under_every_two_level_schedule() {
    let setup = |kv: &MemKv| {
        two_full_chunks(kv, true);
        for id in ["m", "n", "o"] {
            remove4(kv, id);
        }
    };
    let second_len = {
        let kv = MemKv::new();
        setup(&kv);
        kv.reset_calls();
        insert4(&kv, "x");
        kv.calls()
    };
    let first_len = {
        let kv = MemKv::new();
        setup(&kv);
        kv.reset_calls();
        remove4(&kv, "p");
        kv.calls()
    };
    for k in 1..=first_len + 1 {
        for j in 1..=second_len + 1 {
            let kv = MemKv::new();
            setup(&kv);
            kv.at(k, move |kv| {
                kv.at(j, |kv| insert4(kv, "y"));
                insert4(kv, "x");
            });
            remove4(&kv, "p");
            while let Some(f) = kv.take_hook() {
                f(&kv);
            }
            assert_consistent(&kv, &set_of(&["a", "b", "c", "d", "x", "y"]));
        }
    }
}

/// An id one chunk early (a writer routed on a manifest read just before a new
/// chunk was named) is still read, paged, and removable.
#[test]
fn an_id_one_chunk_early_is_still_listed_and_removable() {
    let kv = MemKv::new();
    kv.put_json(&chunk_key(B, 0), &ids(&["a", "b", "z"]));
    kv.put_json(&chunk_key(B, 1), &ids(&["m", "n"]));
    let (r0, r1) = (kv.raw(&chunk_key(B, 0)).unwrap().0, kv.raw(&chunk_key(B, 1)).unwrap().0);
    kv.put_json(
        B,
        &Manifest {
            chunks: vec![
                ChunkMeta { seq: 0, first: "a".into(), count: 3, rev: r0 },
                ChunkMeta { seq: 1, first: "m".into(), count: 2, rev: r1 },
            ],
            next_seq: 2,
        },
    );
    assert_eq!(read_all(&kv, B).unwrap(), ids(&["a", "b", "m", "n", "z"]));
    assert_eq!(page(&kv, B, "n", 10).unwrap(), (ids(&["z"]), false));
    remove(&kv, B, "z").unwrap();
    assert_consistent(&kv, &set_of(&["a", "b", "m", "n"]));
}

#[test]
fn old_manifests_are_read_as_is_and_migrated_on_the_next_write() {
    let kv = MemKv::new();
    two_full_chunks(&kv, false);
    // A pre-`rev` manifest reads fine...
    assert_eq!(count(&kv, B).unwrap(), 8);
    // ...and a heal (what `repair` runs) fills in the revisions.
    heal(&kv, B).unwrap();
    assert_consistent(&kv, &set_of(&["a", "b", "c", "d", "m", "n", "o", "p"]));
    // The new fields are optional in both directions.
    let old: serde_json::Value = serde_json::json!({"chunks":[{"seq":3,"first":"a","count":1}]});
    let m: Manifest = serde_json::from_value(old).unwrap();
    assert_eq!((m.chunks[0].rev, m.next_seq), (0, 0));
}

#[test]
fn chunk_keys_are_recognised_and_base_keys_are_not() {
    assert!(is_chunk_key(&chunk_key("ix_t_state_22p_22", 7)));
    assert!(is_chunk_key(&chunk_key("idx_t", 12345678)));
    assert!(!is_chunk_key("ix_t_state_22p_22"));
    assert!(!is_chunk_key("ix_t_c0x_12345678")); // `_c0` inside a base is not a suffix
    assert!(!is_chunk_key("ix_t_f_c1234567"));
}

// ---- stress ------------------------------------------------------------------

/// Real threads, a store that yields on every call, a chunk cap of 3 so chunks
/// are created and dropped constantly, and ids in random order so inserts land
/// in the middle as well as at the end. Each thread inserts its own ids and
/// removes some of them again (the same id is never raced — that has no defined
/// winner), so the final list is known exactly.
#[test]
fn stress_many_writers_one_list() {
    let kv = MemKv { yield_each: true, ..MemKv::default() };
    let threads = 8;
    let per = 60;
    let wanted = Mutex::new(BTreeSet::new());
    std::thread::scope(|s| {
        for t in 0..threads {
            let (kv, wanted) = (&kv, &wanted);
            s.spawn(move || {
                let mut x: u64 = 0x9E37_79B9_7F4A_7C15 ^ (t as u64 + 1);
                let mut mine = Vec::new();
                for i in 0..per {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    let id = format!("{:04x}-{t}-{i}", x % 0xFFFF);
                    insert_capped(kv, B, &id, 3).unwrap();
                    mine.push(id);
                    if x.is_multiple_of(3) {
                        let gone = mine.remove((x as usize / 3) % mine.len());
                        remove_capped(kv, B, &gone, 3).unwrap();
                    }
                }
                wanted.lock().unwrap().extend(mine);
            });
        }
    });
    // No `heal` first: a quiet list must ALREADY be consistent.
    assert_consistent(&kv, &wanted.into_inner().unwrap());
}

/// All writers appending to the tail at once — the id index's shape, and the
/// treasury gate's: every create lands in the last chunk.
#[test]
fn stress_many_writers_appending() {
    let kv = MemKv { yield_each: true, ..MemKv::default() };
    let next = std::sync::atomic::AtomicU64::new(0);
    std::thread::scope(|s| {
        for _ in 0..12 {
            s.spawn(|| {
                for _ in 0..40 {
                    let n = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    insert_capped(&kv, B, &format!("{n:08}"), 16).unwrap();
                }
            });
        }
    });
    let want: BTreeSet<String> = (0..480).map(|n| format!("{n:08}")).collect();
    assert_consistent(&kv, &want);
}
