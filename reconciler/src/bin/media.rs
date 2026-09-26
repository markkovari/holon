//! `comp-media` — upload, evaluate and render photos too big for anything else.
//!
//! ## Why this is a process and not a component
//!
//! A Sony a7R V raw file is 129 MB. A guest has 64 MiB of memory, every image
//! app reads at most 16 MiB of body, and a NATS value is 1 MB (ADR-0098 has the
//! table). None of that is a gap to route around: a component's job here is
//! deciding who may upload what, not moving the bytes. So the bytes go to
//! S3-compatible object storage, straight from the browser, and this daemon does
//! the three things a `wasm32-wasip2` guest cannot — hold a JetStream consumer,
//! reach Metal / Core Image / Vision, and hold a 129 MB file.
//!
//! `components/media-pipeline` is the component side: it holds the WIT contract
//! and dials this. `components/media-pipeline/CONTRACT.md` is the wire format,
//! and it is authoritative — this file implements it, not the other way round.
//!
//! ## What it does
//!
//! - **Signs**, never carries. `POST /uploads` plans a presigned multipart
//!   upload; the browser `PUT`s each part to the store and reports the ETags;
//!   `POST /uploads/complete` stitches them. `POST /sign` hands out a
//!   time-limited GET for a rendition. rusty-s3 is sans-IO: it computes the
//!   SigV4 URL and the reqwest client sends it, so there is no S3 SDK here.
//! - **Queues.** `POST /jobs` publishes to the JetStream work-queue stream
//!   `MEDIA_JOBS` with `Nats-Msg-Id: <job_id>`, so a double submit is one job.
//!   A guest cannot publish to NATS (comp-host offers no messaging), which is
//!   why the queue is the daemon's.
//! - **Evaluates**, one photo at a time — the GPU is the bottleneck, not the
//!   queue. Download to a temp file (hashing as it streams), decode with
//!   `rawler`, take the half-resolution green plane straight from the Bayer
//!   data, then either run the Swift helper (`--apple-helper`: Core Image
//!   develop, Metal sharpness, Vision) or do it on the CPU (`rawler`'s own
//!   developer, the same sharpness metric in Rust, no Vision). Upload three
//!   renditions, then POST a signed result to the job's callback.
//!
//! ## Sharpness: `green-stab-v1`
//!
//! A plain Laplacian variance scores sensor noise as detail — at ISO 6400 the
//! "sharpest" tile of a real frame was an out-of-focus white shirt. So: square
//! root first (shot noise grows with √signal, this flattens it), a 3×3
//! binomial blur, the 4-neighbour Laplacian's variance per tile of a 16×16
//! grid, and the frame's own noise floor is its median tile. The Metal kernel
//! in `tools/media-apple` computes the same numbers; the unit tests below pin
//! the CPU half.
//!
//! ## Keys
//!
//! A key's first segment names the bucket and the rest is the object inside
//! it: `originals/<photo_id>.arw` is object `<photo_id>.arw` in
//! `--bucket-originals`. Keys are built here from a validated `photo_id` —
//! nothing a caller sends is spliced into a key unchecked.
//!
//!   comp-media --addr 127.0.0.1:8013 --token-file /run/credentials/media-token \
//!     --s3-endpoint http://127.0.0.1:9000 --s3-access-key-file ... --s3-secret-key-file ... \
//!     --nats-url nats://127.0.0.1:4222 --callback-secret-file ... \
//!     --callback-allow 127.0.0.1:3941 --apple-helper /usr/local/bin/comp-media-apple

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use hmac::{Hmac, Mac};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageEncoder};
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

#[derive(Parser)]
#[command(
    name = "comp-media",
    about = "Upload, evaluate and render large photos by key (ADR-0098)."
)]
struct Args {
    /// Shared secret a caller must send as `Authorization: Bearer <token>`.
    /// See `comp_reconciler::daemon_auth` for why loopback alone is not a
    /// boundary. No token means no check, logged loudly.
    #[arg(long)]
    token: Option<String>,
    /// Same, read from a file (a systemd `LoadCredential` path). Wins over
    /// `--token`.
    #[arg(long)]
    token_file: Option<PathBuf>,

    /// Where to listen. Loopback by default.
    #[arg(long, default_value = "127.0.0.1:8013")]
    addr: String,

    /// The S3 API endpoint this daemon talks to, e.g. `http://127.0.0.1:9000`
    /// for the compose RustFS.
    #[arg(long)]
    s3_endpoint: String,
    /// The endpoint BROWSERS reach the same store at — part-upload and
    /// rendition URLs are signed against it (SigV4 signs the host, so it
    /// cannot be rewritten afterwards). Defaults to `--s3-endpoint`; set it
    /// when this daemon reaches the store by an internal name.
    #[arg(long)]
    s3_public_endpoint: Option<String>,
    #[arg(long, default_value = "us-east-1")]
    s3_region: String,
    #[arg(long)]
    s3_access_key: Option<String>,
    #[arg(long)]
    s3_access_key_file: Option<PathBuf>,
    #[arg(long)]
    s3_secret_key: Option<String>,
    #[arg(long)]
    s3_secret_key_file: Option<PathBuf>,
    #[arg(long, default_value = "originals")]
    bucket_originals: String,
    #[arg(long, default_value = "renditions")]
    bucket_renditions: String,
    /// `http://host/bucket/key` rather than `http://bucket.host/key`. True by
    /// default: a self-hosted store on `127.0.0.1` has no DNS for the second.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    path_style: bool,

    /// The JetStream NATS that holds `MEDIA_JOBS`. `MEDIA_NATS_URL` sets it
    /// without editing an app's `extra_args` — `e2e/photoquest.sh` points the
    /// daemon at a private `nats-server -js` this way, since a dev box's
    /// :4222 is often someone else's NATS. The flag wins over the env.
    #[arg(long, env = "MEDIA_NATS_URL", default_value = "nats://127.0.0.1:4222")]
    nats_url: String,

    /// The HMAC key for the callback's `X-Media-Signature`. Required: a
    /// result nobody can verify is a result anybody could have sent.
    #[arg(long)]
    callback_secret: Option<String>,
    #[arg(long)]
    callback_secret_file: Option<PathBuf>,

    /// The Swift helper (`tools/media-apple`). Used per job when set and the
    /// file exists; otherwise everything runs on the CPU and `vision` is null.
    #[arg(long)]
    apple_helper: Option<PathBuf>,

    /// An upload larger than this is refused before any URL is signed.
    #[arg(long, default_value_t = 512)]
    max_upload_mb: u64,

    /// Let `/sign` sign `originals/` keys. Off by default: an original is for
    /// the evaluator, not for a browser.
    #[arg(long)]
    sign_originals: bool,

    /// A browser origin allowed to PUT parts and GET renditions, repeatable.
    /// Applied as bucket CORS (PutBucketCors) on startup. RustFS 1.0 answers
    /// CORS per listener from `RUSTFS_CORS_ALLOWED_ORIGINS` instead, and a
    /// refused PutBucketCors is logged with that workaround, not fatal.
    #[arg(long = "cors-origin")]
    cors_origin: Vec<String>,

    /// `host:port` a job's `callback_url` may point at, repeatable. Anything
    /// else is refused by `/jobs` (and dropped by the worker): the worker POSTs
    /// to that URL from inside the network, so an open list is an SSRF.
    /// Empty = every job refused, logged loudly at startup.
    #[arg(long = "callback-allow")]
    callback_allow: Vec<String>,

    /// Where a job's temp files live while it runs. Each job gets its own
    /// directory, removed when the job ends however it ends.
    #[arg(long)]
    work_dir: Option<PathBuf>,
}

/// S3's minimum part is 5 MiB; 16 MiB keeps a 129 MB raw file at 8 parts,
/// few enough that a browser's retry of one is cheap.
const PART_SIZE: u64 = 16 * 1024 * 1024;
/// S3 allows 10,000 parts; at 16 MiB that is 156 GiB, far past any cap.
const MAX_PARTS: u64 = 10_000;
/// Part URLs live this long — enough for a slow uplink to push 512 MB.
const UPLOAD_URL_TTL: Duration = Duration::from_secs(6 * 3600);
/// SigV4's own ceiling for a presigned URL.
const MAX_SIGN_TTL: u64 = 7 * 24 * 3600;
const ALLOWED_EXTS: &[&str] = &["arw", "jpg", "jpeg"];
/// What a browser plausibly sends for those. An ARW has no registered type,
/// so browsers send an empty string or octet-stream for it.
const ALLOWED_TYPES: &[&str] =
    &["", "application/octet-stream", "image/jpeg", "image/x-sony-arw", "image/arw"];
const STREAM: &str = "MEDIA_JOBS";
const SUBJECT: &str = "media.jobs";
const CONSUMER: &str = "media-worker";
/// A submit repeated inside this window is the same job.
const DEDUPE_WINDOW: Duration = Duration::from_secs(3600);
/// Short, with progress acks every 20 s while a job runs: a worker that dies
/// mid-photo has its job redelivered in a minute, not in a quarter of an hour.
const ACK_WAIT: Duration = Duration::from_secs(60);
const CALLBACK_ATTEMPTS: u32 = 5;
/// The web-share rendition must stay under this (a chat app's limit, and the
/// number the photoquest spec fixes).
const SHARE_MAX_BYTES: usize = 10 * 1024 * 1024;
const GRID: usize = 16;

// ---- validation --------------------------------------------------------------

/// `[A-Za-z0-9_-]{1,64}`. A `photo_id` becomes part of an object key and a
/// `job_id` a directory name, so neither may carry a `/` or a `..`.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The lower-cased extension of `filename`, if it is one this accepts.
fn allowed_ext(filename: &str) -> Option<String> {
    let ext = Path::new(filename).extension()?.to_str()?.to_ascii_lowercase();
    ALLOWED_EXTS.contains(&ext.as_str()).then_some(ext)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Originals,
    Renditions,
}

