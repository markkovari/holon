//! Every scenario against the in-memory backends. Always runs.

mod common;

use std::sync::Arc;

use holon_vcs::mem::{self, MemBlobs, MemGraph, MemOpLog, MemPointers};

type MemStores = common::Stores<MemBlobs, MemPointers, MemGraph, MemOpLog>;

async fn make(name: &str) -> Option<(MemStores, String)> {
    let s = common::Stores {
        blobs: Arc::new(MemBlobs::new()),
        pointers: Arc::new(mem::pointers()),
        graph: Arc::new(MemGraph::new()),
        log: Arc::new(mem::oplog()),
    };
    // A workspace id that needs escaping, so every run exercises it.
    Some((s, format!("goal/{name}.1")))
}

scenario_tests!(make);

/// The N-way race, many times, since a race test that passes once proves little.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn scenario_b_race_repeated() {
    for i in 0..25 {
        let (s, _) = make("x").await.unwrap();
        common::scenario_b_n_way(s, &format!("rep-{i}"), 8).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn scenario_a_repeated() {
    for i in 0..25 {
        let (s, _) = make("x").await.unwrap();
        common::scenario_a(s, &format!("rep-{i}")).await;
    }
}
