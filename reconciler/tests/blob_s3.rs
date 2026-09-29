//! `--blob s3` on a real fleet: a git repository whose objects live in an S3
//! bucket while its refs stay on NATS.
//!
//! `host/src/objkv_test.rs` proves the backend is a correct `KvBackend`. What it
//! cannot reach is the wiring: that a deployed `blob-store` asks for its `blobs`
//! store, that the host routes exactly that store to S3, and that the bytes a
//! guest wrote come back out through a linked capability. So the ground truth is
//! the bucket itself, listed directly — not the component saying it stored
//! something.

mod harness;

use std::time::Duration;

use comp_reconciler::fleet::{repo_root, Fleet};
use harness::RustFs;
use serde_json::{json, Value};

const S3_BUCKET: &str = "holon-blobs";

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    [("gate", "vgit_probe.wasm"), ("vgit", "virt_git.wasm"), ("blobs", "blob_store.wasm")]
        .iter()
        .map(|(id, file)| {
            let p = dir.join(file);
            assert!(p.exists(), "missing {} — run `cargo xtask build --force`", p.display());
            format!("{id}={}", p.display())
        })
        .collect()
}

fn call(port: u16, method: reqwest::Method, path: &str, body: Value) -> Value {
    let http =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build().unwrap();
    let r = match http
        .request(method, format!("http://127.0.0.1:{port}{path}"))
        .header("host", "vgit.acme.test")
        .body(if body.is_null() { String::new() } else { body.to_string() })
        .send()
    {
        Ok(r) => r,
        Err(e) => return Value::String(format!("transport: {e}")),
    };
    let (status, text) = (r.status(), r.text().unwrap_or_default());
    serde_json::from_str(&text).unwrap_or_else(|_| Value::String(format!("HTTP {status}: {text}")))
}

#[test]
fn blob_bytes_land_in_s3_and_come_back_out() {
    let Some(s3) = RustFs::start() else {
        eprintln!("SKIPPED: docker could not start {}", harness::RUSTFS_IMAGE);
        return;
    };
    let creds_dir = tempfile::tempdir().unwrap();
    let (key_file, secret_file) = s3.credential_files(creds_dir.path());
    let host_args: Vec<String> = [
        "--blob",
        "s3",
        "--s3-endpoint",
        &s3.endpoint(),
        "--s3-bucket",
        S3_BUCKET,
        "--s3-path-style",
        "--s3-access-key-file",
        &key_file,
        "--s3-secret-key-file",
        &secret_file,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let fleet = Fleet::start_with_host_args(
        "vgits3",
        &["fixtures/virt-git.yaml"],
        &artifacts(),
        &host_args,
    );
    let port = fleet.ingress_port;
    fleet.until("reading a ref that does not exist", Duration::from_secs(120), || {
        let r = call(port, reqwest::Method::GET, "/ref?name=probe%2Fready", Value::Null);
        if r["found"] == json!(false) {
            Ok(())
        } else {
            Err(r.to_string())
        }
    });

    // Bigger than NATS's default 1 MB payload: on the KV path this is the object
    // that needs a raised `max_payload`; on S3 it is nothing special.
    let big = "x".repeat(2 << 20);
    let first = call(
        port,
        reqwest::Method::POST,
        "/commit",
        json!({ "base": "", "message": "first", "changes": [
            { "path": "src/lib.rs", "content": "fn main() {}\n", "remove": false },
            { "path": "assets/big.txt", "content": big, "remove": false },
        ]}),
    );
    let c1 = first["commit"].as_str().unwrap_or_default().to_string();
    assert_eq!(c1.len(), 40, "no commit came back: {first}");

    // Round trip through the linked capability and the object store.
    let r = call(
        port,
        reqwest::Method::GET,
        &format!("/read?commit={c1}&path=src%2Flib.rs"),
        Value::Null,
    );
    assert_eq!(r["content"], json!("fn main() {}\n"), "the file did not round-trip: {r}");
    let r = call(
        port,
        reqwest::Method::GET,
        &format!("/read?commit={c1}&path=assets%2Fbig.txt"),
        Value::Null,
    );
    assert_eq!(
        r["content"].as_str().map(str::len),
        Some(big.len()),
        "the 2 MB file did not round-trip"
    );

    // The ground truth: the bucket, listed directly.
    let objects = s3.list(S3_BUCKET, "");
    let data: Vec<_> = objects.iter().filter(|(k, _)| !k.starts_with(".probe/")).collect();
    assert!(!data.is_empty(), "nothing reached S3: {objects:?}");
    for (k, _) in &data {
        // `o-<env>/` is the host's object bucket and `bo_` is blob-store's data
        // key. An index key (`bm_`) or any record here would mean keyed state
        // leaked into the object store.
        let (bucket, key) = k.split_once('/').unwrap_or_else(|| panic!("unprefixed key {k}"));
        assert!(bucket.starts_with("o-"), "{k} is not in a host object bucket");
        assert!(key.starts_with("bo_"), "{k} is not a blob-store data key");
    }
    assert!(
        data.iter().any(|(_, size)| *size >= 2 << 20),
        "no object holds the 2 MB file's bytes: {data:?}"
    );
}
