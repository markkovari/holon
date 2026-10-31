//! Automatic HTTP request idempotency for `holon-host`.
//!
//! Intercepts `Idempotency-Key` or `X-Idempotency-Key` headers on mutating requests.
//! Uses `KvBackend::set_if_revision` with revision 0 to atomically reserve
//! in-flight requests and replay completed responses without invoking the guest twice.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use serde::{Deserialize, Serialize};
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;

use crate::kv::{Cas, KvBackend};
use crate::tenant::BucketId;

const PENDING_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Serialize, Deserialize)]
enum IdempotencyRecord {
    Pending { created: u64 },
    Completed { created: u64, status: u16, headers: Vec<(String, String)>, body: Vec<u8> },
}

pub struct IdempotencyReservation<'a> {
    bucket: &'a BucketId,
    storage_key: String,
    kv: &'a Arc<dyn KvBackend>,
}

pub enum IdempotencyCheck<'a> {
    Proceed(Option<IdempotencyReservation<'a>>),
    Replay(hyper::Response<HyperOutgoingBody>),
    Conflict(hyper::Response<HyperOutgoingBody>),
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn sanitize_key(key: &str) -> String {
    key.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

pub async fn check<'a>(
    method: &hyper::Method,
    headers: &hyper::HeaderMap,
    bucket: Option<&'a BucketId>,
    kv: &'a Arc<dyn KvBackend>,
) -> Result<IdempotencyCheck<'a>> {
    if method == hyper::Method::GET
        || method == hyper::Method::HEAD
        || method == hyper::Method::OPTIONS
    {
        return Ok(IdempotencyCheck::Proceed(None));
    }

    let Some(bucket) = bucket else {
        return Ok(IdempotencyCheck::Proceed(None));
    };

    let key_hdr = headers.get("idempotency-key").or_else(|| headers.get("x-idempotency-key"));
    let Some(key_val) = key_hdr else {
        return Ok(IdempotencyCheck::Proceed(None));
    };

    let key_str = match key_val.to_str() {
        Ok(s) => s.trim(),
        Err(_) => return Ok(IdempotencyCheck::Proceed(None)),
    };

    if key_str.is_empty() {
        return Ok(IdempotencyCheck::Proceed(None));
    }

    let storage_key = format!("_idemp:{}", sanitize_key(key_str));
    let now = now_secs();
    let pending_val = serde_json::to_vec(&IdempotencyRecord::Pending { created: now })?;

    match kv.set_if_revision(bucket, &storage_key, &pending_val, 0)? {
        Cas::Committed(_) => {
            Ok(IdempotencyCheck::Proceed(Some(IdempotencyReservation { bucket, storage_key, kv })))
        }
        Cas::Conflict(_) => {
            if let Some(existing) = kv.get(bucket, &storage_key)? {
                if let Ok(rec) = serde_json::from_slice::<IdempotencyRecord>(&existing) {
                    match rec {
                        IdempotencyRecord::Completed { status, headers, body, .. } => {
                            let mut builder = hyper::Response::builder().status(status);
                            for (name, val) in headers {
                                builder = builder.header(name, val);
                            }
                            builder = builder.header("x-idempotency", "REPLAY");
                            let resp = builder.body(
                                Full::new(Bytes::from(body))
                                    .map_err(|never| match never {})
                                    .boxed_unsync(),
                            )?;
                            return Ok(IdempotencyCheck::Replay(resp));
                        }
                        IdempotencyRecord::Pending { created } => {
                            if now.saturating_sub(created) > PENDING_TIMEOUT_SECS {
                                kv.set(bucket, &storage_key, &pending_val)?;
                                return Ok(IdempotencyCheck::Proceed(Some(
                                    IdempotencyReservation { bucket, storage_key, kv },
                                )));
                            } else {
                                let resp = hyper::Response::builder()
                                    .status(hyper::StatusCode::CONFLICT)
                                    .header("retry-after", "1")
                                    .header("content-type", "text/plain; charset=utf-8")
                                    .body(
                                        Full::new(Bytes::from_static(b"concurrent request with this idempotency key is in-flight\n"))
                                            .map_err(|never| match never {})
                                            .boxed_unsync(),
                                    )?;
                                return Ok(IdempotencyCheck::Conflict(resp));
                            }
                        }
                    }
                }
            }
            Ok(IdempotencyCheck::Proceed(None))
        }
    }
}

pub async fn complete(
    reservation: Option<IdempotencyReservation<'_>>,
    resp: hyper::Response<HyperOutgoingBody>,
) -> Result<hyper::Response<HyperOutgoingBody>> {
    let Some(res) = reservation else {
        return Ok(resp);
    };

    let status = resp.status();
    let (parts, body) = resp.into_parts();
    let collected_bytes = body.collect().await?.to_bytes();

    if status.is_server_error() {
        let _ = res.kv.delete(res.bucket, &res.storage_key);
    } else {
        let mut cached_headers = Vec::new();
        for (name, val) in &parts.headers {
            if let Ok(v) = val.to_str() {
                cached_headers.push((name.as_str().to_string(), v.to_string()));
            }
        }
        let completed = IdempotencyRecord::Completed {
            created: now_secs(),
            status: status.as_u16(),
            headers: cached_headers,
            body: collected_bytes.to_vec(),
        };
        if let Ok(bytes) = serde_json::to_vec(&completed) {
            let _ = res.kv.set(res.bucket, &res.storage_key, &bytes);
        }
    }

    let new_body = Full::new(collected_bytes).map_err(|never| match never {}).boxed_unsync();
    Ok(hyper::Response::from_parts(parts, new_body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kv::MemoryKv;
    use crate::tenant::BucketId;

    #[tokio::test]
    async fn test_idempotency_lifecycle() {
        let kv: Arc<dyn KvBackend> = Arc::new(MemoryKv::default());
        let bucket = BucketId::for_test("test-app");

        let method = hyper::Method::POST;
        let mut headers = hyper::HeaderMap::new();
        headers.insert("idempotency-key", "test-key-1".parse().unwrap());

        // 1. First request: proceeds with reservation
        let check1 = check(&method, &headers, Some(&bucket), &kv).await.unwrap();
        let res = match check1 {
            IdempotencyCheck::Proceed(Some(r)) => r,
            _ => panic!("expected proceed with reservation"),
        };

        // 2. Concurrent request while in-flight: returns 409 Conflict
        let check2 = check(&method, &headers, Some(&bucket), &kv).await.unwrap();
        match check2 {
            IdempotencyCheck::Conflict(resp) => {
                assert_eq!(resp.status(), hyper::StatusCode::CONFLICT);
            }
            _ => panic!("expected conflict for concurrent in-flight"),
        }

        // 3. Complete first request
        let resp = hyper::Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(
                Full::new(Bytes::from_static(b"{\"status\":\"paid\"}"))
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
            .unwrap();
        let out_resp = complete(Some(res), resp).await.unwrap();
        assert_eq!(out_resp.status(), 200);

        // 4. Repeated request: replayed from cache without guest execution
        let check3 = check(&method, &headers, Some(&bucket), &kv).await.unwrap();
        match check3 {
            IdempotencyCheck::Replay(replayed) => {
                assert_eq!(replayed.status(), 200);
                assert_eq!(
                    replayed.headers().get("x-idempotency").and_then(|v| v.to_str().ok()),
                    Some("REPLAY")
                );
                let body = replayed.into_body().collect().await.unwrap().to_bytes();
                assert_eq!(&body[..], b"{\"status\":\"paid\"}");
            }
            _ => panic!("expected replay"),
        }
    }
}