/// Parse a key this daemon could have made, and nothing else: `originals/<id>.<ext>`
/// or `renditions/<id>/{thumb,share,ai}.jpg`. Returns the bucket and the object
/// name inside it.
fn parse_key(key: &str) -> Option<(Which, &str)> {
    if let Some(obj) = key.strip_prefix("originals/") {
        let (id, ext) = obj.rsplit_once('.')?;
        return (valid_id(id) && ALLOWED_EXTS.contains(&ext)).then_some((Which::Originals, obj));
    }
    if let Some(obj) = key.strip_prefix("renditions/") {
        let (id, file) = obj.split_once('/')?;
        return (valid_id(id) && ["thumb.jpg", "share.jpg", "ai.jpg"].contains(&file))
            .then_some((Which::Renditions, obj));
    }
    None
}

/// `(number, length)` for every part of a `size`-byte upload. 1-based, as S3
/// numbers them; every part is `part_size` except a shorter last one.
fn plan_parts(size: u64, part_size: u64) -> Vec<(u16, u64)> {
    let n = size.div_ceil(part_size);
    (0..n)
        .map(|i| {
            let len = if i + 1 == n { size - i * part_size } else { part_size };
            (i as u16 + 1, len)
        })
        .collect()
}

// ---- errors, in the contract's shape -------------------------------------------

/// `refused` is a decision, `not-found` a missing thing, `unavailable` the
/// store or the queue. A caller should retry only the last.
#[derive(Debug)]
enum MediaError {
    Refused(String),
    NotFound(String),
    Unavailable(String),
}

impl MediaError {
    fn json(&self) -> Json<Value> {
        let (kind, detail) = match self {
            MediaError::Refused(d) => ("refused", d),
            MediaError::NotFound(d) => ("not-found", d),
            MediaError::Unavailable(d) => ("unavailable", d),
        };
        Json(json!({ "error": kind, "detail": detail }))
    }
}

fn unavailable(e: impl std::fmt::Display) -> MediaError {
    MediaError::Unavailable(e.to_string())
}

/// Every route answers 200: a non-200 means transport, same as `comp-imageopt`.
fn answer(r: Result<Value, MediaError>) -> Json<Value> {
    match r {
        Ok(v) => Json(v),
        Err(e) => e.json(),
    }
}

// ---- the store -----------------------------------------------------------------

struct Store {
    http: reqwest::Client,
    creds: Credentials,
    originals: Bucket,
    renditions: Bucket,
    /// The same buckets at `--s3-public-endpoint`: what a browser is handed.
    public_originals: Bucket,
    public_renditions: Bucket,
}

impl Store {
    fn bucket(&self, w: Which) -> &Bucket {
        match w {
            Which::Originals => &self.originals,
            Which::Renditions => &self.renditions,
        }
    }

    fn public_bucket(&self, w: Which) -> &Bucket {
        match w {
            Which::Originals => &self.public_originals,
            Which::Renditions => &self.public_renditions,
        }
    }

    /// Turn a non-2xx store answer into the contract's error. S3 puts the
    /// reason in an XML `<Code>`; a missing upload or key is `not-found`, a bad
    /// part list is `refused` (the caller sent it), anything else is ours.
    async fn check(resp: reqwest::Response, what: &str) -> Result<String, MediaError> {
        let status = resp.status();
        let body = resp.text().await.map_err(unavailable)?;
        // CompleteMultipartUpload can answer 200 with an <Error> body.
        if status.is_success() && !body.contains("<Error>") {
            return Ok(body);
        }
        let code = xml_tag(&body, "Code").unwrap_or_default();
        let detail = format!("{what}: {status} {code}");
        Err(match code.as_str() {
            "NoSuchUpload" | "NoSuchKey" | "NoSuchBucket" => MediaError::NotFound(detail),
            "InvalidPart" | "InvalidPartOrder" | "EntityTooSmall" => MediaError::Refused(detail),
            _ if status == reqwest::StatusCode::NOT_FOUND => MediaError::NotFound(detail),
            _ => MediaError::Unavailable(detail),
        })
    }

    async fn ensure_bucket(&self, w: Which) -> Result<()> {
        let b = self.bucket(w);
        let url = b.head_bucket(Some(&self.creds)).sign(Duration::from_secs(60));
        let resp = self.http.head(url).send().await.context("HEAD bucket")?;
        if resp.status().is_success() {
            return Ok(());
        }
        let url = b.create_bucket(&self.creds).sign(Duration::from_secs(60));
        let resp = self.http.put(url).send().await.context("create bucket")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        // A race with another instance creating it is fine.
        if status.is_success() || body.contains("BucketAlreadyOwnedByYou") {
            eprintln!("comp-media: created bucket {}", b.name());
            return Ok(());
        }
        bail!("could not create bucket {}: {status} {body}", b.name())
    }

    /// PutBucketCors, signed as a `PUT ?cors` on the bucket — rusty-s3 has no
    /// action for it, and `CreateBucket` is exactly a signed `PUT` on the
    /// bucket URL, so the query is the only difference. `ETag` must be exposed
    /// or the browser cannot read the part's tag back.
    async fn put_cors(&self, w: Which, origins: &[String]) -> Result<()> {
        use base64::Engine as _;
        let b = self.bucket(w);
        let body = cors_xml(origins);
        let md5 =
            base64::engine::general_purpose::STANDARD.encode(md5::Md5::digest(body.as_bytes()));
        let mut action = b.create_bucket(&self.creds);
        action.query_mut().insert("cors", "");
        let url = action.sign(Duration::from_secs(60));
        let resp = self.http.put(url).header("Content-MD5", md5).body(body).send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let text = resp.text().await.unwrap_or_default();
        bail!("{status} {}", xml_tag(&text, "Code").unwrap_or(text))
    }

    async fn healthy(&self) -> bool {
        let url = self.originals.head_bucket(Some(&self.creds)).sign(Duration::from_secs(30));
        matches!(
            self.http.head(url).timeout(Duration::from_secs(3)).send().await,
            Ok(r) if r.status().is_success()
        )
    }

    async fn create_upload(&self, object: &str, content_type: &str) -> Result<String, MediaError> {
        let url = self
            .originals
            .create_multipart_upload(Some(&self.creds), object)
            .sign(Duration::from_secs(60));
        let mut req = self.http.post(url);
        if !content_type.is_empty() {
            req = req.header("Content-Type", content_type);
        }
        let body = Self::check(req.send().await.map_err(unavailable)?, "create upload").await?;
        let parsed =
            rusty_s3::actions::CreateMultipartUpload::parse_response(&body).map_err(unavailable)?;
        Ok(parsed.upload_id().to_string())
    }

    fn part_url(&self, object: &str, number: u16, upload_id: &str) -> String {
        self.public_originals
            .upload_part(Some(&self.creds), object, number, upload_id)
            .sign(UPLOAD_URL_TTL)
            .to_string()
    }

    async fn complete_upload(
        &self,
        object: &str,
        upload_id: &str,
        etags: &[String],
    ) -> Result<(), MediaError> {
        let action = self.originals.complete_multipart_upload(
            Some(&self.creds),
            object,
            upload_id,
            etags.iter().map(String::as_str),
        );
        let url = action.sign(Duration::from_secs(60));
        let body = action.body();
        Self::check(
            self.http.post(url).body(body).send().await.map_err(unavailable)?,
            "complete upload",
        )
        .await?;
        Ok(())
    }

    async fn abort_upload(&self, object: &str, upload_id: &str) -> Result<(), MediaError> {
        let url = self
            .originals
            .abort_multipart_upload(Some(&self.creds), object, upload_id)
            .sign(Duration::from_secs(60));
        Self::check(self.http.delete(url).send().await.map_err(unavailable)?, "abort upload")
            .await?;
        Ok(())
    }

    fn sign_get(&self, w: Which, object: &str, ttl: Duration) -> String {
        self.public_bucket(w).get_object(Some(&self.creds), object).sign(ttl).to_string()
    }

    async fn put(&self, w: Which, object: &str, bytes: Vec<u8>, content_type: &str) -> Result<()> {
        let url =
            self.bucket(w).put_object(Some(&self.creds), object).sign(Duration::from_secs(300));
        let resp =
            self.http.put(url).header("Content-Type", content_type).body(bytes).send().await?;
        Self::check(resp, "put object").await.map_err(|e| anyhow!("{e:?}"))?;
        Ok(())
    }

