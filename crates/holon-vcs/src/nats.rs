//! NATS JetStream adapters (`native` feature): blobs in an ObjectStore, pointers
//! and the oplog in KV.
//!
//! * [`NatsBlobs`] — object name = the blob's SHA-256 hex. `put` checks `info`
//!   first and skips the upload when the object exists; two racing puts of the same
//!   bytes both write the same content, so either winning is the same state.
//! * [`NatsKv`] — the [`Kv`] both [`KvPointers`] and [`KvOpLog`] run on.
//!   `cas(expected: None)` is `create` (fails if the key has a live value);
//!   `cas(expected: Some(r))` is `update(key, value, r)`, which JetStream rejects
//!   unless the key's last revision is exactly `r`. A rejection
//!   (`AlreadyExists` / `WrongLastRevision`) is a [`CasMismatch`]; anything else is
//!   a storage error. Nothing here deletes or purges a key (see [`crate::store`]).
//!
//! # Keys
//!
//! The engine's keys are already in JetStream's key charset (`[-/_=.A-Za-z0-9]`):
//! every user-supplied segment goes through [`crate::store::escape`], which maps
//! everything outside `[A-Za-z0-9_-]` — including `.`, the subject token
//! separator — to `=XX`. A key longer than [`MAX_KEY`] bytes (a very long
//! workspace id) is replaced by `h/<sha256 of the key>`: collision-free in
//! practice, and still deterministic, which is all a CAS key needs.

use async_nats::jetstream::{self, kv, object_store};
use tokio::io::AsyncReadExt;

use crate::error::{Result, VcsError};
use crate::model::Hash;
use crate::oplog::KvOpLog;
use crate::store::{sha256_hex, BlobStore, CasMismatch, Kv, KvPointers, Revision};

/// Longest key passed to JetStream verbatim.
pub const MAX_KEY: usize = 256;

/// Bucket names.
#[derive(Debug, Clone)]
pub struct NatsConfig {
    pub blob_bucket: String,
    pub pointer_bucket: String,
    pub oplog_bucket: String,
}

impl Default for NatsConfig {
    fn default() -> Self {
        NatsConfig {
            blob_bucket: "holon-vcs-blobs".into(),
            pointer_bucket: "holon-vcs-pointers".into(),
            oplog_bucket: "holon-vcs-oplog".into(),
        }
    }
}

impl NatsConfig {
    /// The default names with a prefix, for isolating tests or tenants.
    pub fn prefixed(prefix: &str) -> Self {
        NatsConfig {
            blob_bucket: format!("{prefix}-blobs"),
            pointer_bucket: format!("{prefix}-pointers"),
            oplog_bucket: format!("{prefix}-oplog"),
        }
    }
}

/// Connect and open (creating if needed) the three buckets.
pub async fn connect(
    url: &str,
    cfg: &NatsConfig,
) -> Result<(NatsBlobs, KvPointers<NatsKv>, KvOpLog<NatsKv>)> {
    let client = async_nats::connect(url).await.map_err(VcsError::storage)?;
    let js = jetstream::new(client);
    let blobs = NatsBlobs::open(&js, &cfg.blob_bucket).await?;
    let pointers = KvPointers::new(NatsKv::open(&js, &cfg.pointer_bucket).await?);
    let log = KvOpLog::new(NatsKv::open(&js, &cfg.oplog_bucket).await?);
    Ok((blobs, pointers, log))
}

pub struct NatsBlobs {
    store: object_store::ObjectStore,
    /// The bucket's backing stream, for direct chunk reads (see `get`).
    stream: jetstream::stream::Stream,
    bucket: String,
}

impl NatsBlobs {
    pub async fn open(js: &jetstream::Context, bucket: &str) -> Result<Self> {
        let store = match js.get_object_store(bucket).await {
            Ok(store) => store,
            Err(_) => {
                let cfg = object_store::Config { bucket: bucket.to_string(), ..Default::default() };
                match js.create_object_store(cfg).await {
                    Ok(store) => store,
                    // Somebody else created it in between.
                    Err(e) => {
                        js.get_object_store(bucket).await.map_err(|_| VcsError::storage(e))?
                    }
                }
            }
        };
        let stream = js.get_stream(format!("OBJ_{bucket}")).await.map_err(VcsError::storage)?;
        Ok(NatsBlobs { store, stream, bucket: bucket.to_string() })
    }
}

impl BlobStore for NatsBlobs {
    async fn put(&self, bytes: Vec<u8>) -> Result<Hash> {
        let h = sha256_hex(&bytes);
        if self.contains(&h).await? {
            return Ok(h);
        }
        let mut reader = bytes.as_slice();
        self.store.put(h.as_str(), &mut reader).await.map_err(VcsError::storage)?;
        Ok(h)
    }

