//! `comp-resume` — the resumer ADR-0100 left unbuilt, and what ADR-0106 says it
//! does: hand each resolved ticket to whatever drives the turn, and give up on
//! the calls that never answered.
//!
//! Two loops, in one process:
//!
//! * **Deliver.** A durable pull consumer on `PARK_WAKE`. For each message it
//!   reads the ticket, `POST`s what it holds to `--sink-url`, and only when the
//!   sink says yes closes the ticket and acknowledges the message. At least
//!   once: a resumer killed in between is replaced, and the sink is handed the
//!   same delivery again under the same `Idempotency-Key`.
//! * **Recover.** Every `--sweep-secs`, one pass over every ticket
//!   (`holon_park::recover`): a call past its deadline is re-dispatched,
//!   interrupted or cancelled according to the `replay` it was parked with, and
//!   an answer nobody took is announced again.
//!
//! ## Why this is native (ADR-0095)
//!
//! A held JetStream consumer and a timer: what a guest cannot have. Every
//! decision is `holon-park`'s (`resume.rs`, `recover.rs`), which builds for
//! `wasm32-wasip2` too; this file is the CLI, the HTTP sink, and the signals.
//! It reads and writes the same KV bucket as `comp-park` and shares no process
//! with it: compare-and-set, not a lock, decides who moves a ticket, so any
//! number of resumers may run.
//!
//! ## The sink
//!
//! `POST <sink-url>`, `Content-Type: application/json`, `Idempotency-Key:
//! <ticket>:<what it says>`, `Authorization: Bearer <token>` if one is set.
//! The body is `holon_park::resume::Delivery`:
//!
//! ```json
//! {"ticket": "…", "session": "…", "correlation": "…", "event": "answered",
//!  "answer": {"ok": true, "body": [42], "detail": null}}
//! ```
//!
//! `event` is `answered`, `interrupted` (`interruption: {reason, at, by}`) or
//! `redispatch` (`attempt`, `description`). The sink answers:
//!
//! | status | meaning |
//! |---|---|
//! | `2xx` | accepted, now or on an earlier delivery with the same key |
//! | `410` | nobody is there (the session is gone): the ticket is cancelled |
//! | anything else, or no answer | not now: the message comes back after a delay |
//!
//!   comp-resume --sink-url http://127.0.0.1:8016/park/deliver --nats-url nats://127.0.0.1:4222

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use clap::Parser;

use holon_park::nats;
use holon_park::recover::RecoveryPolicy;
use holon_park::resume::{run, Delivery, RunConfig, Sink, SinkReply};
use holon_park::Engine;

#[derive(Parser, Debug)]
#[command(
    name = "comp-resume",
    about = "Deliver resolved parked calls to the turn that made them, and give up on the ones that never answered (ADR-0106)."
)]
struct Args {
    /// Where deliveries are POSTed: whatever drives the turn.
    #[arg(long)]
    sink_url: String,
    /// Bearer secret the sink expects.
    #[arg(long)]
    sink_token: Option<String>,
    /// Same, read from a file. Wins over `--sink-token`.
    #[arg(long)]
    sink_token_file: Option<PathBuf>,
    /// How long one sink call may take before it counts as "not now".
    #[arg(long, default_value_t = 30)]
    sink_timeout_secs: u64,

    /// The same NATS `comp-park` uses: ticket records (KV) and `PARK_WAKE`. One server, or
    /// several separated by commas.
    #[arg(long, env = "PARK_NATS_URL", default_value = "nats://127.0.0.1:4222")]
    nats_url: String,
    #[arg(long, default_value = "holon-park")]
    bucket: String,
    #[arg(long, default_value = "PARK_WAKE")]
    wake_stream: String,
    /// Copies JetStream keeps of the bucket and `PARK_WAKE` if this is the process that
    /// creates them (ADR-0111): the same number `comp-park` was given. It is a floor, so a
    /// resumer left on 1 never lowers what `comp-park` asked for, and refuses a bucket that
    /// keeps fewer than it was told to expect.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=5))]
    replicas: u8,
    /// The durable consumer's name. Resumers sharing one share the work; give
    /// two resumers different names and each delivers every message.
    #[arg(long, default_value = "comp-resume")]
    consumer: String,

