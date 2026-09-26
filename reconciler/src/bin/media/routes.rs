//! The daemon's shared state, and the HTTP surface: signing, queuing, and
//! handing out renditions. `components/media-pipeline/CONTRACT.md` is the
//! wire format this implements.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::errors::{answer, unavailable, MediaError};
use crate::keys::{allowed_ext, parse_key, plan_parts, valid_id, Which, ALLOWED_EXTS, PART_SIZE};
use crate::store::{callback_allowed, now_secs, Store, UPLOAD_URL_TTL};
use crate::SUBJECT;

/// S3 allows 10,000 parts; at 16 MiB that is 156 GiB, far past any cap.
const MAX_PARTS: u64 = 10_000;
/// What a browser plausibly sends for those. An ARW has no registered type,
/// so browsers send an empty string or octet-stream for it.
const ALLOWED_TYPES: &[&str] =
    &["", "application/octet-stream", "image/jpeg", "image/x-sony-arw", "image/arw"];

pub(crate) struct Daemon {
    pub(crate) store: Store,
    pub(crate) nats: async_nats::Client,
    pub(crate) js: async_nats::jetstream::Context,
    pub(crate) max_upload: u64,
    pub(crate) sign_originals: bool,
    pub(crate) apple_helper: Option<PathBuf>,
    pub(crate) callback_secret: String,
    pub(crate) callback_allow: Vec<String>,
    pub(crate) work_dir: PathBuf,
}

impl Daemon {
    pub(crate) fn apple(&self) -> Option<&Path> {
        self.apple_helper.as_deref().filter(|p| p.is_file())
    }
}

pub(crate) type Shared = State<Arc<Daemon>>;

#[derive(Deserialize)]
pub(crate) struct StartUpload {
    photo_id: String,
    filename: String,
    size: u64,
    #[serde(default)]
    content_type: String,
}

pub(crate) async fn start_upload(State(d): Shared, Json(req): Json<StartUpload>) -> Json<Value> {
    answer(start_upload_inner(&d, req).await)
}

async fn start_upload_inner(d: &Daemon, req: StartUpload) -> Result<Value, MediaError> {
    if !valid_id(&req.photo_id) {
        return Err(MediaError::Refused(format!(
            "photo_id must match [A-Za-z0-9_-]{{1,64}}: {:?}",
            req.photo_id
        )));
    }
    let Some(ext) = allowed_ext(&req.filename) else {
        return Err(MediaError::Refused(format!(
            "not an accepted file type: {:?} (accepts {ALLOWED_EXTS:?})",
            req.filename
        )));
    };
    if !ALLOWED_TYPES.contains(&req.content_type.to_ascii_lowercase().as_str()) {
        return Err(MediaError::Refused(format!(
            "not an accepted content type: {:?}",
            req.content_type
        )));
    }
    if req.size == 0 || req.size > d.max_upload {
        return Err(MediaError::Refused(format!(
            "size {} is outside 1..={} bytes",
            req.size, d.max_upload
        )));
    }
    let parts = plan_parts(req.size, PART_SIZE);
    if parts.len() as u64 > MAX_PARTS {
        return Err(MediaError::Refused("too many parts".into()));
    }
    let object = format!("{}.{ext}", req.photo_id);
    let upload_id = d.store.create_upload(&object, &req.content_type).await?;
    let urls: Vec<Value> = parts
        .iter()
        .map(|(n, _)| json!({ "number": n, "url": d.store.part_url(&object, *n, &upload_id) }))
        .collect();
    Ok(json!({
        "upload_id": upload_id,
        "key": format!("originals/{object}"),
        "part_size": PART_SIZE,
        "parts": urls,
        "expires_at": now_secs() + UPLOAD_URL_TTL.as_secs(),
    }))
}

#[derive(Deserialize)]
struct PartEtag {
    number: u32,
    etag: String,
}

#[derive(Deserialize)]
pub(crate) struct CompleteUpload {
    upload_id: String,
    key: String,
    parts: Vec<PartEtag>,
}

/// The part list must be exactly 1..=n — rusty-s3 numbers the ETags by
/// position, so a gap or a duplicate would silently stitch the wrong bytes.
fn ordered_etags(mut parts: Vec<PartEtag>) -> Result<Vec<String>, MediaError> {
    parts.sort_by_key(|p| p.number);
    if parts.is_empty() || parts.iter().enumerate().any(|(i, p)| p.number as usize != i + 1) {
        return Err(MediaError::Refused(
            "parts must be numbered 1..=n with none missing or repeated".into(),
        ));
    }
    Ok(parts.into_iter().map(|p| p.etag).collect())
}