    /// An object of at most one chunk (128 KiB by default — nearly every source
    /// blob) is read with one direct get of its chunk subject. `ObjectStore::get`
    /// creates an ordered consumer per read, and at the read rate of an engine
    /// those pile up faster than their inactivity timeout reaps them: measured
    /// here, the live suite hit JetStream's "maximum consumers limit reached"
    /// within a few runs (and async-nats 0.50 panics on that inside the reader).
    /// Larger objects still stream through a consumer.
    async fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        let info = match self.store.info(hash).await {
            Ok(info) if !info.deleted => info,
            Ok(_) => return Ok(None),
            Err(e) if e.kind() == object_store::InfoErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(VcsError::storage(e)),
        };
        let buf = match info.chunks {
            0 => Vec::new(),
            1 => {
                let subject = format!("$O.{}.C.{}", self.bucket, info.nuid);
                let msg = self
                    .stream
                    .direct_get_last_for_subject(subject)
                    .await
                    .map_err(VcsError::storage)?;
                msg.payload.to_vec()
            }
            _ => {
                let mut obj = self.store.get(hash).await.map_err(VcsError::storage)?;
                let mut buf = Vec::with_capacity(info.size);
                obj.read_to_end(&mut buf).await.map_err(VcsError::storage)?;
                buf
            }
        };
        if sha256_hex(&buf) != hash {
            return Err(VcsError::Storage(format!("object {hash} does not hash to its name")));
        }
        Ok(Some(buf))
    }

    async fn contains(&self, hash: &str) -> Result<bool> {
        match self.store.info(hash).await {
            Ok(info) => Ok(!info.deleted),
            Err(e) if e.kind() == object_store::InfoErrorKind::NotFound => Ok(false),
            Err(e) => Err(VcsError::storage(e)),
        }
    }
}

pub struct NatsKv {
    store: kv::Store,
}

impl NatsKv {
    pub async fn open(js: &jetstream::Context, bucket: &str) -> Result<Self> {
        if let Ok(store) = js.get_key_value(bucket).await {
            return Ok(NatsKv { store });
        }
        // history 1: a pointer's past is in the oplog, not in the bucket.
        let cfg = kv::Config { bucket: bucket.to_string(), history: 1, ..Default::default() };
        match js.create_key_value(cfg).await {
            Ok(store) => Ok(NatsKv { store }),
            Err(e) => js
                .get_key_value(bucket)
                .await
                .map(|store| NatsKv { store })
                .map_err(|_| VcsError::storage(e)),
        }
    }
}

impl NatsKv {
    /// Every key in the bucket with a live value — JetStream's own spelling,
    /// which is the engine key unless it was longer than [`MAX_KEY`] (then it
    /// is `h/<sha256>` and the original is not recoverable). An admin read: a
    /// scan of the bucket, not for a hot path.
    pub async fn keys(&self) -> Result<Vec<String>> {
        use futures::TryStreamExt;
        let keys = self.store.keys().await.map_err(VcsError::storage)?;
        keys.try_collect().await.map_err(VcsError::storage)
    }
}

/// Every workspace with an oplog in `log`'s bucket, sorted. A workspace whose
/// head key was too long to store verbatim is not listed: its key is hashed.
pub async fn workspaces(log: &KvOpLog<NatsKv>) -> Result<Vec<String>> {
    let mut out: Vec<String> = log
        .kv()
        .keys()
        .await?
        .iter()
        .filter_map(|k| crate::oplog::workspace_of_head_key(k))
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

/// The JetStream key for an engine key (see the module docs).
pub fn kv_key(key: &str) -> Result<String> {
    let ok = !key.is_empty()
        && !key.starts_with('.')
        && !key.ends_with('.')
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b"-/_=.".contains(&b));
    if !ok {
        return Err(VcsError::Invalid(format!("{key:?} is not a JetStream KV key")));
    }
    if key.len() > MAX_KEY {
        return Ok(format!("h/{}", sha256_hex(key.as_bytes())));
    }
    Ok(key.to_string())
}

impl Kv for NatsKv {
    async fn get(&self, key: &str) -> Result<Option<(Vec<u8>, Revision)>> {
        let k = kv_key(key)?;
        match self.store.entry(k).await.map_err(VcsError::storage)? {
            Some(e) if e.operation == kv::Operation::Put => {
                Ok(Some((e.value.to_vec(), e.revision)))
            }
            _ => Ok(None),
        }
    }

    async fn cas(
        &self,
        key: &str,
        expected: Option<Revision>,
        value: Vec<u8>,
    ) -> Result<std::result::Result<Revision, CasMismatch>> {
        let k = kv_key(key)?;
        match expected {
            None => match self.store.create(&k, value.into()).await {
                Ok(r) => Ok(Ok(r)),
                Err(e) if e.kind() == kv::CreateErrorKind::AlreadyExists => {
                    Ok(Err(CasMismatch { current: None }))
                }
                Err(e) => Err(VcsError::storage(e)),
            },
            Some(rev) => match self.store.update(&k, value.into(), rev).await {
                Ok(r) => Ok(Ok(r)),
                Err(e) if e.kind() == kv::UpdateErrorKind::WrongLastRevision => {
                    Ok(Err(CasMismatch { current: None }))
                }
                Err(e) => Err(VcsError::storage(e)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(kv_key("ws/a=2Fb/sym/00").unwrap(), "ws/a=2Fb/sym/00");
        assert!(kv_key("a b").is_err());
        assert!(kv_key(".a").is_err());
        let long = "x".repeat(MAX_KEY + 1);
        let k = kv_key(&long).unwrap();
        assert!(k.starts_with("h/") && k.len() == 66);
    }
}
