//! Object bytes on an S3-compatible store (`--blob s3`): AWS S3, Cloudflare R2,
//! RustFS, MinIO — anything that speaks the S3 API AND enforces conditional
//! writes.
//!
//! Two pieces:
//!
//! - [`S3Kv`], a full [`KvBackend`] over one S3 bucket. Every host bucket is a key
//!   prefix inside it (`{bucket-id}/{key}`), so tenant separation is the same
//!   host-minted `BucketId` every other backend uses.
//! - [`RoutedKv`], which sends `StoreClass::Object` buckets to it and everything
//!   else to the KV backend `--kv` chose. Keyed state stays on a low-latency store
//!   (one S3 round trip is 20-100 ms over a WAN; ADR-0070 counted 85 store
//!   operations in one request); only the bytes `blob-store` keeps in its `blobs`
//!   bucket move.
//!
//! **Revisions.** Each object carries its revision in `x-amz-meta-rev`. Every
//! write — a plain `set` included, per the trait — is a conditional PUT against
//! the ETag it read: `If-None-Match: *` to create, `If-Match: <etag>` to replace.
//! A lost race is a `412`, so two writers can never both land the same revision,
//! and `set_if_revision` is a real compare-and-set rather than a read-compare-write
//! with a window in it. That depends entirely on the store honouring those
//! headers, which is why [`S3Kv::connect`] refuses to start on one that does not
//! (see [`S3Kv::probe_conditional_writes`]): a store that silently ignored them
//! would pass every single-writer test and lose updates under the first race.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};

use crate::kv::{Cas, KvBackend};
use crate::tenant::{BucketId, StoreClass};

/// Signed URLs live this long. They are used at once; the margin is clock skew.
const SIGN_TTL: Duration = Duration::from_secs(300);

/// How many times a plain `set` or `increment` re-reads after losing a race
/// before it gives up. Each attempt is a HEAD + PUT, so this is seconds, not a
/// spin.
const MAX_RETRIES: usize = 32;

const REV_HEADER: &str = "x-amz-meta-rev";

/// Where the store is and how to sign for it.
#[derive(Debug, Clone)]
pub struct S3Config {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    /// `https://host/bucket/key` rather than `https://bucket.host/key`. RustFS
    /// and MinIO need it; R2 and S3 accept either.
    pub path_style: bool,
}

pub struct S3Kv {
    bucket: Bucket,
    creds: Credentials,
    agent: ureq::Agent,
}

/// What a read learned about an object, besides its bytes.
struct Head {
    rev: u64,
    etag: String,
}

/// The precondition a PUT is sent with.
enum Precondition<'a> {
    /// `If-None-Match: *` — the object must not exist.
    Absent,
    /// `If-Match: <etag>` — the object must still be the one read.
    Is(&'a str),
}

impl S3Kv {
    /// Connect, create the bucket if it is missing, and refuse a store that does
    /// not enforce conditional writes.
    pub fn connect(cfg: &S3Config) -> Result<Self> {
        let endpoint: url::Url = cfg.endpoint.parse().context("--s3-endpoint")?;
        let style = if cfg.path_style { UrlStyle::Path } else { UrlStyle::VirtualHost };
        let bucket = Bucket::new(endpoint, style, cfg.bucket.clone(), cfg.region.clone())
            .context("S3 bucket URL")?;
        // `ureq`, like `SurrealKv`: every `KvBackend` method is sync inside an
        // async host, and `reqwest::blocking` panics there.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build();
        let kv = Self {
            bucket,
            creds: Credentials::new(cfg.access_key.clone(), cfg.secret_key.clone()),
            agent,
        };
        kv.ensure_bucket()?;
        kv.probe_conditional_writes()?;
        Ok(kv)
    }

    fn ensure_bucket(&self) -> Result<()> {
        let url = self.bucket.head_bucket(Some(&self.creds)).sign(SIGN_TTL);
        match self.agent.head(url.as_str()).call() {
            Ok(_) => return Ok(()),
            Err(ureq::Error::Status(404, _)) => {}
            Err(e) => bail!("S3 bucket {} is unreachable: {}", self.bucket.name(), describe(e)),
        }
        let url = self.bucket.create_bucket(&self.creds).sign(SIGN_TTL);
        match self.agent.put(url.as_str()).call() {
            Ok(_) => {
                eprintln!("comp-host: created S3 bucket {}", self.bucket.name());
                Ok(())
            }
            // Another host created it in between.
            Err(ureq::Error::Status(409, _)) => Ok(()),
            Err(e) => bail!("could not create S3 bucket {}: {}", self.bucket.name(), describe(e)),
        }
    }

