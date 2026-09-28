//! The gate for `--blob s3`, kept out of `objkv.rs` for the same reason
//! `surrealkv_test.rs` is kept out of `kv.rs`: the file that implements a backend
//! must not be the file that judges it.
//!
//! Each test starts its OWN RustFS in Docker, on a free port, and removes it when
//! it finishes. Skipped, loudly, when Docker cannot start one.
//!
//! What is being claimed, and so what is tested: an S3 bucket behaves as a
//! `KvBackend` — revisions that move on every write, a compare-and-set that
//! exactly one of N racing writers wins, increments that lose nothing under
//! contention — and a second connection (another process, another node) sees
//! what the first wrote.

use std::process::{Command, Stdio};
use std::sync::Arc;

use crate::kv::{Cas, KvBackend};
use crate::objkv::{S3Config, S3Kv};
use crate::tenant::BucketId;

/// Pinned, matching `infra/compose.yaml`.
const RUSTFS_IMAGE: &str = "rustfs/rustfs:1.0.0";
const KEY: &str = "testadmin";
const SECRET: &str = "testadmin-secret";

struct RustFs {
    name: String,
    port: u16,
}

impl Drop for RustFs {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

impl RustFs {
    fn start() -> Option<Self> {
        let port = std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok()?.port();
        let name = format!("comp-host-test-rustfs-{port}");
        let status = Command::new("docker")
            .args(["run", "--rm", "-d", "--name", &name])
            .args(["-p", &format!("127.0.0.1:{port}:9000")])
            .args(["-e", "RUSTFS_VOLUMES=/data", "-e", "RUSTFS_ADDRESS=0.0.0.0:9000"])
            .args(["-e", &format!("RUSTFS_ACCESS_KEY={KEY}")])
            .args(["-e", &format!("RUSTFS_SECRET_KEY={SECRET}")])
            .arg(RUSTFS_IMAGE)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        let me = Self { name, port };
        // `/health` goes green before the S3 API stops answering 503, so the
        // readiness check is the thing under test doing its own first call.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            if S3Kv::connect(&me.config()).is_ok() {
                return Some(me);
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        None
    }

    fn config(&self) -> S3Config {
        S3Config {
            endpoint: format!("http://127.0.0.1:{}", self.port),
            bucket: "holon-test".into(),
            region: "us-east-1".into(),
            access_key: KEY.into(),
            secret_key: SECRET.into(),
            path_style: true,
        }
    }

    fn connect(&self) -> Arc<S3Kv> {
        Arc::new(S3Kv::connect(&self.config()).expect("connect, create the bucket, pass the probe"))
    }
}

macro_rules! store_or_skip {
    () => {
        match RustFs::start() {
            Some(s) => s,
            None => {
                eprintln!("SKIPPED: docker could not start {RUSTFS_IMAGE}");
                return;
            }
        }
    };
}

fn obj(tag: &str) -> BucketId {
    BucketId::object_for_test(&format!("o-{tag}"))
}

#[test]
fn every_operation_round_trips() {
    let fs = store_or_skip!();
    let kv = fs.connect();
    let b = obj("ops");
    assert_eq!(kv.get(&b, "missing").unwrap(), None);
    assert!(!kv.exists(&b, "missing").unwrap());

    // Keys shaped like blob-store's, plus bytes the escape has to carry.
    let keys = ["bo_c/photo.arw", "a=b", "sp ace", "ünï"];
    for (i, k) in keys.iter().enumerate() {
        kv.set(&b, k, format!("v{i}").as_bytes()).unwrap();
    }
    for (i, k) in keys.iter().enumerate() {
        assert_eq!(kv.get(&b, k).unwrap(), Some(format!("v{i}").into_bytes()), "{k}");
        assert!(kv.exists(&b, k).unwrap());
    }
    let mut listed = kv.list_keys(&b).unwrap();
    listed.sort();
    let mut want: Vec<String> = keys.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(listed, want, "list_keys hands back the keys as written");

    kv.delete(&b, "a=b").unwrap();
    kv.delete(&b, "a=b").unwrap(); // idempotent
    assert!(!kv.exists(&b, "a=b").unwrap());

    // A value bigger than NATS's 1 MB payload — the reason this backend exists.
    let big = vec![7u8; 3 << 20];
    kv.set(&b, "big", &big).unwrap();
    assert_eq!(kv.get(&b, "big").unwrap().as_deref(), Some(&big[..]));
}

#[test]
fn two_buckets_never_see_each_other() {
    let fs = store_or_skip!();
    let kv = fs.connect();
    let (a, e) = (obj("alice"), obj("eve"));
    kv.set(&a, "k", b"alice's").unwrap();
    assert_eq!(kv.get(&e, "k").unwrap(), None);
    assert!(kv.list_keys(&e).unwrap().is_empty());
}

#[test]
fn revisions_move_on_every_write_and_guard_a_replace() {
    let fs = store_or_skip!();
    let kv = fs.connect();
    let b = obj("rev");
    assert_eq!(kv.set_if_revision(&b, "k", b"one", 0).unwrap(), Cas::Committed(1));
    assert_eq!(kv.set_if_revision(&b, "k", b"again", 0).unwrap(), Cas::Conflict(1));
    kv.set(&b, "k", b"two").unwrap();
    assert_eq!(kv.get_revision(&b, "k").unwrap(), Some((2, b"two".to_vec())));
    assert_eq!(kv.set_if_revision(&b, "k", b"stale", 1).unwrap(), Cas::Conflict(2));
    assert_eq!(kv.set_if_revision(&b, "k", b"three", 2).unwrap(), Cas::Committed(3));
    assert_eq!(kv.get(&b, "k").unwrap().as_deref(), Some(&b"three"[..]));
}

#[test]
fn exactly_one_of_eight_racing_writers_wins() {
    let fs = store_or_skip!();
    let kv = fs.connect();
    let b = obj("race");
    kv.set(&b, "k", b"base").unwrap();
    let (rev, _) = kv.get_revision(&b, "k").unwrap().unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let (kv, b, barrier) = (kv.clone(), b.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                kv.set_if_revision(&b, "k", format!("writer-{i}").as_bytes(), rev).unwrap()
            })
        })
        .collect();
    let outcomes: Vec<Cas> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let won = outcomes.iter().filter(|c| matches!(c, Cas::Committed(_))).count();
    assert_eq!(won, 1, "one compare-and-set lands, the rest see a conflict: {outcomes:?}");
}

#[test]
fn increments_under_contention_lose_nothing() {
    let fs = store_or_skip!();
    let kv = fs.connect();
    let b = obj("count");
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let (kv, b) = (kv.clone(), b.clone());
            std::thread::spawn(move || {
                for _ in 0..10 {
                    kv.increment(&b, "n", 1).unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(kv.get(&b, "n").unwrap().as_deref(), Some(&b"60"[..]));
}

#[test]
fn a_second_connection_sees_the_first() {
    let fs = store_or_skip!();
    let b = obj("shared");
    fs.connect().set(&b, "k", b"from node one").unwrap();
    let other = fs.connect();
    assert!(other.shared());
    assert_eq!(other.get(&b, "k").unwrap().as_deref(), Some(&b"from node one"[..]));
}