    /// Stream an object to `dest`, hashing as it goes — the 129 MB original is
    /// never in memory, let alone twice. Returns (bytes, sha256 hex).
    async fn download(
        &self,
        w: Which,
        object: &str,
        dest: &Path,
        max: u64,
    ) -> Result<(u64, String)> {
        let url =
            self.bucket(w).get_object(Some(&self.creds), object).sign(Duration::from_secs(600));
        let mut resp = self.http.get(url).send().await?;
        if !resp.status().is_success() {
            let s = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("GET {object}: {s} {}", xml_tag(&text, "Code").unwrap_or_default());
        }
        let mut file =
            tokio::io::BufWriter::with_capacity(1 << 20, tokio::fs::File::create(dest).await?);
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        while let Some(chunk) = resp.chunk().await? {
            total += chunk.len() as u64;
            if total > max {
                bail!("{object} is larger than --max-upload-mb");
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok((total, hex(&hasher.finalize())))
    }
}

/// The text of the first `<tag>…</tag>` — enough XML for an S3 error `Code`.
fn xml_tag(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&format!("</{tag}>"))? + start;
    Some(body[start..end].to_string())
}

fn cors_xml(origins: &[String]) -> String {
    let origins: String = origins
        .iter()
        .map(|o| format!("<AllowedOrigin>{}</AllowedOrigin>", xml_escape(o)))
        .collect();
    format!(
        "<CORSConfiguration><CORSRule>{origins}<AllowedMethod>PUT</AllowedMethod><AllowedMethod>GET</AllowedMethod>\
         <AllowedMethod>HEAD</AllowedMethod><AllowedHeader>*</AllowedHeader><ExposeHeader>ETag</ExposeHeader>\
         <MaxAgeSeconds>3600</MaxAgeSeconds></CORSRule></CORSConfiguration>"
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `url` is http(s) and its `host:port` (port defaulted from the scheme) is
/// on `allow`, compared case-insensitively. An empty list allows nothing.
fn callback_allowed(url: &str, allow: &[String]) -> Result<(), String> {
    let u = reqwest::Url::parse(url).map_err(|_| format!("callback_url is not a URL: {url:?}"))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(format!("callback_url must be http(s): {url:?}"));
    }
    let (Some(host), Some(port)) = (u.host_str(), u.port_or_known_default()) else {
        return Err(format!("callback_url has no host: {url:?}"));
    };
    let authority = format!("{host}:{port}").to_ascii_lowercase();
    if allow.iter().any(|a| a.trim().eq_ignore_ascii_case(&authority)) {
        Ok(())
    } else {
        Err(format!("callback_url {authority} is not on --callback-allow"))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---- the daemon ----------------------------------------------------------------

struct Daemon {
    store: Store,
    nats: async_nats::Client,
    js: async_nats::jetstream::Context,
    max_upload: u64,
    sign_originals: bool,
    apple_helper: Option<PathBuf>,
    callback_secret: String,
    callback_allow: Vec<String>,
    work_dir: PathBuf,
}

impl Daemon {
    fn apple(&self) -> Option<&Path> {
        self.apple_helper.as_deref().filter(|p| p.is_file())
    }
}

type Shared = State<Arc<Daemon>>;

#[derive(Deserialize)]
struct StartUpload {
    photo_id: String,
    filename: String,
    size: u64,
    #[serde(default)]
    content_type: String,
}

async fn start_upload(State(d): Shared, Json(req): Json<StartUpload>) -> Json<Value> {
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
struct CompleteUpload {
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

async fn complete_upload(State(d): Shared, Json(req): Json<CompleteUpload>) -> Json<Value> {
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
struct AbortUpload {
    upload_id: String,
    key: String,
}

async fn abort_upload(State(d): Shared, Json(req): Json<AbortUpload>) -> Json<Value> {
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
struct Job {
    job_id: String,
    photo_id: String,
    key: String,
    callback_url: String,
}

async fn submit(State(d): Shared, Json(job): Json<Job>) -> Json<Value> {
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
struct SignReq {
    key: String,
    ttl_secs: u64,
}

async fn sign(State(d): Shared, Json(req): Json<SignReq>) -> Json<Value> {
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
        let ttl = Duration::from_secs(req.ttl_secs.clamp(1, MAX_SIGN_TTL));
        Ok(json!({ "url": d.store.sign_get(which, object, ttl) }))
    })())
}

async fn health(State(d): Shared) -> Json<Value> {
    let queue = d.nats.connection_state() == async_nats::connection::State::Connected;
    Json(
        json!({ "ok": true, "store": d.store.healthy().await, "queue": queue, "apple": d.apple().is_some() }),
    )
}

// ---- sharpness: green-stab-v1 ----------------------------------------------------

/// Per-tile Laplacian variance (x1e6) of `p`, on a `grid`×`grid` layout. The
/// last row and column of tiles take the remainder, and the one-pixel border
/// the Laplacian cannot reach is skipped — exactly as the Metal kernel does.
fn tile_variances(p: &[f32], w: usize, h: usize, grid: usize) -> Vec<f64> {
    let (tw, th) = ((w / grid).max(1), (h / grid).max(1));
    let mut acc = vec![(0f64, 0f64, 0f64); grid * grid];
    for y in 1..h.saturating_sub(1) {
        let ty = (y / th).min(grid - 1);
        for x in 1..w - 1 {
            let c = p[y * w + x];
            let l = (p[(y - 1) * w + x] + p[(y + 1) * w + x] + p[y * w + x - 1] + p[y * w + x + 1]
                - 4.0 * c) as f64;
            let t = &mut acc[ty * grid + (x / tw).min(grid - 1)];
            t.0 += l;
            t.1 += l * l;
            t.2 += 1.0;
        }
    }
    acc.iter()
        .map(|&(s, ss, n)| if n > 0.0 { (ss / n - (s / n).powi(2)) * 1e6 } else { 0.0 })
        .collect()
}

/// Separable [1 2 1]/4, leaving the border row/column as it was (the Metal
/// kernel matches this, border and all).
fn blur3(p: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut tmp = p.to_vec();
    for y in 0..h {
        for x in 1..w.saturating_sub(1) {
            let i = y * w + x;
            tmp[i] = (p[i - 1] + 2.0 * p[i] + p[i + 1]) * 0.25;
        }
    }
    let mut out = tmp.clone();
    for y in 1..h.saturating_sub(1) {
        for x in 0..w {
            let i = y * w + x;
            out[i] = (tmp[i - w] + 2.0 * tmp[i] + tmp[i + w]) * 0.25;
        }
    }
    out
}

/// `green-stab-v1` on a linear 0–1 plane, in the callback's `sharpness` shape
/// (minus `subjects`, which need Vision's faces).
fn green_stab_v1(plane: &[f32], w: usize, h: usize) -> Value {
    let stab: Vec<f32> = plane.iter().map(|v| v.max(0.0).sqrt()).collect();
    let tiles = tile_variances(&blur3(&stab, w, h), w, h, GRID);
    let (floor, peak) = floor_and_peak(&tiles);
    json!({
        "method": "green-stab-v1",
        "grid": GRID,
        "tiles": tiles.iter().map(|v| round(*v, 3)).collect::<Vec<_>>(),
        "floor": round(floor, 3),
        "peak": round(peak, 3),
        "focus_ratio": round(if floor > 0.0 { peak / floor } else { 0.0 }, 3),
        "subjects": [],
    })
}

/// The median tile — most of a frame is not the subject, so the middle of the
/// distribution is the noise — and the highest tile above it.
fn floor_and_peak(tiles: &[f64]) -> (f64, f64) {
    let mut sorted = tiles.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let floor = sorted[sorted.len() / 2];
    let peak = tiles.iter().map(|t| (t - floor).max(0.0)).fold(0.0, f64::max);
    (floor, peak)
}

fn round(v: f64, places: i32) -> f64 {
    let m = 10f64.powi(places);
    (v * m).round() / m
}

// ---- decode ----------------------------------------------------------------------

/// What the decode stage hands the rest of the job.
struct Decoded {
    metadata: Value,
    /// Half-resolution green plane, linear 0–1, cropped to the picture and
    /// turned the way the picture is displayed — so a tile's position means
    /// the same thing as a Vision box on the developed image.
    green: Vec<f32>,
    gw: usize,
    gh: usize,
}

/// EXIF orientation 1–8 from rawler's enum.
fn exif_orientation(o: rawler::decoders::Orientation) -> u8 {
    use rawler::decoders::Orientation::*;
    match o {
        Normal | Unknown => 1,
        HorizontalFlip => 2,
        Rotate180 => 3,
        VerticalFlip => 4,
        Transpose => 5,
        Rotate90 => 6,
        Transverse => 7,
        Rotate270 => 8,
    }
}

/// Apply EXIF orientation `o` to a `w`×`h` plane. Returns the new plane and
/// its dimensions (swapped for 5–8).
fn orient_plane(p: &[f32], w: usize, h: usize, o: u8) -> (Vec<f32>, usize, usize) {
    if o <= 1 || o > 8 {
        return (p.to_vec(), w, h);
    }
    let (ow, oh) = if o >= 5 { (h, w) } else { (w, h) };
    let mut out = vec![0f32; p.len()];
    for y in 0..oh {
        for x in 0..ow {
            let (sx, sy) = match o {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                _ => (w - 1 - y, x), // 8
            };
            out[y * ow + x] = p[sy * w + sx];
        }
    }
    (out, ow, oh)
}

fn rational(r: &Option<rawler::formats::tiff::Rational>) -> Value {
    match r {
        Some(r) if r.d != 0 => json!(round(r.n as f64 / r.d as f64, 6)),
        _ => Value::Null,
    }
}

/// `2026:09:23 13:11:47` -> `2026-09-23T13:11:47`: the camera clock, no zone.
/// Anything else — blank, all-spaces, a stray multi-byte character — is null.
fn exif_datetime(s: &str) -> Option<String> {
    let (date, time) = s.trim().split_once(' ')?;
    let time = time.get(..8)?;
    // `d` is a digit, anything else must match exactly.
    let shaped = |v: &str, pat: &str| {
        v.len() == pat.len()
            && v.bytes()
                .zip(pat.bytes())
                .all(|(b, p)| if p == b'd' { b.is_ascii_digit() } else { b == p })
    };
    let ok = shaped(date, "dddd:dd:dd") && shaped(time, "dd:dd:dd") && !date.starts_with("0000");
    ok.then(|| format!("{}T{}", date.replace(':', "-"), time))
}

/// Metadata and the green plane of a camera raw file.
fn decode_raw(path: &Path) -> Result<Decoded> {
    use rawler::rawimage::{RawImageData, RawPhotometricInterpretation};
    let params = rawler::decoders::RawDecodeParams::default();
    let src = rawler::rawsource::RawSource::new(path).context("open raw")?;
    let decoder = rawler::get_decoder(&src)?;
    let meta = decoder.raw_metadata(&src, &params)?;
    let raw = decoder.raw_image(&src, &params, false)?;
    let RawPhotometricInterpretation::Cfa(cfa) = &raw.photometric else {
        bail!("not a colour-filter-array raw: {:?}", raw.photometric);
    };
    let RawImageData::Integer(data) = &raw.data else {
        bail!("floating-point raw data is not handled");
    };
    let w = raw.width;
    let white = *raw.whitelevel.0.first().unwrap_or(&65535) as f32;
    let black = raw.blacklevel.levels.first().map(|r| r.as_f32()).unwrap_or(0.0);

    // Only the picture: the sensor's masked borders are black, and a black
    // edge is the sharpest thing in any frame.
    let area = raw.crop_area.or(raw.active_area);
    let (x0, y0, cw, ch) = match area {
        Some(r) => (r.p.x & !1, r.p.y & !1, r.d.w, r.d.h),
        None => (0, 0, w, raw.height),
    };
    let (gw, gh) = (cw / 2, ch / 2);
    let mut green = vec![0f32; gw * gh];
    let range = (white - black).max(1.0);
    for gy in 0..gh {
        for gx in 0..gw {
            let (mut sum, mut n) = (0f32, 0f32);
            for dy in 0..2 {
                for dx in 0..2 {
                    let (y, x) = (y0 + gy * 2 + dy, x0 + gx * 2 + dx);
                    if cfa.cfa.color_at(y, x) == 1 {
                        sum += data[y * w + x] as f32;
                        n += 1.0;
                    }
                }
            }
            green[gy * gw + gx] = ((sum / n.max(1.0) - black) / range).clamp(0.0, 1.0);
        }
    }
    let o = exif_orientation(raw.orientation);
    let (green, gw, gh) = orient_plane(&green, gw, gh, o);
    let (pw, ph) = if o >= 5 { (ch, cw) } else { (cw, ch) };

    let ex = &meta.exif;
    let camera = format!("{} {}", meta.make, meta.model).trim().to_string();
    let metadata = json!({
        "camera": camera,
        "lens": ex.lens_model.clone().or_else(|| meta.lens.as_ref().map(|l| l.lens_name.clone())),
        "captured_at": ex.date_time_original.as_deref().and_then(exif_datetime),
        "exposure_s": rational(&ex.exposure_time),
        "fnumber": rational(&ex.fnumber),
        "focal_mm": rational(&ex.focal_length),
        "iso": ex.iso_speed_ratings.map(u32::from).or(ex.iso_speed),
        "width": pw,
        "height": ph,
    });
    Ok(Decoded { metadata, green, gw, gh })
}

/// A JPEG original: its EXIF (when it has any) is the metadata, and the
/// "green plane" is the G channel, linearised from sRGB.
fn decode_jpeg(path: &Path) -> Result<Decoded> {
    let img = open_upright(path)?.to_rgb8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let green = img.pixels().map(|p| srgb_to_linear(p[1] as f32 / 255.0)).collect();
    let mut metadata = jpeg_metadata(path).unwrap_or_else(|e| {
        // No EXIF, or EXIF too broken to read: the picture is still a picture.
        eprintln!("comp-media: no readable EXIF in the JPEG ({e:#})");
        json!({
            "camera": null, "lens": null, "captured_at": null, "exposure_s": null,
            "fnumber": null, "focal_mm": null, "iso": null, "width": null, "height": null,
        })
    });
    metadata["width"] = json!(w);
    metadata["height"] = json!(h);
    Ok(Decoded { metadata, green, gw: w, gh: h })
}

/// The metadata fields a JPEG's EXIF gives, `width`/`height` left for the
/// caller (the upright decoded size, not whatever PixelXDimension claims).
/// The JPEG decoder that decodes the pixels also finds the APP1 segment.
fn jpeg_metadata(path: &Path) -> Result<Value> {
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::open(path)?.with_guessed_format()?.into_decoder()?;
    let chunk = decoder.exif_metadata()?.ok_or_else(|| anyhow!("no EXIF segment"))?;
    exif_chunk_metadata(&chunk)
}

/// Parse a raw EXIF chunk (a TIFF header and IFDs, optionally still behind
/// the `Exif\0\0` APP1 marker). kamadak-exif rather than rawler's TIFF
/// reader: rawler allocates whatever an entry's count claims before
/// reading it, and a 64 KB APP1 segment must not be able to ask for 32 GB.
/// A partly broken chunk still gives the fields that did parse.
fn exif_chunk_metadata(chunk: &[u8]) -> Result<Value> {
    use exif::{In, Tag};
    let tiff = chunk.strip_prefix(b"Exif\0\0").unwrap_or(chunk).to_vec();
    let ex = exif::Reader::new()
        .continue_on_error(true)
        .read_raw(tiff)
        .or_else(|e| e.distill_partial_result(|_| {}))
        .map_err(|e| anyhow!("EXIF: {e}"))?;
    let field = |t| ex.get_field(t, In::PRIMARY).map(|f| &f.value);
    let text = |t| match field(t) {
        Some(exif::Value::Ascii(v)) => {
            v.first().map(|s| String::from_utf8_lossy(s).trim().to_string())
        }
        _ => None,
    };
    let ratio = |t| match field(t) {
        Some(exif::Value::Rational(v)) => {
            v.first().filter(|r| r.denom != 0).map(|r| json!(round(r.to_f64(), 6)))
        }
        _ => None,
    };
    let uint = |t| field(t).and_then(|v| v.get_uint(0));
    let camera =
        compose_camera(&text(Tag::Make).unwrap_or_default(), &text(Tag::Model).unwrap_or_default());
    Ok(json!({
        "camera": camera,
        "lens": text(Tag::LensModel).filter(|s| !s.is_empty()),
        "captured_at": text(Tag::DateTimeOriginal).as_deref().and_then(exif_datetime),
        "exposure_s": ratio(Tag::ExposureTime),
        "fnumber": ratio(Tag::FNumber),
        "focal_mm": ratio(Tag::FocalLength),
        // ISOSpeedRatings, renamed PhotographicSensitivity in EXIF 2.3.
        "iso": uint(Tag::PhotographicSensitivity).or_else(|| uint(Tag::ISOSpeed)),
        "width": null,
        "height": null,
    }))
}

/// `Make` + `Model` the way the ARW path shows them: rawler's clean names
/// from its camera table when it knows the body ("SONY" + "ILCE-7RM5" is
/// "Sony ILCE-7RM5", as on the ARW), else the EXIF strings as written,
/// minus a repeated make ("Canon" + "Canon EOS R5" is "Canon EOS R5").
fn compose_camera(make: &str, model: &str) -> Option<String> {
    let (make, model) = (make.trim(), model.trim());
    let known = rawler::global_loader()
        .get_cameras()
        .iter()
        .filter(|((mk, md, _), _)| mk == make && md == model)
        .min_by_key(|((_, _, mode), _)| !mode.is_empty())
        .map(|(_, cam)| (cam.clean_make.as_str(), cam.clean_model.as_str()));
    let (make, model) = known.unwrap_or((make, model));
    let camera = if !make.is_empty() && model.to_lowercase().starts_with(&make.to_lowercase()) {
        model.to_string()
    } else {
        format!("{make} {model}").trim().to_string()
    };
    (!camera.is_empty()).then_some(camera)
}

/// Decode a JPEG and turn it the way its EXIF says — Core Image does the same
/// on the Apple path, and the plane and the renditions must agree with it.
fn open_upright(path: &Path) -> Result<DynamicImage> {
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::open(path)?.with_guessed_format()?.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    Ok(img)
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

// ---- develop (CPU) ---------------------------------------------------------------

/// (name, width, height, JPEG bytes).
type Rendition = (&'static str, u32, u32, Vec<u8>);

/// Long edge of each rendition.
const RENDITIONS: [(&str, u32); 3] = [("thumb", 512), ("share", 4096), ("ai", 1568)];

/// The developed picture, upright, and which developer made it.
fn develop_cpu(path: &Path, ext: &str) -> Result<(DynamicImage, &'static str)> {
    if ext != "arw" {
        return Ok((open_upright(path)?, "jpeg"));
    }
    let params = rawler::decoders::RawDecodeParams::default();
    let src = rawler::rawsource::RawSource::new(path)?;
    let decoder = rawler::get_decoder(&src)?;
    let raw = decoder.raw_image(&src, &params, false)?;
    let o = exif_orientation(raw.orientation);
    let developed = rawler::imgop::develop::RawDevelop::default()
        .develop_intermediate(&raw)
        .map_err(anyhow::Error::from)
        .and_then(|i| i.to_dynamic_image().ok_or_else(|| anyhow!("develop produced no image")));
    match developed {
        Ok(img) => {
            let mut img = DynamicImage::ImageRgb8(img.to_rgb8());
            if let Some(o) = image::metadata::Orientation::from_exif(o) {
                img.apply_orientation(o);
            }
            Ok((img, "rawler"))
        }
        Err(e) => {
            // The camera's own JPEG is a worse picture but a real one; say so.
            eprintln!("comp-media: rawler develop failed ({e}); using the embedded preview");
            let preview = decoder
                .preview_image(&src, &params)?
                .or(decoder.thumbnail_image(&src, &params)?)
                .ok_or_else(|| anyhow!("no embedded preview either"))?;
            Ok((preview, "embedded-preview"))
        }
    }
}

fn encode_jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let rgb = img.to_rgb8();
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality).write_image(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(out)
}

fn fit(img: &DynamicImage, long_edge: u32, filter: FilterType) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    if w.max(h) <= long_edge {
        return img.clone();
    }
    let s = long_edge as f64 / w.max(h) as f64;
    let (nw, nh) = (((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1));
    img.resize_exact(nw, nh, filter)
}

/// The three renditions as (name, width, height, jpeg bytes). The share copy
/// is shrunk from the full picture once and the others from it — resampling
/// 61 MP three times is the slow part of the CPU path.
fn renditions_cpu(full: &DynamicImage) -> Result<Vec<Rendition>> {
    let share = fit(full, 4096, FilterType::Triangle);
    let mut out = Vec::new();
    for (name, edge) in RENDITIONS {
        let img =
            if name == "share" { share.clone() } else { fit(&share, edge, FilterType::Lanczos3) };
        let mut bytes = encode_jpeg(&img, if name == "thumb" { 80 } else { 85 })?;
        // A busy high-ISO frame can blow past the share cap at q85.
        let mut q = 85;
        while name == "share" && bytes.len() >= SHARE_MAX_BYTES && q > 50 {
            q -= 10;
            bytes = encode_jpeg(&img, q)?;
        }
        out.push((name, img.width(), img.height(), bytes));
    }
    Ok(out)
}

/// Exposure at a glance, from the AI copy: mean luma, the share of pixels
/// crushed to black or blown to white, and mean HSV saturation.
fn colour(ai_jpeg: &[u8]) -> Result<Value> {
    let img = image::load_from_memory(ai_jpeg)?.to_rgb8();
    let n = (img.width() as f64 * img.height() as f64).max(1.0);
    let (mut luma, mut dark, mut bright, mut sat) = (0f64, 0f64, 0f64, 0f64);
    for p in img.pixels() {
        let (r, g, b) = (p[0] as f64 / 255.0, p[1] as f64 / 255.0, p[2] as f64 / 255.0);
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        luma += y;
        if y <= 2.0 / 255.0 {
            dark += 1.0;
        }
        if y >= 253.0 / 255.0 {
            bright += 1.0;
        }
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        if mx > 0.0 {
            sat += (mx - mn) / mx;
        }
    }
    Ok(json!({
        "mean_luma": round(luma / n, 4),
        "clipped_shadows_pct": round(dark / n * 100.0, 3),
        "clipped_highlights_pct": round(bright / n * 100.0, 3),
        "saturation_mean": round(sat / n, 4),
    }))
}

// ---- the Swift helper --------------------------------------------------------------

#[derive(Deserialize)]
struct HelperRendition {
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
struct HelperOut {
    #[serde(default)]
    develop: Option<String>,
    renditions: BTreeMap<String, HelperRendition>,
    #[serde(default)]
    sharpness: Option<Value>,
    #[serde(default)]
    vision: Option<Value>,
    #[serde(default)]
    timings_ms: BTreeMap<String, u64>,
}

async fn run_helper(
    helper: &Path,
    original: &Path,
    dec: &Decoded,
    dir: &Path,
) -> Result<HelperOut> {
    let green_path = dir.join("green.f32");
    let bytes: Vec<u8> = dec.green.iter().flat_map(|v| v.to_le_bytes()).collect();
    tokio::fs::write(&green_path, bytes).await?;
    let out = tokio::process::Command::new(helper)
        .arg("--original")
        .arg(original)
        .arg("--green")
        .arg(&green_path)
        .arg("--green-width")
        .arg(dec.gw.to_string())
        .arg("--green-height")
        .arg(dec.gh.to_string())
        .arg("--out")
        .arg(dir)
        .kill_on_drop(true)
        .output()
        .await?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.trim().is_empty() {
        eprintln!("comp-media-apple: {}", stderr.trim());
    }
    if !out.status.success() {
        bail!("helper exited {}", out.status);
    }
    serde_json::from_slice(&out.stdout).context("helper printed something that is not its JSON")
}

// ---- the worker ------------------------------------------------------------------

type HmacSha256 = Hmac<Sha256>;

/// `sha256=<hex>` over the raw body — the GitHub scheme `webhook:sign`
/// verifies with `scheme::github`.
fn signature(body: &[u8], secret: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC takes any key length");
    mac.update(body);
    format!("sha256={}", hex(&mac.finalize().into_bytes()))
}

fn ms(t: Instant) -> u64 {
    t.elapsed().as_millis() as u64
}

/// Everything from "the original is in the bucket" to "here is the result".
async fn evaluate(d: &Daemon, job: &Job, dir: &Path) -> Result<Value> {
    let (_, object) = parse_key(&job.key).ok_or_else(|| anyhow!("bad key {}", job.key))?;
    let ext = object.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default();
    let original = dir.join(format!("original.{ext}"));
    let mut timings = BTreeMap::new();

    let t = Instant::now();
    let (size, sha256) =
        d.store.download(Which::Originals, object, &original, d.max_upload).await?;
    timings.insert("download".to_string(), ms(t));
    eprintln!("comp-media: [{}] downloaded {size} bytes in {} ms", job.job_id, ms(t));

    let t = Instant::now();
    let (path, e) = (original.clone(), ext.clone());
    let mut dec = tokio::task::spawn_blocking(move || {
        if e == "arw" {
            decode_raw(&path)
        } else {
            decode_jpeg(&path)
        }
    })
    .await??;
    timings.insert("decode".to_string(), ms(t));
    eprintln!(
        "comp-media: [{}] decoded, green plane {}x{} in {} ms",
        job.job_id,
        dec.gw,
        dec.gh,
        ms(t)
    );

    // The Apple path first; any failure there falls back to the CPU path, so a
    // broken helper degrades the result instead of losing it.
    let mut helper_out = None;
    if let Some(helper) = d.apple() {
        let t = Instant::now();
        match run_helper(helper, &original, &dec, dir).await {
            Ok(h) => {
                eprintln!("comp-media: [{}] apple helper done in {} ms", job.job_id, ms(t));
                helper_out = Some(h);
            }
            Err(e) => eprintln!(
                "comp-media: [{}] apple helper failed ({e:#}); falling back to the CPU",
                job.job_id
            ),
        }
    }

    let mut renditions: Vec<Rendition> = Vec::new();
    let (develop, sharp_backend, sharpness, vision);
    match helper_out {
        Some(h) => {
            for (name, _) in RENDITIONS {
                let r = h
                    .renditions
                    .get(name)
                    .ok_or_else(|| anyhow!("helper wrote no {name} rendition"))?;
                let bytes = tokio::fs::read(dir.join(format!("{name}.jpg"))).await?;
                renditions.push((name, r.width, r.height, bytes));
            }
            develop = h.develop.unwrap_or_else(|| "coreimage".into());
            for (k, v) in h.timings_ms {
                timings.insert(k, v);
            }
            match h.sharpness {
                Some(s) => {
                    sharp_backend = "metal";
                    sharpness = s;
                }
                None => {
                    let t = Instant::now();
                    sharp_backend = "cpu";
                    sharpness = green_stab_v1(&dec.green, dec.gw, dec.gh);
                    timings.insert("sharpness".into(), ms(t));
                }
            }
            vision = h.vision.unwrap_or(Value::Null);
        }
        None => {
            let t = Instant::now();
            sharp_backend = "cpu";
            sharpness = green_stab_v1(&dec.green, dec.gw, dec.gh);
            timings.insert("sharpness".into(), ms(t));
            eprintln!("comp-media: [{}] sharpness (cpu) in {} ms", job.job_id, ms(t));

            let t = Instant::now();
            let (path, e) = (original.clone(), ext.clone());
            let (out, dev) = tokio::task::spawn_blocking(move || -> Result<_> {
                let (full, dev) = develop_cpu(&path, &e)?;
                Ok((renditions_cpu(&full)?, dev))
            })
            .await??;
            renditions = out;
            develop = dev.to_string();
            timings.insert("develop".into(), ms(t));
            eprintln!("comp-media: [{}] developed ({develop}) in {} ms", job.job_id, ms(t));
            vision = Value::Null;
        }
    }
    let metadata = std::mem::take(&mut dec.metadata);
    drop(dec);

    let ai = renditions.iter().find(|r| r.0 == "ai").map(|r| r.3.clone()).unwrap_or_default();
    let colour = tokio::task::spawn_blocking(move || colour(&ai)).await??;

    let t = Instant::now();
    let mut rendition_json = serde_json::Map::new();
    for (name, w, h, bytes) in renditions {
        let object = format!("{}/{name}.jpg", job.photo_id);
        let len = bytes.len();
        d.store.put(Which::Renditions, &object, bytes, "image/jpeg").await?;
        rendition_json.insert(
            name.to_string(),
            json!({ "key": format!("renditions/{object}"), "width": w, "height": h, "bytes": len }),
        );
    }
    timings.insert("upload".into(), ms(t));
    eprintln!("comp-media: [{}] renditions uploaded in {} ms", job.job_id, ms(t));

    Ok(json!({
        "job_id": job.job_id,
        "photo_id": job.photo_id,
        "status": "done",
        "error": null,
        "backend": { "sharpness": sharp_backend, "develop": develop, "vision": !vision.is_null() },
        "sha256": sha256,
        "metadata": metadata,
        "renditions": rendition_json,
        "sharpness": sharpness,
        "vision": vision,
        "colour": colour,
        "timings_ms": timings,
    }))
}

fn failed_body(job: &Job, error: &str) -> Value {
    json!({
        "job_id": job.job_id, "photo_id": job.photo_id, "status": "failed", "error": error,
        "backend": null, "sha256": null, "metadata": null, "renditions": null,
        "sharpness": null, "vision": null, "colour": null, "timings_ms": null,
    })
}

/// POST `body`, signed, retrying a non-2xx with backoff. True once accepted.
async fn deliver(
    http: &reqwest::Client,
    url: &str,
    body: &Value,
    secret: &str,
    attempts: u32,
) -> bool {
    let raw = serde_json::to_vec(body).unwrap_or_default();
    let sig = signature(&raw, secret);
    for attempt in 1..=attempts {
        let sent = http
            .post(url)
            .header("Content-Type", "application/json")
            .header("X-Media-Signature", &sig)
            .timeout(Duration::from_secs(30))
            .body(raw.clone())
            .send()
            .await;
        match sent {
            Ok(r) if r.status().is_success() => return true,
            Ok(r) => eprintln!(
                "comp-media: callback {url} answered {} (attempt {attempt}/{attempts})",
                r.status()
            ),
            Err(e) => {
                eprintln!("comp-media: callback {url} failed: {e} (attempt {attempt}/{attempts})")
            }
        }
        if attempt < attempts {
            tokio::time::sleep(Duration::from_secs(1 << (attempt - 1))).await;
        }
    }
    false
}

async fn run_job(d: &Daemon, job: &Job) {
    let started = Instant::now();
    let dir = d.work_dir.join(&job.job_id);
    let result = async {
        tokio::fs::create_dir_all(&dir).await?;
        evaluate(d, job, &dir).await
    }
    .await;
    // Always, however the job went.
    let _ = tokio::fs::remove_dir_all(&dir).await;

    let body = match result {
        Ok(v) => v,
        Err(e) => {
            eprintln!("comp-media: [{}] failed: {e:#}", job.job_id);
            failed_body(job, &format!("{e:#}"))
        }
    };
    let status = body["status"].as_str().unwrap_or("?").to_string();
    let ok =
        deliver(&d.store.http, &job.callback_url, &body, &d.callback_secret, CALLBACK_ATTEMPTS)
            .await;
    if !ok && status == "done" {
        // The contract: after the last attempt, a final `failed` — best effort,
        // to the same receiver, so a receiver that recovers learns the job
        // will not be retried.
        let fb =
            failed_body(job, &format!("callback not accepted after {CALLBACK_ATTEMPTS} attempts"));
        deliver(&d.store.http, &job.callback_url, &fb, &d.callback_secret, 1).await;
    }
    eprintln!(
        "comp-media: [{}] {status}, callback {} — {} ms total",
        job.job_id,
        if ok { "accepted" } else { "NOT accepted" },
        ms(started)
    );
}

async fn worker(d: Arc<Daemon>, consumer: async_nats::jetstream::consumer::PullConsumer) {
    use futures::StreamExt;
    loop {
        let batch =
            consumer.batch().max_messages(1).expires(Duration::from_secs(30)).messages().await;
        let mut batch = match batch {
            Ok(b) => b,
            Err(e) => {
                eprintln!("comp-media: pull failed: {e}; retrying");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        while let Some(msg) = batch.next().await {
            let msg = match msg {
                Ok(m) => Arc::new(m),
                Err(e) => {
                    eprintln!("comp-media: message error: {e}");
                    continue;
                }
            };
            let job: Job = match serde_json::from_slice(&msg.payload) {
                Ok(j) => j,
                Err(e) => {
                    // Not a job and never will be: ack it away rather than
                    // redeliver garbage forever.
                    eprintln!("comp-media: dropping an unreadable job: {e}");
                    let _ = msg.ack().await;
                    continue;
                }
            };
            // `/jobs` checked these, but anything with NATS access can publish
            // to the subject, and a job_id becomes a directory name.
            if !valid_id(&job.job_id)
                || !matches!(parse_key(&job.key), Some((Which::Originals, _)))
                || callback_allowed(&job.callback_url, &d.callback_allow).is_err()
            {
                eprintln!(
                    "comp-media: dropping a job with a bad id, key or callback: {:?} {:?} {:?}",
                    job.job_id, job.key, job.callback_url
                );
                let _ = msg.ack().await;
                continue;
            }
            eprintln!("comp-media: [{}] started ({})", job.job_id, job.key);
            let progress = {
                let msg = msg.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_secs(20)).await;
                        let _ = msg.ack_with(async_nats::jetstream::AckKind::Progress).await;
                    }
                })
            };
            run_job(&d, &job).await;
            progress.abort();
            if let Err(e) = msg.double_ack().await {
                eprintln!("comp-media: [{}] ack failed: {e}", job.job_id);
            }
        }
    }
}

// ---- startup ---------------------------------------------------------------------

/// A secret from `--x-file` (wins) or `--x`.
fn secret(direct: Option<String>, file: Option<PathBuf>, name: &str) -> Result<String> {
    if let Some(path) = file {
        return Ok(std::fs::read_to_string(&path)
            .with_context(|| format!("--{name}-file {}", path.display()))?
            .trim()
            .to_string());
    }
    direct.ok_or_else(|| anyhow!("--{name} or --{name}-file is required"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token =
        comp_reconciler::daemon_auth::resolve_token(args.token.clone(), args.token_file.clone());
    comp_reconciler::daemon_auth::warn_if_unauthenticated("comp-media", &token);

    let creds = Credentials::new(
        secret(args.s3_access_key, args.s3_access_key_file, "s3-access-key")?,
        secret(args.s3_secret_key, args.s3_secret_key_file, "s3-secret-key")?,
    );
    let callback_secret =
        secret(args.callback_secret, args.callback_secret_file, "callback-secret")?;
    if args.callback_allow.is_empty() {
        eprintln!(
            "comp-media: WARNING — no --callback-allow given: EVERY job will be refused. \
             Pass --callback-allow <host:port> for each app that may receive results."
        );
    } else {
        eprintln!("comp-media: callbacks allowed to {:?}", args.callback_allow);
    }
    let endpoint: reqwest::Url = args.s3_endpoint.parse().context("--s3-endpoint")?;
    let public: reqwest::Url = match &args.s3_public_endpoint {
        Some(p) => p.parse().context("--s3-public-endpoint")?,
        None => endpoint.clone(),
    };
    let style = if args.path_style { UrlStyle::Path } else { UrlStyle::VirtualHost };
    let region = args.s3_region.clone();
    let store = Store {
        http: reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build()?,
        originals: Bucket::new(
            endpoint.clone(),
            style,
            args.bucket_originals.clone(),
            region.clone(),
        )?,
        renditions: Bucket::new(endpoint, style, args.bucket_renditions.clone(), region.clone())?,
        public_originals: Bucket::new(
            public.clone(),
            style,
            args.bucket_originals.clone(),
            region.clone(),
        )?,
        public_renditions: Bucket::new(public, style, args.bucket_renditions.clone(), region)?,
        creds,
    };
    store.ensure_bucket(Which::Originals).await?;
    store.ensure_bucket(Which::Renditions).await?;
    if !args.cors_origin.is_empty() {
        for w in [Which::Originals, Which::Renditions] {
            match store.put_cors(w, &args.cors_origin).await {
                Ok(()) => eprintln!("comp-media: bucket CORS set on {} for {:?}", store.bucket(w).name(), args.cors_origin),
                Err(e) => eprintln!(
                    "comp-media: PutBucketCors on {} refused ({e}). RustFS answers CORS per listener: \
                     set RUSTFS_CORS_ALLOWED_ORIGINS={} on the store instead.",
                    store.bucket(w).name(),
                    args.cors_origin.join(",")
                ),
            }
        }
    }

    let nats = async_nats::connect(&args.nats_url)
        .await
        .with_context(|| format!("connect {}", args.nats_url))?;
    let js = async_nats::jetstream::new(nats.clone());
    let stream = js
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: STREAM.into(),
            subjects: vec![SUBJECT.into()],
            retention: async_nats::jetstream::stream::RetentionPolicy::WorkQueue,
            duplicate_window: DEDUPE_WINDOW,
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow!("stream {STREAM}: {e}"))?;
    let consumer: async_nats::jetstream::consumer::PullConsumer = stream
        .get_or_create_consumer(
            CONSUMER,
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some(CONSUMER.into()),
                ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                ack_wait: ACK_WAIT,
                max_deliver: 3,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| anyhow!("consumer {CONSUMER}: {e}"))?;

    let work_dir = args.work_dir.unwrap_or_else(|| std::env::temp_dir().join("comp-media")); // nosemgrep: rust.lang.security.temp-dir.temp-dir
    std::fs::create_dir_all(&work_dir)?;
    let d = Arc::new(Daemon {
        store,
        nats,
        js,
        max_upload: args.max_upload_mb * 1024 * 1024,
        sign_originals: args.sign_originals,
        apple_helper: args.apple_helper,
        callback_secret,
        callback_allow: args.callback_allow,
        work_dir,
    });
    println!(
        "comp-media: listening on http://{} | store {} (browsers: {}) | queue {} | apple helper {}",
        args.addr,
        args.s3_endpoint,
        args.s3_public_endpoint.as_deref().unwrap_or(&args.s3_endpoint),
        args.nats_url,
        d.apple().map(|p| p.display().to_string()).unwrap_or_else(|| "none (CPU path)".into())
    );
    tokio::spawn(worker(d.clone(), consumer));

    let app = Router::new()
        .route("/uploads", post(start_upload))
        .route("/uploads/complete", post(complete_upload))
        .route("/uploads/abort", post(abort_upload))
        .route("/jobs", post(submit))
        .route("/sign", post(sign))
        .with_state(d.clone())
        .layer(axum::middleware::from_fn(comp_reconciler::daemon_auth::require_token))
        .layer(axum::Extension(Arc::new(token)))
        // Outside the auth layer: a liveness probe carries no token.
        .merge(Router::new().route("/health", get(health)).with_state(d));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `e2e/photoquest.sh` relies on `MEDIA_NATS_URL` reaching the daemon
    /// through `cargo xtask host` (which inherits its environment); pin the
    /// env name here rather than mutating the process env in a test.
    #[test]
    fn nats_url_reads_media_nats_url() {
        use clap::CommandFactory;
        let cmd = Args::command();
        let arg = cmd.get_arguments().find(|a| a.get_id() == "nats_url").unwrap();
        assert_eq!(arg.get_env().and_then(|e| e.to_str()), Some("MEDIA_NATS_URL"));
        let args = Args::try_parse_from([
            "comp-media",
            "--s3-endpoint",
            "http://127.0.0.1:9000",
            "--nats-url",
            "nats://127.0.0.1:4999",
        ])
        .unwrap();
        assert_eq!(args.nats_url, "nats://127.0.0.1:4999");
    }

    /// A tiny deterministic noise source — the test must not depend on a
    /// crate's RNG, and must give the same plane every run.
    fn noise(seed: &mut u64) -> f32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed % 10_000) as f32 / 10_000.0 - 0.5
    }

    /// 256×256: a vertical edge at x=128 in the middle tile rows, sharp or
    /// blurred, on a mid-grey with shot-like noise everywhere.
    fn plane(edge: Option<usize>, noise_amp: f32) -> Vec<f32> {
        let (w, h) = (256usize, 256usize);
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut p = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let base = match edge {
                    Some(0) => {
                        if x < 128 {
                            0.2
                        } else {
                            0.8
                        }
                    }
                    // A ramp `width` pixels wide: an out-of-focus version.
                    Some(width) => {
                        let t = ((x as f32 - 128.0) / width as f32 + 0.5).clamp(0.0, 1.0);
                        0.2 + 0.6 * t
                    }
                    None => 0.5,
                };
                p[y * w + x] = (base + noise_amp * noise(&mut seed) * base.sqrt()).clamp(0.0, 1.0);
            }
        }
        p
    }

    fn peak(v: &Value) -> f64 {
        v["peak"].as_f64().unwrap()
    }

    /// A sharp edge beats a blurred one at the same noise, and a frame of
    /// pure noise — however loud — has no focus to speak of: its best tile is
    /// barely above its own floor.
    #[test]
    fn green_stab_v1_ranks_a_sharp_edge_over_a_blurred_one_and_noise_has_no_focus() {
        let sharp = green_stab_v1(&plane(Some(0), 0.02), 256, 256);
        let blurred = green_stab_v1(&plane(Some(24), 0.02), 256, 256);
        let loud_noise = green_stab_v1(&plane(None, 0.2), 256, 256);
        assert!(
            peak(&sharp) > 3.0 * peak(&blurred),
            "sharp {} vs blurred {}",
            peak(&sharp),
            peak(&blurred)
        );
        let ratio = |v: &Value| v["focus_ratio"].as_f64().unwrap();
        assert!(ratio(&loud_noise) < 1.0, "noise focus ratio {}", ratio(&loud_noise));
        assert!(
            ratio(&sharp) > 10.0 * ratio(&loud_noise),
            "sharp {} vs noise {}",
            ratio(&sharp),
            ratio(&loud_noise)
        );
        assert_eq!(sharp["tiles"].as_array().unwrap().len(), GRID * GRID);
        assert_eq!(sharp["method"], "green-stab-v1");
    }

    /// The finding that made the metric: at high ISO a bright, flat,
    /// out-of-focus area (a white shirt) has the most absolute noise in the
    /// frame, and a raw Laplacian variance calls it the sharpest thing there.
    /// After the square root the shirt is as quiet as everything else and the
    /// real edge wins.
    #[test]
    fn a_bright_noisy_flat_patch_does_not_beat_a_real_edge() {
        let (w, h) = (256usize, 256usize);
        let mut seed = 42u64;
        let mut p = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                // The shirt: top-left, bright and flat, its outline as soft as
                // anything out of focus is (a 48 px smoothstep, not a step).
                let inside = (96.0 - x.max(y) as f32) / 48.0 + 0.5;
                let shirt = inside.clamp(0.0, 1.0);
                let shirt = shirt * shirt * (3.0 - 2.0 * shirt);
                let base: f32 = if x < 120 && y < 120 {
                    0.1 + 0.8 * shirt
                } else if y >= 160 && x >= 160 {
                    if x < 208 {
                        0.08
                    } else {
                        0.2
                    } // a dim, sharp edge bottom-right
                } else {
                    0.1
                };
                p[y * w + x] = (base + 0.08 * noise(&mut seed) * base.sqrt()).clamp(0.0, 1.0);
            }
        }
        let best = |tiles: &[f64]| {
            let i = (0..tiles.len()).max_by(|a, b| tiles[*a].total_cmp(&tiles[*b])).unwrap();
            (i % GRID, i / GRID)
        };
        let raw = tile_variances(&p, w, h, GRID);
        let (rx, ry) = best(&raw);
        assert!(rx < 5 && ry < 5, "the raw metric should fall for the shirt, picked ({rx},{ry})");
        let v = green_stab_v1(&p, w, h);
        let tiles: Vec<f64> =
            v["tiles"].as_array().unwrap().iter().map(|t| t.as_f64().unwrap()).collect();
        assert_eq!(
            best(&tiles).0,
            13,
            "green-stab-v1 must pick the edge column, got {:?}",
            best(&tiles)
        );
    }

    /// The sharp tiles are where the edge is: columns 7 and 8 of 16.
    #[test]
    fn the_peak_is_in_the_tiles_under_the_edge() {
        let v = green_stab_v1(&plane(Some(0), 0.02), 256, 256);
        let tiles: Vec<f64> =
            v["tiles"].as_array().unwrap().iter().map(|t| t.as_f64().unwrap()).collect();
        let best = (0..tiles.len()).max_by(|a, b| tiles[*a].total_cmp(&tiles[*b])).unwrap();
        assert!([7, 8].contains(&(best % GRID)), "best tile column {}", best % GRID);
    }

    /// Orientation 6 turns the plane a quarter clockwise: the source's
    /// bottom-left pixel becomes the top-left.
    #[test]
    fn orienting_a_plane_moves_pixels_the_way_exif_says() {
        // 3 wide, 2 high:  a b c / d e f
        let p = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        assert_eq!(orient_plane(&p, 3, 2, 6), (vec![4.0, 1.0, 5.0, 2.0, 6.0, 3.0], 2, 3));
        assert_eq!(orient_plane(&p, 3, 2, 8), (vec![3.0, 6.0, 2.0, 5.0, 1.0, 4.0], 2, 3));
        assert_eq!(orient_plane(&p, 3, 2, 3).0, vec![6.0, 5.0, 4.0, 3.0, 2.0, 1.0]);
        assert_eq!(orient_plane(&p, 3, 2, 1).0, p.to_vec());
    }

    #[test]
    fn ids_are_the_contracts_character_class_and_nothing_else() {
        assert!(valid_id("photo_01-A"));
        assert!(valid_id(&"x".repeat(64)));
        assert!(!valid_id(&"x".repeat(65)));
        assert!(!valid_id(""));
        for bad in ["../etc", "a/b", "a.b", "a b", "é", "a\0b"] {
            assert!(!valid_id(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    fn only_keys_this_daemon_could_have_made_parse() {
        assert_eq!(parse_key("originals/p1.arw"), Some((Which::Originals, "p1.arw")));
        assert_eq!(parse_key("renditions/p1/share.jpg"), Some((Which::Renditions, "p1/share.jpg")));
        for bad in [
            "originals/../x.arw",
            "originals/p1.exe",
            "originals/p1",
            "originals/a/b.arw",
            "renditions/p1/other.jpg",
            "renditions/../p1/thumb.jpg",
            "renditions/p1/thumb.jpg/x",
            "elsewhere/p1.arw",
            "p1.arw",
        ] {
            assert_eq!(parse_key(bad), None, "{bad:?} must not parse");
        }
    }

    #[test]
    fn the_extension_is_lower_cased_and_allow_listed() {
        assert_eq!(allowed_ext("SZI02833.ARW").as_deref(), Some("arw"));
        assert_eq!(allowed_ext("a.JPEG").as_deref(), Some("jpeg"));
        assert_eq!(allowed_ext("a.png"), None);
        assert_eq!(allowed_ext("noext"), None);
    }

    /// The real file: 129,581,056 bytes is seven full 16 MiB parts and a
    /// shorter eighth, and the lengths add back up to the size.
    #[test]
    fn parts_are_planned_with_a_short_last_one() {
        let parts = plan_parts(129_581_056, PART_SIZE);
        assert_eq!(parts.len(), 8);
        assert_eq!(parts[0], (1, PART_SIZE));
        assert_eq!(parts[7].0, 8);
        assert_eq!(parts.iter().map(|p| p.1).sum::<u64>(), 129_581_056);
        assert_eq!(plan_parts(PART_SIZE, PART_SIZE), vec![(1, PART_SIZE)]);
        assert_eq!(plan_parts(1, PART_SIZE), vec![(1, 1)]);
        assert_eq!(plan_parts(PART_SIZE + 1, PART_SIZE), vec![(1, PART_SIZE), (2, 1)]);
    }

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

    /// The callback signature is `webhook:sign`'s GitHub scheme: `sha256=`
    /// and the lower-case hex HMAC-SHA256 of the raw body. The expected value
    /// is RFC 4231 test case 2 ("Jefe" / "what do ya want for nothing?"), so
    /// this pins the scheme, not just self-consistency.
    #[test]
    fn the_callback_signature_is_the_github_scheme() {
        assert_eq!(
            signature(b"what do ya want for nothing?", "Jefe"),
            "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    /// Only an allow-listed `host:port` is a callback target; the port comes
    /// from the scheme when absent, and an empty list refuses everything.
    #[test]
    fn a_callback_must_be_on_the_allow_list() {
        let allow = vec!["127.0.0.1:3941".to_string(), "App.Example.com:443".to_string()];
        assert!(
            callback_allowed("http://127.0.0.1:3941/internal/photos/x/evaluated", &allow).is_ok()
        );
        assert!(callback_allowed("https://app.example.com/cb", &allow).is_ok());
        for bad in [
            "http://127.0.0.1:3942/cb",
            "http://169.254.169.254/latest/meta-data",
            "http://localhost:3941/cb",
            "http://app.example.com/cb", // port 80, not 443
            "ftp://127.0.0.1:3941/cb",
            "not a url",
        ] {
            assert!(callback_allowed(bad, &allow).is_err(), "{bad:?} must be refused");
        }
        assert!(callback_allowed("http://127.0.0.1:3941/cb", &[]).is_err());
    }

    /// Browsers get URLs signed for the public endpoint; the daemon's own
    /// calls use the internal one.
    #[test]
    fn browser_urls_are_signed_against_the_public_endpoint() {
        let b = |e: &str, n: &str| {
            Bucket::new(e.parse().unwrap(), UrlStyle::Path, n.to_string(), "us-east-1".to_string())
                .unwrap()
        };
        let store = Store {
            http: reqwest::Client::new(),
            creds: Credentials::new("k", "s"),
            originals: b("http://rustfs:9000", "originals"),
            renditions: b("http://rustfs:9000", "renditions"),
            public_originals: b("https://s3.example.com", "originals"),
            public_renditions: b("https://s3.example.com", "renditions"),
        };
        let part = store.part_url("p1.arw", 1, "u1");
        assert!(part.starts_with("https://s3.example.com/originals/p1.arw?"), "{part}");
        let get = store.sign_get(Which::Renditions, "p1/share.jpg", Duration::from_secs(60));
        assert!(get.starts_with("https://s3.example.com/renditions/p1/share.jpg?"), "{get}");
        assert!(store
            .bucket(Which::Originals)
            .base_url()
            .as_str()
            .starts_with("http://rustfs:9000"));
    }

    #[test]
    fn exif_dates_become_iso_without_a_zone() {
        assert_eq!(exif_datetime("2026:09:23 13:11:47").as_deref(), Some("2026-09-23T13:11:47"));
        assert_eq!(exif_datetime("garbage"), None);
    }

    #[test]
    fn exif_datetime_rejects_what_is_not_a_camera_clock() {
        assert_eq!(
            exif_datetime("2026:09:23 13:11:47.123").as_deref(),
            Some("2026-09-23T13:11:47")
        );
        assert_eq!(exif_datetime("    :  :     :  :  "), None);
        assert_eq!(exif_datetime("0000:00:00 00:00:00"), None);
        assert_eq!(exif_datetime("2026:09:23 13:11:4\u{e9}"), None);
        assert_eq!(exif_datetime("2026-09-23 13:11:47"), None);
        assert_eq!(exif_datetime(""), None);
    }

    /// A TIFF value for the test EXIF writer.
    enum V {
        A(&'static str),
        R(u32, u32),
        S(u16),
    }

    /// (type, count, bytes) as TIFF stores them, little-endian.
    fn tiff_value(v: &V) -> (u16, u32, Vec<u8>) {
        match v {
            V::A(s) => (2, s.len() as u32 + 1, [s.as_bytes(), &[0]].concat()),
            V::R(n, d) => (5, 1, [n.to_le_bytes(), d.to_le_bytes()].concat()),
            V::S(x) => (3, 1, x.to_le_bytes().to_vec()),
        }
    }

    fn tiff_ifd(out: &mut Vec<u8>, data: &mut Vec<u8>, data_base: usize, entries: &[(u16, V)]) {
        out.extend((entries.len() as u16).to_le_bytes());
        for (tag, v) in entries {
            let (ty, count, bytes) = tiff_value(v);
            out.extend(tag.to_le_bytes());
            out.extend(ty.to_le_bytes());
            out.extend(count.to_le_bytes());
            if bytes.len() <= 4 {
                let mut inline = bytes.clone();
                inline.resize(4, 0);
                out.extend(inline);
            } else {
                out.extend(((data_base + data.len()) as u32).to_le_bytes());
                data.extend(&bytes);
                if data.len() % 2 == 1 {
                    data.push(0);
                }
            }
        }
        out.extend(0u32.to_le_bytes());
    }

    /// A minimal EXIF chunk: IFD0 (tags sorted, the Exif IFD pointer
    /// appended) and an Exif sub-IFD (tags sorted).
    fn tiff(ifd0: Vec<(u16, V)>, exif: Vec<(u16, V)>) -> Vec<u8> {
        let exif_at = 8 + 2 + 12 * (ifd0.len() + 1) + 4;
        let data_at = exif_at + 2 + 12 * exif.len() + 4;
        let mut ifd0 = ifd0;
        ifd0.push((0x8769, V::S(0))); // placeholder, patched below
        let mut out = b"II\x2a\x00".to_vec();
        out.extend(8u32.to_le_bytes());
        let mut data = Vec::new();
        tiff_ifd(&mut out, &mut data, data_at, &ifd0);
        // The pointer is a LONG holding the Exif IFD's offset.
        let ptr = 8 + 2 + 12 * (ifd0.len() - 1);
        out[ptr + 2..ptr + 4].copy_from_slice(&4u16.to_le_bytes());
        out[ptr + 8..ptr + 12].copy_from_slice(&(exif_at as u32).to_le_bytes());
        tiff_ifd(&mut out, &mut data, data_at, &exif);
        assert_eq!(out.len(), data_at);
        out.extend(data);
        out
    }

    /// A 16x8 JPEG on disk, with `exif` as its APP1 segment when given.
    /// A uniquely named temp file (created securely by `tempfile`, not a
    /// predictable name in the shared temp dir), deleted when the path drops.
    fn jpeg_file(name: &str, exif: Option<Vec<u8>>) -> tempfile::TempPath {
        let img = image::RgbImage::from_fn(16, 8, |x, y| {
            image::Rgb([(x * 16) as u8, (y * 32) as u8, 90])
        });
        let mut bytes = Vec::new();
        let mut enc = JpegEncoder::new_with_quality(&mut bytes, 90);
        if let Some(exif) = exif {
            enc.set_exif_metadata(exif).unwrap();
        }
        enc.write_image(img.as_raw(), 16, 8, image::ExtendedColorType::Rgb8).unwrap();
        let mut file = tempfile::Builder::new()
            .prefix(&format!("comp-media-test-{name}-"))
            .suffix(".jpg")
            .tempfile()
            .unwrap();
        std::io::Write::write_all(&mut file, &bytes).unwrap();
        file.into_temp_path()
    }

    #[test]
    fn a_jpeg_originals_exif_is_its_metadata() {
        let exif = tiff(
            vec![(0x010F, V::A("SONY")), (0x0110, V::A("ILCE-7RM5"))],
            vec![
                (0x829A, V::R(1, 250)),
                (0x829D, V::R(28, 10)),
                (0x8827, V::S(400)),
                (0x9003, V::A("2026:05:01 09:30:15")),
                (0x920A, V::R(500, 10)),
                (0xA434, V::A("FE 24-70mm F2.8 GM II")),
            ],
        );
        let path = jpeg_file("exif", Some(exif));
        let m = decode_jpeg(&path).unwrap().metadata;
        std::fs::remove_file(&path).ok();
        assert_eq!(
            m,
            json!({
                "camera": "Sony ILCE-7RM5", "lens": "FE 24-70mm F2.8 GM II",
                "captured_at": "2026-05-01T09:30:15", "exposure_s": 0.004, "fnumber": 2.8,
                "focal_mm": 50.0, "iso": 400, "width": 16, "height": 8,
            })
        );
    }

    #[test]
    fn a_missing_exif_tag_is_null_on_its_own() {
        // An unknown body keeps its EXIF strings; a model repeating the make
        // does not say it twice; only the tags present are filled.
        let exif = tiff(
            vec![(0x010F, V::A("Acme")), (0x0110, V::A("Acme Box 1")), (0x0112, V::S(6))],
            vec![(0x829D, V::R(8, 1))],
        );
        let path = jpeg_file("partial", Some(exif));
        let m = decode_jpeg(&path).unwrap().metadata;
        std::fs::remove_file(&path).ok();
        assert_eq!(
            m,
            json!({
                "camera": "Acme Box 1", "lens": null, "captured_at": null, "exposure_s": null,
                "fnumber": 8.0, "focal_mm": null, "iso": null,
                // Orientation 6: the upright picture is 8 wide.
                "width": 8, "height": 16,
            })
        );
    }

    #[test]
    fn a_jpeg_without_exif_has_only_its_size() {
        let path = jpeg_file("plain", None);
        let m = decode_jpeg(&path).unwrap().metadata;
        std::fs::remove_file(&path).ok();
        assert_eq!(
            m,
            json!({
                "camera": null, "lens": null, "captured_at": null, "exposure_s": null,
                "fnumber": null, "focal_mm": null, "iso": null, "width": 16, "height": 8,
            })
        );
    }

    #[test]
    fn broken_exif_is_no_metadata_not_a_failed_job() {
        let all_null = |v: &Value| v.as_object().unwrap().values().all(Value::is_null);
        for chunk in [&b"Exif\0\0II\x2a\x00\xff\xff\xff\x7f"[..], b"", b"not a tiff at all"] {
            if let Ok(v) = exif_chunk_metadata(chunk) {
                assert!(all_null(&v), "{v}");
            }
        }
        let mut truncated =
            tiff(vec![(0x010F, V::A("SONY"))], vec![(0x9003, V::A("2026:05:01 09:30:15"))]);
        truncated.truncate(truncated.len() - 12);
        let _ = exif_chunk_metadata(&truncated); // must not panic

        // An entry claiming four billion rationals is dropped without asking
        // for 32 GB; the fields around it still read.
        let mut huge = tiff(
            vec![(0x010F, V::A("Acme")), (0x0110, V::A("Box"))],
            vec![(0x829A, V::R(1, 250)), (0x829D, V::R(4, 1))],
        );
        let exif_at = 8 + 2 + 12 * 3 + 4;
        huge[exif_at + 2 + 4..exif_at + 2 + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        let v = exif_chunk_metadata(&huge).unwrap();
        assert_eq!(
            (v["camera"].as_str(), v["exposure_s"].is_null(), v["fnumber"].as_f64()),
            (Some("Acme Box"), true, Some(4.0))
        );
    }

    #[test]
    fn s3_error_codes_are_read_from_the_xml() {
        let body =
            "<?xml version=\"1.0\"?><Error><Code>NoSuchUpload</Code><Message>x</Message></Error>";
        assert_eq!(xml_tag(body, "Code").as_deref(), Some("NoSuchUpload"));
        assert_eq!(xml_tag("<a/>", "Code"), None);
    }

    #[test]
    fn colour_of_a_flat_grey_picture() {
        let img =
            DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([128, 128, 128])));
        let c = colour(&encode_jpeg(&img, 95).unwrap()).unwrap();
        assert!((c["mean_luma"].as_f64().unwrap() - 0.502).abs() < 0.01);
        assert_eq!(c["clipped_highlights_pct"].as_f64().unwrap(), 0.0);
        assert!(c["saturation_mean"].as_f64().unwrap() < 0.02);
    }
}