    /// Prove the store refuses a write whose precondition fails.
    ///
    /// Four writes to a scratch key: a create that must land, a second create
    /// that must be refused, a replace against a wrong ETag that must be refused,
    /// and a replace against the right one that must land. A store that answers
    /// `200` to either refusal ignores the header, and every compare-and-set on
    /// it would be a silent last-writer-wins.
    pub fn probe_conditional_writes(&self) -> Result<()> {
        let key = format!(
            ".probe/{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let refuse = |what: &str| {
            anyhow::anyhow!(
                "S3 store at {} ignored {what}: it does not enforce conditional writes, so \
                 compare-and-set on it would silently lose updates. Use a store that does \
                 (AWS S3, Cloudflare R2, MinIO) or leave --blob unset.",
                self.bucket.base_url()
            )
        };
        let result = (|| {
            let first = self.put(&key, b"1", 1, Precondition::Absent)?.context("probe create")?;
            if self.put(&key, b"2", 2, Precondition::Absent)?.is_some() {
                return Err(refuse("If-None-Match: *"));
            }
            if self.put(&key, b"2", 2, Precondition::Is("\"0000\""))?.is_some() {
                return Err(refuse("If-Match"));
            }
            self.put(&key, b"2", 2, Precondition::Is(&first))?
                .context("a replace against the ETag just returned was refused")?;
            Ok(())
        })();
        let _ = self.delete_object(&key);
        result
    }

    /// The object key for a host bucket and a guest key.
    ///
    /// Reversible, unlike the NATS encoding: `list_keys` has to hand back the
    /// keys as the guest wrote them, and S3 keys are opaque bytes we control.
    /// Everything outside `[A-Za-z0-9._-]` — `=` included — becomes `=XX`.
    fn object_key(bucket: &BucketId, key: &str) -> String {
        format!("{}/{}", bucket.as_str(), escape(key))
    }

    fn head(&self, object: &str) -> Result<Option<Head>> {
        let url = self.bucket.head_object(Some(&self.creds), object).sign(SIGN_TTL);
        match self.agent.head(url.as_str()).call() {
            Ok(r) => Ok(Some(head_of(&r))),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => bail!("S3 HEAD {object}: {}", describe(e)),
        }
    }

    fn read(&self, object: &str) -> Result<Option<(Head, Vec<u8>)>> {
        let url = self.bucket.get_object(Some(&self.creds), object).sign(SIGN_TTL);
        match self.agent.get(url.as_str()).call() {
            Ok(r) => {
                let head = head_of(&r);
                let mut body = Vec::new();
                std::io::Read::read_to_end(&mut r.into_reader(), &mut body)
                    .with_context(|| format!("S3 GET {object}: reading the body"))?;
                Ok(Some((head, body)))
            }
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => bail!("S3 GET {object}: {}", describe(e)),
        }
    }

    /// A conditional PUT. `Some(etag)` if it landed, `None` if the precondition
    /// failed — `412`, or `409` which S3 answers when two conditional writes to
    /// one key are in flight at once.
    fn put(
        &self,
        object: &str,
        value: &[u8],
        rev: u64,
        pre: Precondition,
    ) -> Result<Option<String>> {
        let rev_s = rev.to_string();
        let mut action = self.bucket.put_object(Some(&self.creds), object);
        action.headers_mut().insert(REV_HEADER, rev_s.as_str());
        let (name, cond) = match pre {
            Precondition::Absent => ("if-none-match", "*"),
            Precondition::Is(etag) => ("if-match", etag),
        };
        action.headers_mut().insert(name, cond);
        let url = action.sign(SIGN_TTL);
        match self.agent.put(url.as_str()).set(REV_HEADER, &rev_s).set(name, cond).send_bytes(value)
        {
            Ok(r) => Ok(Some(r.header("etag").unwrap_or_default().to_string())),
            Err(ureq::Error::Status(412 | 409, _)) => Ok(None),
            Err(e) => bail!("S3 PUT {object}: {}", describe(e)),
        }
    }

    fn delete_object(&self, object: &str) -> Result<()> {
        let url = self.bucket.delete_object(Some(&self.creds), object).sign(SIGN_TTL);
        match self.agent.delete(url.as_str()).call() {
            Ok(_) | Err(ureq::Error::Status(404, _)) => Ok(()),
            Err(e) => bail!("S3 DELETE {object}: {}", describe(e)),
        }
    }

    /// Write at the next revision, whatever the current one is, retrying a lost
    /// race. Returns the revision it landed at.
    fn write_next(&self, object: &str, value: &[u8]) -> Result<u64> {
        for attempt in 0..MAX_RETRIES {
            let landed = match self.head(object)? {
                None => self.put(object, value, 1, Precondition::Absent)?.map(|_| 1),
                Some(h) => self
                    .put(object, value, h.rev + 1, Precondition::Is(&h.etag))?
                    .map(|_| h.rev + 1),
            };
            if let Some(rev) = landed {
                return Ok(rev);
            }
            backoff(attempt);
        }
        bail!("S3 PUT {object}: lost the race {MAX_RETRIES} times in a row")
    }
}

