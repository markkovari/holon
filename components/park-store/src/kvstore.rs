//! `holon_park::kvstore::Kv` over this app's keyvalue, moved by `comp:store/cas` (ADR-0132). Every
//! call is one host call and finishes at once, so the engine's futures are driven by [`block_on`].

use std::future::Future;

use holon_park::error::{ParkError, Result};
use holon_park::kvstore::{Kv, KvParkStore};
use holon_park::store::Revision;

use crate::bindings::comp::store::cas;
use crate::bindings::wasi::keyvalue::store;

pub type KvStore = KvParkStore<AppKv>;

pub fn kv_store() -> KvStore {
    KvParkStore(AppKv)
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

fn storage(e: impl std::fmt::Debug) -> ParkError {
    ParkError::storage(format!("{e:?}"))
}

fn bucket() -> Result<store::Bucket> {
    store::open("default").map_err(storage)
}

pub struct AppKv;

impl Kv for AppKv {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, Revision)>> {
        Ok(cas::get(&bucket()?, key).map_err(storage)?.map(|v| (v.value, v.revision)))
    }

    fn set(&self, key: &str, value: &[u8], expected: Revision) -> Result<bool> {
        Ok(matches!(
            cas::set(&bucket()?, key, value, expected).map_err(storage)?,
            cas::Outcome::Committed(_)
        ))
    }

    fn keys(&self) -> Result<Vec<String>> {
        let b = bucket()?;
        let mut out = Vec::new();
        let mut cursor = None;
        loop {
            let page = b.list_keys(cursor).map_err(storage)?;
            out.extend(page.keys);
            match page.cursor {
                Some(c) => cursor = Some(c),
                None => return Ok(out),
            }
        }
    }
}