pub(crate) async fn complete_upload(
    State(d): Shared,
    Json(req): Json<CompleteUpload>,
) -> Json<Value> {
    answer(
        async {
            let Some((Which::Originals, object)) = parse_key(&req.key) else {
                return Err(MediaError::Refused(format!("not an originals key: {:?}", req.key)));
            };
            let etags = ordered_etags(req.parts)?;
            d.store.complete_upload(object, &req.upload_id, &etags).await?;
            Ok(json!({ "key": req.key }))
        }
        .await,
    )
}

#[derive(Deserialize)]
pub(crate) struct AbortUpload {
    upload_id: String,
    key: String,
}

pub(crate) async fn abort_upload(State(d): Shared, Json(req): Json<AbortUpload>) -> Json<Value> {
    answer(
        async {
            let Some((Which::Originals, object)) = parse_key(&req.key) else {
                return Err(MediaError::Refused(format!("not an originals key: {:?}", req.key)));
            };
            d.store.abort_upload(object, &req.upload_id).await?;
            Ok(json!({}))
        }
        .await,
    )
}

#[derive(Deserialize, serde::Serialize, Clone)]
pub(crate) struct Job {
    pub(crate) job_id: String,
    pub(crate) photo_id: String,
    pub(crate) key: String,
    pub(crate) callback_url: String,
}

pub(crate) async fn submit(State(d): Shared, Json(job): Json<Job>) -> Json<Value> {
    answer(
        async {
            if !valid_id(&job.job_id) || !valid_id(&job.photo_id) {
                return Err(MediaError::Refused(
                    "job_id and photo_id must match [A-Za-z0-9_-]{1,64}".into(),
                ));
            }
            match parse_key(&job.key) {
                Some((Which::Originals, obj)) if obj.starts_with(&format!("{}.", job.photo_id)) => {
                }
                _ => {
                    return Err(MediaError::Refused(format!(
                        "not this photo's original: {:?}",
                        job.key
                    )))
                }
            }
            callback_allowed(&job.callback_url, &d.callback_allow).map_err(MediaError::Refused)?;
            let mut headers = async_nats::HeaderMap::new();
            headers.insert("Nats-Msg-Id", job.job_id.as_str());
            let payload = serde_json::to_vec(&job).map_err(unavailable)?;
            let ack =
                d.js.publish_with_headers(SUBJECT, headers, payload.into())
                    .await
                    .map_err(unavailable)?
                    .await
                    .map_err(unavailable)?;
            if ack.duplicate {
                eprintln!(
                    "comp-media: job {} was already queued — dropped as a duplicate",
                    job.job_id
                );
            }
            Ok(json!({ "job_id": job.job_id, "queued": true }))
        }
        .await,
    )
}

#[derive(Deserialize)]
pub(crate) struct SignReq {
    key: String,
    ttl_secs: u64,
}

/// SigV4's own ceiling for a presigned URL.
const MAX_SIGN_TTL: u64 = 7 * 24 * 3600;

pub(crate) async fn sign(State(d): Shared, Json(req): Json<SignReq>) -> Json<Value> {
    answer((|| {
        let Some((which, object)) = parse_key(&req.key) else {
            return Err(MediaError::Refused(format!(
                "not a key this daemon issues: {:?}",
                req.key
            )));
        };
        if which == Which::Originals && !d.sign_originals {
            return Err(MediaError::Refused(
                "originals are not signed for browsers (see --sign-originals)".into(),
            ));
        }
        let ttl = std::time::Duration::from_secs(req.ttl_secs.clamp(1, MAX_SIGN_TTL));
        Ok(json!({ "url": d.store.sign_get(which, object, ttl) }))
    })())
}

pub(crate) async fn health(State(d): Shared) -> Json<Value> {
    let queue = d.nats.connection_state() == async_nats::connection::State::Connected;
    Json(
        json!({ "ok": true, "store": d.store.healthy().await, "queue": queue, "apple": d.apple().is_some() }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_part_list_with_a_gap_or_a_repeat_is_refused() {
        let p = |n: u32| PartEtag { number: n, etag: format!("\"e{n}\"") };
        assert_eq!(
            ordered_etags(vec![p(2), p(1), p(3)]).unwrap(),
            vec!["\"e1\"", "\"e2\"", "\"e3\""]
        );
        assert!(ordered_etags(vec![p(1), p(3)]).is_err());
        assert!(ordered_etags(vec![p(1), p(1)]).is_err());
        assert!(ordered_etags(vec![p(2)]).is_err());
        assert!(ordered_etags(vec![]).is_err());
    }
}