impl KvBackend for S3Kv {
    /// One bucket, reached over the network, is one store for every replica.
    fn shared(&self) -> bool {
        true
    }

    fn get(&self, bucket: &BucketId, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.read(&Self::object_key(bucket, key))?.map(|(_, body)| body))
    }

    fn set(&self, bucket: &BucketId, key: &str, value: &[u8]) -> Result<()> {
        self.write_next(&Self::object_key(bucket, key), value).map(|_| ())
    }

    fn delete(&self, bucket: &BucketId, key: &str) -> Result<()> {
        self.delete_object(&Self::object_key(bucket, key))
    }

    fn exists(&self, bucket: &BucketId, key: &str) -> Result<bool> {
        Ok(self.head(&Self::object_key(bucket, key))?.is_some())
    }

    fn list_keys(&self, bucket: &BucketId) -> Result<Vec<String>> {
        let prefix = format!("{}/", bucket.as_str());
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut action = self.bucket.list_objects_v2(Some(&self.creds));
            action.with_prefix(prefix.as_str());
            if let Some(t) = &token {
                action.with_continuation_token(t.as_str());
            }
            let url = action.sign(SIGN_TTL);
            let body = match self.agent.get(url.as_str()).call() {
                Ok(r) => r.into_string().context("S3 LIST: reading the body")?,
                Err(e) => bail!("S3 LIST {prefix}: {}", describe(e)),
            };
            let page = rusty_s3::actions::ListObjectsV2::parse_response(&body)
                .map_err(|e| anyhow::anyhow!("S3 LIST {prefix}: {e}"))?;
            for obj in page.contents {
                if let Some(enc) = obj.key.strip_prefix(&prefix) {
                    keys.push(unescape(enc));
                }
            }
            match page.next_continuation_token {
                Some(t) => token = Some(t),
                None => return Ok(keys),
            }
        }
    }

    fn increment(&self, bucket: &BucketId, key: &str, delta: u64) -> Result<u64> {
        let object = Self::object_key(bucket, key);
        for attempt in 0..MAX_RETRIES {
            let (next, landed) = match self.read(&object)? {
                None => {
                    let next = delta;
                    (next, self.put(&object, next.to_string().as_bytes(), 1, Precondition::Absent)?)
                }
                Some((h, body)) => {
                    let cur: u64 =
                        std::str::from_utf8(&body).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
                    let next = cur.saturating_add(delta);
                    let bytes = next.to_string().into_bytes();
                    (next, self.put(&object, &bytes, h.rev + 1, Precondition::Is(&h.etag))?)
                }
            };
            if landed.is_some() {
                return Ok(next);
            }
            backoff(attempt);
        }
        bail!("S3 increment {object}: lost the race {MAX_RETRIES} times in a row")
    }

    fn get_revision(&self, bucket: &BucketId, key: &str) -> Result<Option<(u64, Vec<u8>)>> {
        Ok(self.read(&Self::object_key(bucket, key))?.map(|(h, body)| (h.rev, body)))
    }

    /// The compare happens at the store: the PUT carries the ETag of the object
    /// whose revision was compared, so anything that landed in between turns it
    /// into a `412` rather than an overwrite.
    fn set_if_revision(
        &self,
        bucket: &BucketId,
        key: &str,
        value: &[u8],
        expected: u64,
    ) -> Result<Cas> {
        let object = Self::object_key(bucket, key);
        let head = self.head(&object)?;
        let current = head.as_ref().map(|h| h.rev).unwrap_or(0);
        if current != expected {
            return Ok(Cas::Conflict(current));
        }
        let pre = match &head {
            None => Precondition::Absent,
            Some(h) => Precondition::Is(&h.etag),
        };
        match self.put(&object, value, expected + 1, pre)? {
            Some(_) => Ok(Cas::Committed(expected + 1)),
            None => Ok(Cas::Conflict(self.head(&object)?.map(|h| h.rev).unwrap_or(0))),
        }
    }
}

/// Sleep before retrying a lost race: exponential from 5 ms, capped at 250 ms,
/// with jitter so the losers of one race do not all collide again in the next.
/// Without it six writers on one key starved one of them past `MAX_RETRIES`
/// (measured: one run in four).
fn backoff(attempt: usize) {
    let cap = (5u64 << attempt.min(6)).min(250);
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0)
        % cap.max(1);
    std::thread::sleep(Duration::from_millis(cap / 2 + jitter / 2));
}

