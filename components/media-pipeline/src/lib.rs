//! `media-pipeline` — presigned uploads and evaluation jobs for large photos
//!
//! Everything interesting happens in `comp-media`, which is native (ADR-0098):
//! it signs URLs against the object store and owns the `MEDIA_JOBS` work queue.
//! This is the component side. It holds the `media:pipeline/jobs` contract and
//! turns each call into exactly one HTTP request to the daemon, the same way
//! `image-optimizer` reaches `comp-imageopt`. No call carries pixels — only
//! keys, sizes and URLs — so nothing here ever holds more than a few KiB.
//!
//! Config (wasi:config/store):
//!   media-url     where `comp-media` is listening, e.g. http://127.0.0.1:8013
//!   media-token   bearer token matching the daemon's --token (absent = none)
//!
//! The wire shapes are `CONTRACT.md`'s, and that file is authoritative.

#[allow(warnings)]
mod bindings;

use bindings::exports::media::pipeline::jobs::{Guest, MediaError, PartEtag, PartUrl, UploadPlan};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
use bindings::wasi::io::streams::StreamError;
use serde_json::{json, Value};

struct Component;

/// Thirty seconds. Every route is bookkeeping — a presign, a
/// `CompleteMultipartUpload`, a publish — and none of them waits on the
/// evaluation itself, which comes back later as a callback.
const TIMEOUT_NS: u64 = 30_000_000_000;

fn daemon_url() -> Result<String, MediaError> {
    match config::get("media-url") {
        Ok(Some(u)) if !u.is_empty() => Ok(u),
        // Unavailable rather than refused: nobody decided anything, the
        // deployment simply never said where the daemon is.
        _ => Err(MediaError::Unavailable(
            "media-url is not set — this component has nowhere to ask".into(),
        )),
    }
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), MediaError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(MediaError::Unavailable(format!("media-url must be http(s), got {url:?}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].trim_end_matches('/').to_string()),
        None => (rest.to_string(), String::new()),
    };
    Ok((scheme, authority, path))
}

/// POST `body` to `route` and return the parsed JSON answer.
///
/// Every failure at this level is `unavailable`: the daemon's own refusals
/// arrive as JSON with a 200 (CONTRACT.md), so a non-200 — a 401 for a wrong
/// token included — is the transport or the deployment, never an answer.
fn post(route: &str, body: &Value) -> Result<Value, MediaError> {
    let url = daemon_url()?;
    let (scheme, authority, base) = parse_url(&url)?;
    let net = |m: &str| MediaError::Unavailable(m.to_string());

    let payload = body.to_string().into_bytes();
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    // Stated, so the request is not sent chunked: every body here is small and
    // already whole, and a length is one less thing a daemon has to decode.
    let _ = headers.set("content-length", &[payload.len().to_string().into_bytes()]);
    // A shared secret, if the deployment set one — see
    // `comp_reconciler::daemon_auth`'s own doc for why loopback binding alone
    // is not a boundary. Absent means the daemon runs with no --token.
    if let Ok(Some(token)) = config::get("media-token") {
        if !token.is_empty() {
            let _ = headers.set("authorization", &[format!("Bearer {token}").into_bytes()]);
        }
    }
    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&format!("{base}{route}"))).map_err(|_| net("set path"))?;

    let out = req.body().map_err(|_| net("body"))?;
    {
        let stream = out.write().map_err(|_| net("write"))?;
        // Chunked: `blocking-write-and-flush` traps above 4096 bytes, and a
        // part list for a 512 MiB upload is thirty-two ETags long.
        for chunk in payload.chunks(4096) {
            stream
                .blocking_write_and_flush(chunk)
                .map_err(|e| net(&format!("body write: {e:?}")))?;
        }
    }
    OutgoingBody::finish(out, None).map_err(|_| net("finish"))?;

    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(TIMEOUT_NS));
    let _ = opts.set_first_byte_timeout(Some(TIMEOUT_NS));
    let _ = opts.set_between_bytes_timeout(Some(TIMEOUT_NS));

    let fut =
        outgoing_handler::handle(req, Some(opts)).map_err(|e| net(&format!("handle: {e:?}")))?;
    fut.subscribe().block();
    let resp = fut
        .get()
        .ok_or_else(|| net("no response"))?
        .map_err(|_| net("response taken"))?
        .map_err(|e| net(&format!("http: {e:?}")))?;
    let status = resp.status();

    let body = resp.consume().map_err(|_| net("consume"))?;
    let stream = body.stream().map_err(|_| net("stream"))?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(c) if c.is_empty() => break,
            Ok(c) => buf.extend_from_slice(&c),
            // `Closed` is end-of-body; anything else is a read that went
            // wrong, and parsing what arrived would be a truncated answer
            // presented as a whole one.
            Err(StreamError::Closed) => break,
            Err(e) => return Err(net(&format!("read: {e:?}"))),
        }
    }
    drop(stream);
    if status != 200 {
        let text = String::from_utf8_lossy(&buf);
        return Err(net(&format!(
            "comp-media answered {status} on {route}: {}",
            truncate(&text, 200)
        )));
    }
    let reply: Value = serde_json::from_slice(&buf).map_err(|_| {
        net(&format!("comp-media answered {route} with something that is not JSON"))
    })?;
    check(reply)
}