    /// Messages handled at once.
    #[arg(long, default_value_t = 8)]
    concurrency: usize,
    /// How long a delivered message may go unanswered before the server gives it
    /// to another resumer. Must exceed `--sink-timeout-secs`, or a slow sink gets
    /// every delivery twice.
    #[arg(long, default_value_t = 60)]
    ack_wait_secs: u64,

    /// Seconds between recovery passes.
    #[arg(long, default_value_t = 15)]
    sweep_secs: u64,
    /// How long past its deadline a parked call must sit before a pass acts on
    /// it: slack for a late webhook, and for a clock that disagrees with the
    /// caller's.
    #[arg(long, default_value_t = 10)]
    deadline_grace_secs: u64,
    /// Re-dispatches a `replay: safe` call is granted before it is interrupted.
    #[arg(long, default_value_t = 3)]
    max_redispatches: u32,
    /// How long a re-dispatched call gets before the next pass looks at it.
    #[arg(long, default_value_t = 60)]
    retry_after_secs: u64,
    /// How long a resolved ticket may sit untaken before it is announced again.
    #[arg(long, default_value_t = 30)]
    reannounce_after_secs: u64,
    /// How long a commit's intent may sit pending before a pass finishes it or rolls it back
    /// (ADR-0110): a younger one is presumed to be in the hands of the caller that wrote it.
    #[arg(long, default_value_t = 30)]
    batch_lease_secs: u64,
}

struct HttpSink {
    http: reqwest::Client,
    url: String,
    token: Option<String>,
}

impl Sink for HttpSink {
    async fn deliver(&self, d: &Delivery) -> SinkReply {
        let mut req =
            self.http.post(&self.url).header("Idempotency-Key", d.idempotency_key()).json(d);
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        match req.send().await {
            Ok(r) if r.status().is_success() => SinkReply::Delivered,
            Ok(r) if r.status().as_u16() == 410 => {
                SinkReply::Gone(r.text().await.unwrap_or_default())
            }
            Ok(r) => {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                SinkReply::Retry(format!(
                    "sink answered {status}: {}",
                    &text[..text.len().min(200)]
                ))
            }
            Err(e) => SinkReply::Retry(format!("sink unreachable: {e}")),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        args.ack_wait_secs > args.sink_timeout_secs,
        "--ack-wait-secs ({}) must exceed --sink-timeout-secs ({}): otherwise the server \
         redelivers a message whose sink call is still in flight",
        args.ack_wait_secs,
        args.sink_timeout_secs
    );
    ensure!(args.concurrency > 0, "--concurrency must be at least 1");
    let token = comp_daemon_kit::resolve_token(args.sink_token.clone(), args.sink_token_file);

    let (store, wake) = nats::connect_with_replicas(
        &args.nats_url,
        &args.bucket,
        &args.wake_stream,
        usize::from(args.replicas),
    )
    .await
    .with_context(|| {
        format!(
            "NATS at {} (bucket {}, stream {}, {} replicas)",
            args.nats_url, args.bucket, args.wake_stream, args.replicas
        )
    })?;
    let sink = HttpSink {
        http: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(args.sink_timeout_secs))
            .build()?,
        url: args.sink_url.clone(),
        token,
    };
    let cfg = RunConfig {
        consumer: args.consumer.clone(),
        ack_wait: Duration::from_secs(args.ack_wait_secs),
        concurrency: args.concurrency,
        sweep_every: Duration::from_secs(args.sweep_secs),
        policy: RecoveryPolicy {
            deadline_grace_ms: args.deadline_grace_secs * 1_000,
            max_redispatches: args.max_redispatches,
            retry_after_ms: args.retry_after_secs * 1_000,
            reannounce_after_ms: args.reannounce_after_secs * 1_000,
            batch_lease_ms: args.batch_lease_secs * 1_000,
        },
        ..Default::default()
    };

    println!(
        "comp-resume: {} | bucket {} | stream {} | consumer {} | sink {} | sweep every {}s",
        args.nats_url, args.bucket, args.wake_stream, args.consumer, args.sink_url, args.sweep_secs
    );

    let (stop, shutdown) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        wait_for_signal().await;
        eprintln!("comp-resume: stopping; finishing what is in flight");
        let _ = stop.send(true);
    });

    run(Arc::new(Engine::new(store)), Arc::new(wake), Arc::new(sink), cfg, shutdown)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

async fn wait_for_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installing a SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}
