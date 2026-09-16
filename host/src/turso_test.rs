//! The gate for `--kv turso`, deliberately OUTSIDE the file that implements it —
//! same rule as `surrealkv_test.rs`: a test living beside the code it judges is
//! a test the code can rewrite.
//!
//! Each test starts its OWN self-hosted `libsql-server` in Docker (the same
//! Hrana protocol hosted Turso speaks) and tears it down when it finishes — no
//! Turso account needed to verify this backend works.

use std::process::{Command, Stdio};

use crate::kv::{self, Cas};
use crate::tenant::BucketId;

/// Not version-pinned like `SURREAL_IMAGE`: `tursodatabase/libsql-server` ships
/// no numbered tags as of writing, only `:latest` and `:main`.
const LIBSQL_IMAGE: &str = "ghcr.io/tursodatabase/libsql-server:latest";

/// A libsql-server container that dies with the test. Unauthenticated by
/// default (no `--auth-jwt-key` / token given), matching `kv::build`'s empty
/// token meaning "no `Authorization` header sent".
struct Libsql {
    name: String,
    port: u16,
}

impl Drop for Libsql {
    fn drop(&mut self) {
        let _ =
            Command::new("docker").args(["rm", "-f", &self.name]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

impl Libsql {
    fn start() -> Option<Self> {
        let port = std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok()?.port();
        let name = format!("comp-host-test-libsql-{port}");
        let status = Command::new("docker")
            .args(["run", "--rm", "-d", "--name", &name])
            .args(["-p", &format!("127.0.0.1:{port}:8080")])
            .arg(LIBSQL_IMAGE)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        let me = Self { name, port };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            let probe = ureq::post(&format!("http://127.0.0.1:{port}/v2/pipeline"))
                .send_string(r#"{"requests":[{"type":"close"}]}"#);
            if probe.is_ok() {
                return Some(me);
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        None
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// A bucket name nothing else uses, so a rerun never reads its own leftovers.
fn bucket(tag: &str) -> BucketId {
    BucketId::for_test(&format!("turso-{tag}"))
}

async fn backend(url: &str) -> std::sync::Arc<dyn kv::KvBackend> {
    kv::build("turso", "", "", url, 1, "").await.expect("`turso` must be a backend `kv::build` knows")
}

macro_rules! turso_test {
    ($name:ident, |$db:ident| $body:expr) => {
        #[tokio::test(flavor = "multi_thread")]
        async fn $name() {
            let Some($db) = Libsql::start() else {
                eprintln!(
                    "SKIPPED: could not start {LIBSQL_IMAGE} — this test needs Docker to run \
                     it in. Nothing about --kv turso was verified by this run."
                );
                return;
            };
            $body
        }
    };
}

turso_test!(the_backend_is_named_and_reachable, |db| {
    let b = backend(&db.url()).await;
    // Shared, and it must SAY so: an app placed on two nodes against one
    // libsql database sees one store, which is the entire reason to add this
    // backend rather than use sqlite.
    assert!(b.shared(), "a libsql database reachable over the network is a SHARED store");
});

turso_test!(a_value_round_trips_and_deletes, |db| {
    let b = backend(&db.url()).await;
    let k = bucket("roundtrip");
    b.set(&k, "greeting", b"hello").unwrap();
    assert_eq!(b.get(&k, "greeting").unwrap().as_deref(), Some(&b"hello"[..]));
    assert!(b.exists(&k, "greeting").unwrap());
    assert!(b.list_keys(&k).unwrap().contains(&"greeting".to_string()));
    b.delete(&k, "greeting").unwrap();
    assert_eq!(b.get(&k, "greeting").unwrap(), None, "a deleted key is gone, not empty");
    assert!(!b.exists(&k, "greeting").unwrap());
});

turso_test!(absent_keys_are_none_not_errors, |db| {
    let b = backend(&db.url()).await;
    let k = bucket("absent");
    assert_eq!(b.get(&k, "never-written").unwrap(), None);
    assert!(!b.exists(&k, "never-written").unwrap());
    b.delete(&k, "never-written").unwrap();
});

turso_test!(increment_starts_at_zero_and_accumulates, |db| {
    let b = backend(&db.url()).await;
    let k = bucket("counter");
    assert_eq!(b.increment(&k, "hits", 1).unwrap(), 1, "an absent counter starts at zero");
    assert_eq!(b.increment(&k, "hits", 4).unwrap(), 5);
    assert_eq!(b.get(&k, "hits").unwrap().as_deref(), Some(&b"5"[..]), "stored as a decimal string");
});

turso_test!(the_revision_moves_on_every_write_including_a_plain_set, |db| {
    let b = backend(&db.url()).await;
    let k = bucket("revision");
    assert_eq!(b.get_revision(&k, "doc").unwrap(), None);

    b.set(&k, "doc", b"v1").unwrap();
    let (r1, v1) = b.get_revision(&k, "doc").unwrap().expect("written, so present");
    assert_eq!(v1, b"v1");
    assert!(r1 > 0, "a written key is at some revision above zero");

    b.set(&k, "doc", b"v2").unwrap();
    let (r2, _) = b.get_revision(&k, "doc").unwrap().unwrap();
    assert!(r2 > r1, "a plain set must bump the revision too, or a guard is not a guard");
});

turso_test!(compare_and_set_commits_once_and_then_conflicts, |db| {
    let b = backend(&db.url()).await;
    let k = bucket("cas");

    let first = b.set_if_revision(&k, "row", b"one", 0).unwrap();
    let rev = match first {
        Cas::Committed(r) => r,
        Cas::Conflict(r) => panic!("a create against an absent key must commit, got conflict at {r}"),
    };

    match b.set_if_revision(&k, "row", b"two", 0).unwrap() {
        Cas::Conflict(seen) => assert_eq!(seen, rev, "a conflict reports the revision actually held"),
        Cas::Committed(_) => panic!("a stale guard must NOT commit — this is the lost update"),
    }
    assert_eq!(b.get(&k, "row").unwrap().as_deref(), Some(&b"one"[..]), "the refused write left no trace");

    match b.set_if_revision(&k, "row", b"two", rev).unwrap() {
        Cas::Committed(next) => assert!(next > rev),
        Cas::Conflict(r) => panic!("a current guard must commit, conflicted at {r}"),
    }
    assert_eq!(b.get(&k, "row").unwrap().as_deref(), Some(&b"two"[..]));
});

turso_test!(two_buckets_do_not_see_each_other, |db| {
    let b = backend(&db.url()).await;
    let (a, z) = (bucket("iso-a"), bucket("iso-z"));
    b.set(&a, "same-key", b"from-a").unwrap();
    b.set(&z, "same-key", b"from-z").unwrap();
    assert_eq!(b.get(&a, "same-key").unwrap().as_deref(), Some(&b"from-a"[..]));
    assert_eq!(b.get(&z, "same-key").unwrap().as_deref(), Some(&b"from-z"[..]));
    assert!(!b.list_keys(&a).unwrap().iter().any(|key| b.get(&z, key).unwrap().as_deref() == Some(&b"from-a"[..])));
});

turso_test!(a_second_handle_sees_the_first_handles_writes, |db| {
    let one = backend(&db.url()).await;
    let k = bucket("shared");
    one.set(&k, "written-by", b"handle-one").unwrap();
    let two = backend(&db.url()).await;
    assert_eq!(
        two.get(&k, "written-by").unwrap().as_deref(),
        Some(&b"handle-one"[..]),
        "a shared store is shared across handles, or it is not shared",
    );
});

turso_test!(an_unknown_backend_still_names_the_ones_that_exist, |db| {
    let err = match kv::build("postgres", "", "", &db.url(), 1, "").await {
        Err(e) => e.to_string(),
        Ok(_) => panic!("`postgres` is not a backend"),
    };
    assert!(err.contains("turso"), "the error must list turso as a choice, got: {err}");
});
