//! `blob-store` — reference implementation of `blob:store`.
//!
//! Large-object storage backed by `wasi:keyvalue`. Per object, two keys:
//!   `bo_{container}/{name}`  -> the raw bytes
//!   `bm_{container}/{name}`  -> metadata line `{size}:{content-type}`
//! Both `container` and `name` are sanitized (so the `/` separator and `_`
//! escape char can't appear literally inside either part), letting `list` scan
//! by the `bm_{container}/{prefix}` key prefix.
//!
//! Whole-body `list<u8>` in/out — wasip2-stable. A real deployment binds the kv
//! import to an object backend (S3/R2/GCS/fs); the component never knows.

#[allow(warnings)]
mod bindings;

use bindings::exports::blob::store::blobstore::{BlobError, Guest, ObjectInfo};
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";

// ---- key scheme ---------------------------------------------------------

fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            // '/' (separator) and '_' (escape lead) are escaped so they can't
            // appear literally inside a sanitized container/name part.
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'=' | b'.' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn unsanitize(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'_' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `{container}/{name}` with both parts sanitized.
fn scoped(container: &str, name: &str) -> String {
    format!("{}/{}", sanitize(container), sanitize(name))
}
fn data_key(container: &str, name: &str) -> String {
    format!("bo_{}", scoped(container, name))
}
fn meta_key(container: &str, name: &str) -> String {
    format!("bm_{}", scoped(container, name))
}
/// The `bm_{container}/` prefix a `list` scan filters on.
fn meta_container_prefix(container: &str) -> String {
    format!("bm_{}/", sanitize(container))
}

fn open() -> Result<kv::Bucket, BlobError> {
    kv::open(BUCKET).map_err(|e| BlobError::BackendUnavailable(format!("open: {e:?}")))
}

/// Parse a metadata line `{size}:{content-type}` into (size, content-type).
fn parse_meta(bytes: &[u8]) -> Option<(u64, String)> {
    let s = String::from_utf8(bytes.to_vec()).ok()?;
    let (size, ct) = s.split_once(':')?;
    Some((size.parse().ok()?, ct.to_string()))
}

impl Guest for Component {
    fn put(
        container: String,
        name: String,
        data: Vec<u8>,
        content_type: String,
    ) -> Result<(), BlobError> {
        let bucket = open()?;
        let meta = format!("{}:{}", data.len(), content_type);
        bucket
            .set(&data_key(&container, &name), &data)
            .map_err(|e| BlobError::BackendUnavailable(format!("set data: {e:?}")))?;
        bucket
            .set(&meta_key(&container, &name), meta.as_bytes())
            .map_err(|e| BlobError::BackendUnavailable(format!("set meta: {e:?}")))
    }

    fn get(container: String, name: String) -> Result<Vec<u8>, BlobError> {
        match open()?.get(&data_key(&container, &name)) {
            Ok(Some(bytes)) => Ok(bytes),
            Ok(None) => Err(BlobError::NotFound),
            Err(e) => Err(BlobError::BackendUnavailable(format!("get: {e:?}"))),
        }
    }

    fn head(container: String, name: String) -> Result<ObjectInfo, BlobError> {
        match open()?.get(&meta_key(&container, &name)) {
            Ok(Some(bytes)) => {
                let (size, content_type) = parse_meta(&bytes)
                    .ok_or_else(|| BlobError::BackendUnavailable("corrupt metadata".into()))?;
                Ok(ObjectInfo { name, size, content_type })
            }
            Ok(None) => Err(BlobError::NotFound),
            Err(e) => Err(BlobError::BackendUnavailable(format!("head: {e:?}"))),
        }
    }

    fn exists(container: String, name: String) -> Result<bool, BlobError> {
        open()?
            .exists(&data_key(&container, &name))
            .map_err(|e| BlobError::BackendUnavailable(format!("exists: {e:?}")))
    }

    fn delete(container: String, name: String) -> Result<(), BlobError> {
        let bucket = open()?;
        let _ = bucket.delete(&data_key(&container, &name));
        bucket
            .delete(&meta_key(&container, &name))
            .map_err(|e| BlobError::BackendUnavailable(format!("delete: {e:?}")))
    }

    fn list_objects(container: String, prefix: String) -> Result<Vec<ObjectInfo>, BlobError> {
        let bucket = open()?;
        let cprefix = meta_container_prefix(&container);
        let mut out = Vec::new();
        let mut cursor: Option<u64> = None;
        loop {
            let page = bucket
                .list_keys(cursor)
                .map_err(|e| BlobError::BackendUnavailable(format!("list-keys: {e:?}")))?;
            for key in &page.keys {
                let Some(enc_name) = key.strip_prefix(&cprefix) else {
                    continue;
                };
                let name = unsanitize(enc_name);
                if !name.starts_with(&prefix) {
                    continue;
                }
                if let Ok(Some(bytes)) = bucket.get(key) {
                    if let Some((size, content_type)) = parse_meta(&bytes) {
                        out.push(ObjectInfo { name, size, content_type });
                    }
                }
            }
            match page.cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(out)
    }
}

bindings::export!(Component with_types_in bindings);