/// The daemon reports its refusals in the body. Mapping them back to the
/// variant matters: a caller that cannot tell "too big" from "the daemon is
/// down" will retry the first one forever.
fn check(reply: Value) -> Result<Value, MediaError> {
    let Some(err) = reply.get("error").and_then(Value::as_str) else {
        return Ok(reply);
    };
    let detail = reply.get("detail").and_then(Value::as_str).unwrap_or_default().to_string();
    Err(match err {
        "refused" => MediaError::Refused(detail),
        "not-found" => MediaError::NotFound(detail),
        "unavailable" => MediaError::Unavailable(detail),
        // An error the contract does not name is still an error — and the
        // one a caller may retry, since nothing said it was a decision.
        other => MediaError::Unavailable(format!("{other}: {detail}")),
    })
}

fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn missing(route: &str, field: &str) -> MediaError {
    MediaError::Unavailable(format!("comp-media's {route} answer had no {field}"))
}

/// `POST /uploads`' answer, as the WIT record.
fn plan_from(reply: &Value) -> Result<UploadPlan, MediaError> {
    let s = |k: &str| {
        reply
            .get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| missing("/uploads", k))
    };
    let n = |k: &str| reply.get(k).and_then(Value::as_u64).ok_or_else(|| missing("/uploads", k));
    let parts = reply
        .get("parts")
        .and_then(Value::as_array)
        .ok_or_else(|| missing("/uploads", "parts"))?
        .iter()
        .map(|p| {
            let number = p
                .get("number")
                .and_then(Value::as_u64)
                .ok_or_else(|| missing("/uploads", "parts[].number"))?;
            let url = p
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| missing("/uploads", "parts[].url"))?;
            Ok(PartUrl { number: number as u32, url: url.to_string() })
        })
        .collect::<Result<Vec<_>, MediaError>>()?;
    Ok(UploadPlan {
        upload_id: s("upload_id")?,
        key: s("key")?,
        part_size: n("part_size")?,
        parts,
        expires_at: n("expires_at")?,
    })
}

impl Guest for Component {
    fn start_upload(
        photo_id: String,
        filename: String,
        size: u64,
        content_type: String,
    ) -> Result<UploadPlan, MediaError> {
        let reply = post(
            "/uploads",
            &json!({"photo_id": photo_id, "filename": filename, "size": size, "content_type": content_type}),
        )?;
        plan_from(&reply)
    }

    fn complete_upload(
        upload_id: String,
        key: String,
        parts: Vec<PartEtag>,
    ) -> Result<(), MediaError> {
        let parts: Vec<Value> =
            parts.iter().map(|p| json!({"number": p.number, "etag": p.etag})).collect();
        post("/uploads/complete", &json!({"upload_id": upload_id, "key": key, "parts": parts}))
            .map(|_| ())
    }

    fn abort_upload(upload_id: String, key: String) -> Result<(), MediaError> {
        post("/uploads/abort", &json!({"upload_id": upload_id, "key": key})).map(|_| ())
    }

    fn submit(
        job_id: String,
        photo_id: String,
        key: String,
        callback_url: String,
    ) -> Result<(), MediaError> {
        post(
            "/jobs",
            &json!({"job_id": job_id, "photo_id": photo_id, "key": key, "callback_url": callback_url}),
        )
        .map(|_| ())
    }

    fn sign_get(key: String, ttl_secs: u32) -> Result<String, MediaError> {
        let reply = post("/sign", &json!({"key": key, "ttl_secs": ttl_secs}))?;
        reply
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| missing("/sign", "url"))
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_told_apart_from_the_daemon_being_down() {
        let r = check(json!({"error": "refused", "detail": "too big"}));
        assert!(matches!(r, Err(MediaError::Refused(d)) if d == "too big"));
        let r = check(json!({"error": "not-found", "detail": "no upload"}));
        assert!(matches!(r, Err(MediaError::NotFound(_))));
        let r = check(json!({"error": "unavailable", "detail": "store down"}));
        assert!(matches!(r, Err(MediaError::Unavailable(_))));
    }

    #[test]
    fn an_error_the_contract_does_not_name_is_retryable_not_success() {
        let r = check(json!({"error": "exploded"}));
        assert!(matches!(r, Err(MediaError::Unavailable(d)) if d.starts_with("exploded")));
    }

    #[test]
    fn an_answer_without_an_error_passes_through() {
        assert!(check(json!({"key": "originals/x.arw"})).is_ok());
    }

    #[test]
    fn an_upload_plan_is_read_back_whole() {
        let plan = plan_from(&json!({
            "upload_id": "u1", "key": "originals/p1.arw", "part_size": 16777216,
            "parts": [{"number": 1, "url": "https://s3/1"}, {"number": 2, "url": "https://s3/2"}],
            "expires_at": 1_790_000_000u64,
        }))
        .unwrap();
        assert_eq!(plan.key, "originals/p1.arw");
        assert_eq!(plan.parts.len(), 2);
        assert_eq!(plan.parts[1].number, 2);
    }

    #[test]
    fn a_plan_missing_a_field_is_unavailable_rather_than_a_half_plan() {
        let r = plan_from(&json!({"upload_id": "u1", "key": "k", "part_size": 1, "expires_at": 1}));
        assert!(matches!(r, Err(MediaError::Unavailable(d)) if d.contains("parts")));
    }

    #[test]
    fn a_base_path_on_the_url_is_kept() {
        let (_, auth, base) = parse_url("http://127.0.0.1:8013/media/").unwrap();
        assert_eq!(auth, "127.0.0.1:8013");
        assert_eq!(base, "/media");
    }
}
