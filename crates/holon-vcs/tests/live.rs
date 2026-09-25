//! Every scenario against real backends: NATS JetStream (ObjectStore + KV) and
//! SurrealDB. Gated on the environment, and LOUD when it skips:
//!
//!   HOLON_VCS_NATS_URL=nats://127.0.0.1:4333 \
//!   HOLON_VCS_SURREAL_URL=127.0.0.1:8000 \
//!   cargo test --manifest-path crates/Cargo.toml --features holon-vcs/native --test live
//!
//! `nats-server -js -p 4333 -sd <tmpdir>` and
//! `docker compose -f infra/compose.yaml --profile graph up -d surreal` are enough.
//! Each test uses its own workspace id, so runs share buckets and a database
//! without seeing each other.
#![cfg(feature = "native")]

mod common;

use std::sync::Arc;

use holon_vcs::nats::{self, NatsBlobs, NatsConfig, NatsKv};
use holon_vcs::oplog::KvOpLog;
use holon_vcs::store::KvPointers;
use holon_vcs::surreal::{SurrealConfig, SurrealGraph};

type LiveStores = common::Stores<NatsBlobs, KvPointers<NatsKv>, SurrealGraph, KvOpLog<NatsKv>>;

async fn make(name: &str) -> Option<(LiveStores, String)> {
    let (Ok(nats_url), Ok(surreal_url)) =
        (std::env::var("HOLON_VCS_NATS_URL"), std::env::var("HOLON_VCS_SURREAL_URL"))
    else {
        eprintln!(
            "SKIPPED {name}: set HOLON_VCS_NATS_URL and HOLON_VCS_SURREAL_URL to run it against real NATS + SurrealDB"
        );
        return None;
    };
    let prefix =
        std::env::var("HOLON_VCS_BUCKET_PREFIX").unwrap_or_else(|_| "holon-vcs-test".into());
    let (blobs, pointers, log) =
        nats::connect(&nats_url, &NatsConfig::prefixed(&prefix)).await.expect("nats");
    let mut cfg = SurrealConfig::new(&surreal_url);
    cfg.namespace = "holon_vcs_test".into();
    cfg.database = "live".into();
    let graph = SurrealGraph::connect(&cfg).await.expect("surreal");
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let ws = format!("{name}/{}-{nanos}", std::process::id());
    Some((
        common::Stores {
            blobs: Arc::new(blobs),
            pointers: Arc::new(pointers),
            graph: Arc::new(graph),
            log: Arc::new(log),
        },
        ws,
    ))
}

scenario_tests!(make);

/// The races again, repeatedly, over the network — where interleavings are real.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn races_repeated() {
    for i in 0..10 {
        let Some((s, ws)) = make(&format!("race-rep-{i}")).await else { return };
        common::scenario_b_n_way(s.clone(), &ws, 8).await;
        common::scenario_a(s, &format!("{ws}-a")).await;
    }
}

/// A workspace id too long for a JetStream key verbatim goes through the hashed
/// key path and still works end to end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn long_workspace_id() {
    let Some((s, ws)) = make("long").await else { return };
    let ws = format!("{ws}/{}", "x.y".repeat(120));
    common::scenario_b(s, &ws).await;
}

/// Blobs of zero, one and several chunks round-trip (the direct-get path and the
/// streaming path), and `put` is idempotent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blob_sizes() {
    use holon_vcs::store::{sha256_hex, BlobStore};
    let Some((s, _)) = make("blobs").await else { return };
    for len in [0usize, 1, 128 * 1024, 128 * 1024 + 1, 700 * 1024] {
        let bytes: Vec<u8> = (0..len).map(|i| (i * 7 % 251) as u8).collect();
        let h = s.blobs.put(bytes.clone()).await.unwrap();
        assert_eq!(h, sha256_hex(&bytes));
        assert_eq!(s.blobs.put(bytes.clone()).await.unwrap(), h);
        assert_eq!(s.blobs.get(&h).await.unwrap().as_deref(), Some(bytes.as_slice()), "len {len}");
    }
    assert_eq!(s.blobs.get(&"0".repeat(64)).await.unwrap(), None);
}
