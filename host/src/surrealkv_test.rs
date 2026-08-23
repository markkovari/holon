//! The gate for `--kv surreal`, deliberately OUTSIDE the file that implements it.
//!
//! `kv.rs` is the only writable path in this goal, so these assertions cannot be
//! edited by whatever writes the backend — a test living beside the code it
//! judges is a test the code can rewrite, and a goal whose gate is writable is
//! not gated at all.
//!
//! It talks to a REAL SurrealDB on 127.0.0.1:8000 (root/root), because the whole
//! claim being tested is "this survives a restart and is visible to another
//! process", and an in-memory double proves neither.

use crate::kv::{self, Cas};
use crate::tenant::BucketId;

const URL: &str = "http://127.0.0.1:8000";

/// A bucket name nothing else uses, so a rerun never reads its own leftovers.
fn bucket(tag: &str) -> BucketId {
    BucketId::for_test(&format!("surrealkv-{tag}"))
}

async fn backend() -> std::sync::Arc<dyn kv::KvBackend> {
    kv::build("surreal", "", "", URL, 1)
        .await
        .expect("`surreal` must be a backend `kv::build` knows; is SurrealDB up on :8000?")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_backend_is_named_and_reachable() {
    let b = backend().await;
    // Shared, and it must SAY so: an app placed on two nodes against one
    // SurrealDB sees one store, which is the entire reason to add this backend
    // rather than use sqlite.
    assert!(b.shared(), "a SurrealDB reachable over the network is a SHARED store");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_value_round_trips_and_deletes() {
    let b = backend().await;
    let k = bucket("roundtrip");
    b.set(&k, "greeting", b"hello").unwrap();
    assert_eq!(b.get(&k, "greeting").unwrap().as_deref(), Some(&b"hello"[..]));
    assert!(b.exists(&k, "greeting").unwrap());
    assert!(b.list_keys(&k).unwrap().contains(&"greeting".to_string()));
    b.delete(&k, "greeting").unwrap();
    assert_eq!(b.get(&k, "greeting").unwrap(), None, "a deleted key is gone, not empty");
    assert!(!b.exists(&k, "greeting").unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn absent_keys_are_none_not_errors() {
    let b = backend().await;
    let k = bucket("absent");
    assert_eq!(b.get(&k, "never-written").unwrap(), None);
    assert!(!b.exists(&k, "never-written").unwrap());
    // Deleting what is not there is not an error: every other backend here is
    // idempotent about it, and a caller cannot tell the difference anyway.
    b.delete(&k, "never-written").unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn increment_starts_at_zero_and_accumulates() {
    let b = backend().await;
    let k = bucket("counter");
    assert_eq!(b.increment(&k, "hits", 1).unwrap(), 1, "an absent counter starts at zero");
    assert_eq!(b.increment(&k, "hits", 4).unwrap(), 5);
    assert_eq!(
        b.get(&k, "hits").unwrap().as_deref(),
        Some(&b"5"[..]),
        "stored as a decimal string"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_revision_moves_on_every_write_including_a_plain_set() {
    let b = backend().await;
    let k = bucket("revision");
    assert_eq!(b.get_revision(&k, "doc").unwrap(), None);

    b.set(&k, "doc", b"v1").unwrap();
    let (r1, v1) = b.get_revision(&k, "doc").unwrap().expect("written, so present");
    assert_eq!(v1, b"v1");
    assert!(r1 > 0, "a written key is at some revision above zero");

    // ADR-0065: a revision that only moved for GUARDED writes would let a plain
    // `set` slip past a guard silently. This is the assertion that says so.
    b.set(&k, "doc", b"v2").unwrap();
    let (r2, _) = b.get_revision(&k, "doc").unwrap().unwrap();
    assert!(r2 > r1, "a plain set must bump the revision too, or a guard is not a guard");
}

#[tokio::test(flavor = "multi_thread")]
async fn compare_and_set_commits_once_and_then_conflicts() {
    let b = backend().await;
    let k = bucket("cas");

    // expected == 0 means "must not exist yet", so create and update are one call.
    let first = b.set_if_revision(&k, "row", b"one", 0).unwrap();
    let rev = match first {
        Cas::Committed(r) => r,
        Cas::Conflict(r) => {
            panic!("a create against an absent key must commit, got conflict at {r}")
        }
    };

    // The same guard a second time is the lost update ADR-0065 measured.
    match b.set_if_revision(&k, "row", b"two", 0).unwrap() {
        Cas::Conflict(seen) => {
            assert_eq!(seen, rev, "a conflict reports the revision actually held")
        }
        Cas::Committed(_) => panic!("a stale guard must NOT commit — this is the lost update"),
    }
    assert_eq!(
        b.get(&k, "row").unwrap().as_deref(),
        Some(&b"one"[..]),
        "the refused write left no trace"
    );

    match b.set_if_revision(&k, "row", b"two", rev).unwrap() {
        Cas::Committed(next) => assert!(next > rev),
        Cas::Conflict(r) => panic!("a current guard must commit, conflicted at {r}"),
    }
    assert_eq!(b.get(&k, "row").unwrap().as_deref(), Some(&b"two"[..]));
}

#[tokio::test(flavor = "multi_thread")]
async fn two_buckets_do_not_see_each_other() {
    let b = backend().await;
    let (a, z) = (bucket("iso-a"), bucket("iso-z"));
    b.set(&a, "same-key", b"from-a").unwrap();
    b.set(&z, "same-key", b"from-z").unwrap();
    assert_eq!(b.get(&a, "same-key").unwrap().as_deref(), Some(&b"from-a"[..]));
    assert_eq!(b.get(&z, "same-key").unwrap().as_deref(), Some(&b"from-z"[..]));
    // Tenancy is the point: `BucketId` is already namespaced, and a backend that
    // flattened it would leak one tenant's data into another's reads.
    assert!(!b
        .list_keys(&a)
        .unwrap()
        .iter()
        .any(|key| b.get(&z, key).unwrap().as_deref() == Some(&b"from-a"[..])));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_handle_sees_the_first_handles_writes() {
    // The claim that separates this from sqlite. Two connections, one store.
    let one = backend().await;
    let k = bucket("shared");
    one.set(&k, "written-by", b"handle-one").unwrap();
    let two = backend().await;
    assert_eq!(
        two.get(&k, "written-by").unwrap().as_deref(),
        Some(&b"handle-one"[..]),
        "a shared store is shared across handles, or it is not shared",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_backend_still_names_the_ones_that_exist() {
    let err = match kv::build("postgres", "", "", URL, 1).await {
        Err(e) => e.to_string(),
        Ok(_) => panic!("`postgres` is not a backend"),
    };
    assert!(err.contains("surreal"), "the error must list surreal as a choice, got: {err}");
}