fn head_of(r: &ureq::Response) -> Head {
    Head {
        // An object some other writer put there without our header is at
        // revision 1: it exists, and nothing we wrote came before it.
        rev: r.header(REV_HEADER).and_then(|v| v.parse().ok()).unwrap_or(1),
        etag: r.header("etag").unwrap_or_default().to_string(),
    }
}

/// A failed request as one line, with S3's `<Code>` when it sent one.
fn describe(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, r) => {
            let body = r.into_string().unwrap_or_default();
            match body.split_once("<Code>").and_then(|(_, rest)| rest.split_once("</Code>")) {
                Some((c, _)) => format!("HTTP {code} {c}"),
                None => format!("HTTP {code}"),
            }
        }
        other => other.to_string(),
    }
}

fn escape(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for b in key.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' => out.push(b as char),
            _ => out.push_str(&format!("={b:02X}")),
        }
    }
    out
}

fn unescape(enc: &str) -> String {
    let bytes = enc.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' && i + 2 < bytes.len() {
            if let Some(b) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---- routing ----------------------------------------------------------------

/// Object buckets to one backend, keyed state to another.
///
/// The class is on the `BucketId`, which only a `Scope` mints, so what a guest
/// writes can change which key it touches but never which store it lands in.
pub struct RoutedKv {
    kv: Arc<dyn KvBackend>,
    objects: Arc<dyn KvBackend>,
}

impl RoutedKv {
    pub fn new(kv: Arc<dyn KvBackend>, objects: Arc<dyn KvBackend>) -> Arc<Self> {
        Arc::new(Self { kv, objects })
    }

    fn pick(&self, bucket: &BucketId) -> &dyn KvBackend {
        match bucket.class() {
            StoreClass::Kv => self.kv.as_ref(),
            StoreClass::Object => self.objects.as_ref(),
        }
    }
}

impl KvBackend for RoutedKv {
    /// Shared only if both halves are: a replica on another node reaches both.
    fn shared(&self) -> bool {
        self.kv.shared() && self.objects.shared()
    }
    fn get(&self, b: &BucketId, k: &str) -> Result<Option<Vec<u8>>> {
        self.pick(b).get(b, k)
    }
    fn set(&self, b: &BucketId, k: &str, v: &[u8]) -> Result<()> {
        self.pick(b).set(b, k, v)
    }
    fn delete(&self, b: &BucketId, k: &str) -> Result<()> {
        self.pick(b).delete(b, k)
    }
    fn exists(&self, b: &BucketId, k: &str) -> Result<bool> {
        self.pick(b).exists(b, k)
    }
    fn list_keys(&self, b: &BucketId) -> Result<Vec<String>> {
        self.pick(b).list_keys(b)
    }
    fn increment(&self, b: &BucketId, k: &str, d: u64) -> Result<u64> {
        self.pick(b).increment(b, k, d)
    }
    fn get_revision(&self, b: &BucketId, k: &str) -> Result<Option<(u64, Vec<u8>)>> {
        self.pick(b).get_revision(b, k)
    }
    fn set_if_revision(&self, b: &BucketId, k: &str, v: &[u8], e: u64) -> Result<Cas> {
        self.pick(b).set_if_revision(b, k, v, e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kv::MemoryKv;

    #[test]
    fn the_key_escape_round_trips_every_byte_that_matters() {
        for key in ["plain", "bo_c/n", "a=b", "=41", "\u{1f}x", "ünï", "", "trailing="] {
            assert_eq!(unescape(&escape(key)), key, "{key:?}");
        }
        // Distinct keys stay distinct, which the NATS encoding cannot promise.
        assert_ne!(escape("a/b"), escape("a=2Fb"));
    }

    #[test]
    fn routing_follows_the_class_the_host_assigned() {
        let kv: Arc<dyn KvBackend> = Arc::new(MemoryKv::default());
        let objects: Arc<dyn KvBackend> = Arc::new(MemoryKv::default());
        let r = RoutedKv::new(kv.clone(), objects.clone());
        let (state, blobs) = (BucketId::for_test("b"), BucketId::object_for_test("o"));
        r.set(&state, "k", b"state").unwrap();
        r.set(&blobs, "k", b"bytes").unwrap();
        assert_eq!(kv.get(&state, "k").unwrap().as_deref(), Some(&b"state"[..]));
        assert_eq!(objects.get(&blobs, "k").unwrap().as_deref(), Some(&b"bytes"[..]));
        assert!(kv.get(&blobs, "k").unwrap().is_none(), "object bytes leaked into the KV store");
        assert!(
            objects.get(&state, "k").unwrap().is_none(),
            "keyed state leaked into the object store"
        );
        // Memory is node-local, so the pair is too.
        assert!(!r.shared());
    }
}
