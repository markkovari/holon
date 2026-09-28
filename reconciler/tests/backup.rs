//! `comp-backup` end to end: real NATS, real SurrealDB, real S3 (RustFS), the
//! real binary — backed up from one set of servers, restored into a fresh one.
//!
//! The claim is "what comes back is what went in", so the state written here
//! is the awkward kind, not a single key: KV history and a delete marker, a
//! value written by a guarded update (its stored message carries a
//! `Nats-Expected-*` header that must not be replayed), a multi-chunk object,
//! custom headers, a work-queue stream (which refuses the dumper's usual
//! consumer), and a SurrealDB database. Then the ways a restore must refuse: a
//! wrong key, a tampered part, an existing stream, an unsealed run nobody asked
//! for.

mod harness;

use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use async_nats::jetstream::{self, kv, object_store, stream};
use comp_reconciler::fleet::free_port;
use futures::StreamExt;
use harness::{RustFs, Surreal, SURREAL_PASSWORD};
use tokio::io::AsyncReadExt;

const BUCKET: &str = "holon-backups";

struct Nats {
    child: Child,
    url: String,
    _dir: tempfile::TempDir,
}

impl Drop for Nats {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Nats {
    fn start() -> Option<Self> {
        let port = free_port();
        let dir = tempfile::tempdir().ok()?;
        let child = Command::new("nats-server")
            .args(["-js", "-a", "127.0.0.1", "-p", &port.to_string(), "-sd"])
            .arg(dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let me = Self { child, url: format!("nats://127.0.0.1:{port}"), _dir: dir };
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return Some(me);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    async fn js(&self) -> jetstream::Context {
        jetstream::new(async_nats::connect(&self.url).await.unwrap())
    }
}

fn backup(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_comp-backup")).args(args).output().unwrap()
}

fn ok(out: Output) -> String {
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "comp-backup failed:\n{err}");
    String::from_utf8_lossy(&out.stdout).to_string() + &err
}

fn refused(out: Output) -> String {
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "comp-backup should have refused, and did not:\n{err}");
    err
}

async fn sql(port: u16, q: &str) -> serde_json::Value {
    reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/sql"))
        .basic_auth("root", Some(SURREAL_PASSWORD))
        .header("accept", "application/json")
        .header("surreal-ns", "holon")
        .header("surreal-db", "graph")
        .body(q.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// Everything the test writes, so the same function can check a restore.
async fn populate(js: &jetstream::Context) -> Vec<u8> {
    let kv = js
        .create_key_value(kv::Config { bucket: "b-acme".into(), history: 2, ..Default::default() })
        .await
        .unwrap();
    // Three writes into a history of two: the first is dropped, so the stream
    // has a gap and a restore renumbers what follows it.
    kv.put("a", "1".into()).await.unwrap();
    kv.put("a", "2".into()).await.unwrap();
    let rev = kv.put("a", "2b".into()).await.unwrap();
    // A guarded write: its stored message carries
    // Nats-Expected-Last-Subject-Sequence = the ORIGINAL sequence, which after
    // the renumbering is not the restored one — replayed, it would be refused.
    kv.update("a", "3".into(), rev).await.unwrap();
    kv.put("b", "x".into()).await.unwrap();
    kv.put("c", "gone".into()).await.unwrap();
    kv.delete("c").await.unwrap();

    let objects = js
        .create_object_store(object_store::Config {
            bucket: "vcs-blobs".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let big: Vec<u8> = (0..(3 << 20)).map(|i| (i % 251) as u8).collect();
    objects.put("big", &mut &big[..]).await.unwrap();

    js.create_stream(stream::Config {
        name: "EVENTS".into(),
        subjects: vec!["events.>".into()],
        ..Default::default()
    })
    .await
    .unwrap();
    for i in 0..3 {
        let mut h = async_nats::HeaderMap::new();
        h.insert("X-Kind", format!("kind-{i}").as_str());
        h.insert("Nats-Msg-Id", format!("ev-{i}").as_str());
        js.publish_with_headers(format!("events.{i}"), h, format!("event {i}").into())
            .await
            .unwrap()
            .await
            .unwrap();
    }

    let jobs = js
        .create_stream(stream::Config {
            name: "JOBS".into(),
            subjects: vec!["jobs.>".into()],
            retention: stream::RetentionPolicy::WorkQueue,
            ..Default::default()
        })
        .await
        .unwrap();
    // A consumer already on the queue: an unfiltered ordered one is now refused.
    jobs.create_consumer(jetstream::consumer::pull::Config {
        durable_name: Some("worker".into()),
        ..Default::default()
    })
    .await
    .unwrap();
    for i in 0..3 {
        js.publish(format!("jobs.{i}"), format!("job {i}").into()).await.unwrap().await.unwrap();
    }
    big
}

async fn check(js: &jetstream::Context, big: &[u8]) {
    let kv = js.get_key_value("b-acme").await.expect("the KV bucket came back");
    assert_eq!(kv.get("a").await.unwrap().as_deref(), Some(&b"3"[..]));
    assert_eq!(kv.get("b").await.unwrap().as_deref(), Some(&b"x"[..]));
    assert_eq!(kv.get("c").await.unwrap(), None, "a deleted key came back to life");
    let history: Vec<_> = kv.history("a").await.unwrap().collect().await;
    assert_eq!(history.len(), 2, "history of a: {history:?}");

    let objects = js.get_object_store("vcs-blobs").await.expect("the object store came back");
    let mut back = Vec::new();
    objects.get("big").await.unwrap().read_to_end(&mut back).await.unwrap();
    assert_eq!(back.len(), big.len());
    assert!(back == big, "the 3 MB object differs");

    let mut events = js.get_stream("EVENTS").await.unwrap();
    assert_eq!(events.info().await.unwrap().state.messages, 3);
    let m = events.get_raw_message(2).await.unwrap();
    assert_eq!(m.headers.get("X-Kind").map(|v| v.to_string()), Some("kind-1".into()));
    assert_eq!(&m.payload[..], b"event 1");

    let mut jobs = js.get_stream("JOBS").await.unwrap();
    assert_eq!(jobs.info().await.unwrap().state.messages, 3);
}

#[test]
fn a_backup_restores_into_fresh_servers_and_refuses_what_it_should() {
    // A plain test with a runtime of its own: the harness's S3 client is
    // blocking, and a blocking client inside an async test panics on drop.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let Some(s3) = RustFs::start() else {
        eprintln!("SKIPPED: docker could not start {}", harness::RUSTFS_IMAGE);
        return;
    };
    let (Some(src), Some(dst)) = (Nats::start(), Nats::start()) else {
        eprintln!("SKIPPED: no nats-server on PATH");
        return;
    };
    let (Some(db_src), Some(db_dst)) = (Surreal::start(), Surreal::start()) else {
        eprintln!("SKIPPED: docker could not start {}", harness::SURREAL_IMAGE);
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (s3_key, s3_secret) = s3.credential_files(dir.path());
    let key_file = dir.path().join("backup.key");
    std::fs::write(&key_file, ok(backup(&["keygen"])).lines().next().unwrap()).unwrap();
    let other_key = dir.path().join("other.key");
    std::fs::write(&other_key, ok(backup(&["keygen"])).lines().next().unwrap()).unwrap();
    let pass_file = dir.path().join("surreal.pass");
    std::fs::write(&pass_file, SURREAL_PASSWORD).unwrap();
    let p = |x: &Path| x.display().to_string();

    let endpoint = s3.endpoint();
    let s3_args: Vec<String> = [
        "--s3-endpoint",
        &endpoint,
        "--s3-bucket",
        BUCKET,
        "--s3-path-style",
        "--s3-access-key-file",
        &s3_key,
        "--s3-secret-key-file",
        &s3_secret,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let with = |head: &[&str], tail: &[String]| -> Vec<String> {
        head.iter()
            .map(|s| s.to_string())
            .chain(s3_args.iter().cloned())
            .chain(tail.iter().cloned())
            .collect()
    };
    let run = |args: Vec<String>| backup(&args.iter().map(String::as_str).collect::<Vec<_>>());

    // --- the state -----------------------------------------------------------
    let big = rt.block_on(async { populate(&src.js().await).await });
    rt.block_on(async {
        sql(db_src.port, "DEFINE NAMESPACE holon; USE NS holon; DEFINE DATABASE graph;").await;
        sql(db_src.port, "CREATE symbol:one SET name = 'compute_total'; CREATE symbol:two SET name = 'validate';")
            .await;
    });

    // --- refusing to write plaintext nobody asked for ------------------------
    let err = refused(run(with(&["run", "--nats-url", &src.url], &[])));
    assert!(err.contains("--plaintext"), "{err}");

    // --- the backup ----------------------------------------------------------
    let surreal_url = format!("http://127.0.0.1:{}", db_src.port);
    let log = ok(run(with(
        &["run", "--nats-url", &src.url, "--encrypt-key-file", &p(&key_file)],
        &[
            "--surreal-url".into(),
            surreal_url,
            "--surreal-db".into(),
            "holon/graph".into(),
            "--surreal-pass-file".into(),
            p(&pass_file),
        ],
    )));
    assert!(log.contains("reading by sequence"), "the work queue took the fallback path: {log}");
    let keys = s3.list(BUCKET, "backups/");
    let manifest_key =
        keys.iter().map(|(k, _)| k).find(|k| k.ends_with("/manifest.json")).expect("a manifest");
    let manifest: serde_json::Value =
        serde_json::from_slice(&s3.get(BUCKET, manifest_key).unwrap()).unwrap();
    assert_eq!(manifest["sealed"], true);
    let parts: Vec<&str> =
        manifest["parts"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    for want in ["KV_b-acme", "OBJ_vcs-blobs", "EVENTS", "JOBS", "holon/graph"] {
        assert!(parts.contains(&want), "{want} missing from {parts:?}");
    }
    // Every part in the bucket is sealed, not merely compressed.
    for (k, _) in keys.iter().filter(|(k, _)| !k.ends_with("manifest.json")) {
        assert_eq!(&s3.get(BUCKET, k).unwrap()[..8], b"HOLONBK1", "{k} is not sealed");
    }

    // --- restored into servers that have never seen any of it ----------------
    let dst_surreal = format!("http://127.0.0.1:{}", db_dst.port);
    let surreal_dst_args: Vec<String> =
        vec!["--surreal-url".into(), dst_surreal, "--surreal-pass-file".into(), p(&pass_file)];
    rt.block_on(sql(db_dst.port, "DEFINE NAMESPACE holon; USE NS holon; DEFINE DATABASE graph;"));
    ok(run(with(
        &["restore", "--nats-url", &dst.url, "--encrypt-key-file", &p(&key_file)],
        &surreal_dst_args,
    )));
    rt.block_on(async { check(&dst.js().await, &big).await });
    let rows = rt.block_on(sql(db_dst.port, "SELECT name FROM symbol ORDER BY name;"));
    assert_eq!(rows[0]["result"].as_array().map(|r| r.len()), Some(2), "surreal rows: {rows}");

    // --- the refusals ---------------------------------------------------------
    // A restore does not merge into what is there.
    let err = refused(run(with(
        &[
            "restore",
            "--nats-url",
            &dst.url,
            "--encrypt-key-file",
            &p(&key_file),
            "--only",
            "EVENTS",
        ],
        &[],
    )));
    assert!(err.contains("already exists"), "{err}");
    ok(run(with(
        &[
            "restore",
            "--nats-url",
            &dst.url,
            "--encrypt-key-file",
            &p(&key_file),
            "--only",
            "EVENTS",
            "--replace",
        ],
        &[],
    )));
    // The wrong key is caught from the manifest, before a byte is downloaded.
    let err = refused(run(with(
        &["restore", "--nats-url", &dst.url, "--encrypt-key-file", &p(&other_key)],
        &[],
    )));
    assert!(err.contains("was sealed with key"), "{err}");
    // A part the bucket altered is refused whole.
    let events_key = keys.iter().map(|(k, _)| k).find(|k| k.contains("EVENTS")).unwrap();
    let mut tampered = s3.get(BUCKET, events_key).unwrap();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    s3.put(BUCKET, events_key, tampered);
    let err = refused(run(with(
        &[
            "restore",
            "--nats-url",
            &dst.url,
            "--encrypt-key-file",
            &p(&key_file),
            "--only",
            "EVENTS",
            "--replace",
        ],
        &[],
    )));
    assert!(err.contains("does not match its manifest"), "{err}");

    // --- retention -------------------------------------------------------------
    for _ in 0..2 {
        ok(run(with(
            &[
                "run",
                "--nats-url",
                &src.url,
                "--encrypt-key-file",
                &p(&key_file),
                "--keep",
                "2",
                "--stream",
                "KV_*",
            ],
            &[],
        )));
    }
    let listing = ok(run(with(&["list"], &[])));
    let complete = listing.lines().filter(|l| l.contains("sealed:")).count();
    assert_eq!(complete, 2, "--keep 2 keeps two:\n{listing}");
}
