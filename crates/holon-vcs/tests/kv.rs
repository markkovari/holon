//! Every scenario against the stores a component keeps in one keyvalue bucket: blobs, pointers,
//! graph and oplog all in the same [`MemKv`], moved only by compare-and-set.

mod common;

use std::sync::Arc;

use holon_vcs::kvgraph::KvGraph;
use holon_vcs::mem::MemKv;
use holon_vcs::oplog::KvOpLog;
use holon_vcs::store::{KvBlobs, KvPointers};

type Bucket = Arc<MemKv>;
type KvStores =
    common::Stores<KvBlobs<Bucket>, KvPointers<Bucket>, KvGraph<Bucket>, KvOpLog<Bucket>>;

async fn make(name: &str) -> Option<(KvStores, String)> {
    let kv = Arc::new(MemKv::new());
    let s = common::Stores {
        blobs: Arc::new(KvBlobs::new(kv.clone())),
        pointers: Arc::new(KvPointers::new(kv.clone())),
        graph: Arc::new(KvGraph::new(kv.clone())),
        log: Arc::new(KvOpLog::new(kv)),
    };
    Some((s, format!("goal/{name}.1")))
}

scenario_tests!(make);

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn races_repeated() {
    for i in 0..10 {
        let (s, _) = make("x").await.unwrap();
        common::scenario_b_n_way(s.clone(), &format!("rep-{i}"), 8).await;
        common::rename_race(s.clone(), &format!("ren-{i}"), 8).await;
        common::concurrent_inserts(s, &format!("ins-{i}"), 8).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graph_rebuilt_from_the_log() {
    let (s, ws) = make("rebuild").await.unwrap();
    common::graph_rebuild(s, Arc::new(KvGraph::new(Arc::new(MemKv::new()))), &ws).await;
}
