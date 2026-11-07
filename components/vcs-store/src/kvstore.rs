//! `holon_vcs::store::Kv` over this app's keyvalue, moved by `comp:store/cas` (ADR-0134). Every call
//! is one host call and finishes at once, so the engine's futures are driven by [`block_on`].

use std::future::Future;

use holon_vcs::error::{Result, VcsError};
use holon_vcs::kvgraph::KvGraph;
use holon_vcs::oplog::KvOpLog;
use holon_vcs::store::{CasMismatch, Kv, KvBlobs, KvPointers, Revision};
use holon_vcs::Engine;

use crate::bindings::comp::store::cas;
use crate::bindings::wasi::keyvalue::store;

pub type Store = Engine<KvBlobs<Bucket>, KvPointers<Bucket>, KvGraph<Bucket>, KvOpLog<Bucket>>;

pub fn engine() -> Store {
    Engine::new(
        KvBlobs::new(Bucket),
        KvPointers::new(Bucket),
        KvGraph::new(Bucket),
        KvOpLog::new(Bucket),
    )
}

/// The engine's futures never wait on anything: each step is a host call that has returned.
pub fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

fn storage(e: impl std::fmt::Debug) -> VcsError {
    VcsError::Storage(format!("{e:?}"))
}

fn bucket() -> Result<store::Bucket> {
    store::open("default").map_err(storage)
}

#[derive(Clone, Copy)]
pub struct Bucket;

impl Kv for Bucket {
    async fn get(&self, key: &str) -> Result<Option<(Vec<u8>, Revision)>> {
        Ok(cas::get(&bucket()?, key).map_err(storage)?.map(|v| (v.value, v.revision)))
    }

    async fn cas(
        &self,
        key: &str,
        expected: Option<Revision>,
        value: Vec<u8>,
    ) -> Result<std::result::Result<Revision, CasMismatch>> {
        Ok(match cas::set(&bucket()?, key, &value, expected.unwrap_or(0)).map_err(storage)? {
            cas::Outcome::Committed(r) => Ok(r),
            cas::Outcome::Conflict(0) => Err(CasMismatch { current: None }),
            cas::Outcome::Conflict(r) => Err(CasMismatch { current: Some(r) }),
        })
    }
}
