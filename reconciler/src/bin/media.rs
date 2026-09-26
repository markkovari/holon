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
//! - **Signs**, never carries ([`routes`]). `POST /uploads` plans a presigned
//!   multipart upload; the browser `PUT`s each part to the store and reports
//!   the ETags; `POST /uploads/complete` stitches them. `POST /sign` hands out
//!   a time-limited GET for a rendition. rusty-s3 is sans-IO ([`store`]): it
//!   computes the SigV4 URL and the reqwest client sends it, so there is no S3
//!   SDK here.
//! - **Queues** ([`routes::submit`]). `POST /jobs` publishes to the JetStream
//!   work-queue stream `MEDIA_JOBS` with `Nats-Msg-Id: <job_id>`, so a double
//!   submit is one job. A guest cannot publish to NATS (comp-host offers no
//!   messaging), which is why the queue is the daemon's.
//! - **Evaluates** ([`job_worker`]), one photo at a time — the GPU is the
//!   bottleneck, not the queue. Download to a temp file (hashing as it
//!   streams), decode with `rawler` ([`decode`]), take the half-resolution
//!   green plane straight from the Bayer data, then either run the Swift
//!   helper ([`apple_helper`]: Core Image develop, Metal sharpness, Vision) or
//!   do it on the CPU ([`develop`]: `rawler`'s own developer, the same
//!   sharpness metric in Rust ([`sharpness`]), no Vision). Upload three
//!   renditions, then POST a signed result to the job's callback.
//!
//! ## Keys
//!
//! A key's first segment names the bucket and the rest is the object inside
//! it: `originals/<photo_id>.arw` is object `<photo_id>.arw` in
//! `--bucket-originals`. Keys are built here from a validated `photo_id` —
//! nothing a caller sends is spliced into a key unchecked ([`keys`]).
//!
//!   comp-media --addr 127.0.0.1:8013 --token-file /run/credentials/media-token \
//!     --s3-endpoint http://127.0.0.1:9000 --s3-access-key-file ... --s3-secret-key-file ... \
//!     --nats-url nats://127.0.0.1:4222 --callback-secret-file ... \
//!     --callback-allow 127.0.0.1:3941 --apple-helper /usr/local/bin/comp-media-apple

#[path = "media/apple_helper.rs"]
mod apple_helper;
#[path = "media/decode.rs"]
mod decode;
#[path = "media/develop.rs"]
mod develop;
#[path = "media/errors.rs"]
mod errors;
#[path = "media/job_worker.rs"]
mod job_worker;
#[path = "media/keys.rs"]
mod keys;
#[path = "media/routes.rs"]
mod routes;
#[path = "media/sharpness.rs"]
mod sharpness;
#[path = "media/store.rs"]
mod store;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use rusty_s3::{Bucket, Credentials, UrlStyle};

use keys::Which;
use routes::{abort_upload, complete_upload, health, sign, start_upload, submit, Daemon};
use store::Store;

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

const STREAM: &str = "MEDIA_JOBS";
pub(crate) const SUBJECT: &str = "media.jobs";
const CONSUMER: &str = "media-worker";
/// A submit repeated inside this window is the same job.
const DEDUPE_WINDOW: Duration = Duration::from_secs(3600);
/// Short, with progress acks every 20 s while a job runs: a worker that dies
/// mid-photo has its job redelivered in a minute, not in a quarter of an hour.
const ACK_WAIT: Duration = Duration::from_secs(60);

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
    tokio::spawn(job_worker::worker(d.clone(), consumer));

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
}
