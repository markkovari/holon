//! NATS JetStream backed multi-tier LLM queuing and coalescing daemon (ADR-0126).

use anyhow::{anyhow, Context, Result};
use async_nats::jetstream;
use clap::Parser;
use std::sync::Arc;
use std::time::Duration;

mod worker;
use worker::{
    BatchWorker, WorkerConfig, BATCH_STREAM, BATCH_SUBJECT, CONSUMER_NAME, REALTIME_STREAM,
    REALTIME_SUBJECT,
};

#[derive(Parser)]
#[command(
    name = "holon-llmbatch",
    about = "Coalescing LLM batch drainer and realtime router over NATS JetStream"
)]
struct Args {
    /// NATS server URL.
    #[arg(long, default_value = "nats://127.0.0.1:4222", env = "NATS_URL")]
    nats_url: String,

    /// Upstream model API URL (Gemini REST generateContent or OpenAI-compatible chat/completions).
    #[arg(
        long,
        default_value = "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent",
        env = "LLM_UPSTREAM_URL"
    )]
    upstream_url: String,

    /// Upstream API key (Gemini API key or OpenAI Bearer token).
    #[arg(long, env = "GEMINI_API_KEY")]
    api_key: Option<String>,

    /// Path to file containing upstream API key.
    #[arg(long)]
    api_key_file: Option<std::path::PathBuf>,

    /// Target model identifier.
    #[arg(long, default_value = "gemini-2.5-flash", env = "LLM_MODEL")]
    model: String,

    /// Maximum batch size of coalesced tasks.
    #[arg(long, default_value = "50")]
    batch_size: usize,

    /// Maximum wait window in milliseconds for batch accumulation.
    #[arg(long, default_value = "2000")]
    batch_window_ms: u64,
}

fn resolve_key(key: Option<String>, file: Option<std::path::PathBuf>) -> Option<String> {
    if let Some(path) = file {
        if let Ok(content) = std::fs::read_to_string(path) {
            let trimmed = content.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    key.filter(|s| !s.trim().is_empty())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let api_key = resolve_key(args.api_key, args.api_key_file);

    eprintln!(
        "holon-llmbatch: connecting to NATS at {} | upstream: {} | model: {}",
        args.nats_url, args.upstream_url, args.model
    );

    let nats = async_nats::connect(&args.nats_url)
        .await
        .with_context(|| format!("connect NATS {}", args.nats_url))?;
    let js = jetstream::new(nats.clone());

    // 1. Ensure WorkQueue stream for batch tasks
    let batch_stream = js
        .get_or_create_stream(jetstream::stream::Config {
            name: BATCH_STREAM.into(),
            subjects: vec![BATCH_SUBJECT.into()],
            retention: jetstream::stream::RetentionPolicy::WorkQueue,
            max_age: Duration::from_secs(7 * 86400),
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow!("stream {BATCH_STREAM}: {e}"))?;

    // 2. Ensure Limits stream for realtime tasks (60s TTL)
    let _ = js
        .get_or_create_stream(jetstream::stream::Config {
            name: REALTIME_STREAM.into(),
            subjects: vec![REALTIME_SUBJECT.into()],
            retention: jetstream::stream::RetentionPolicy::Limits,
            max_age: Duration::from_secs(60),
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow!("stream {REALTIME_STREAM}: {e}"))?;

    // 3. Ensure durable consumer for batch queue
    let consumer = batch_stream
        .get_or_create_consumer(
            CONSUMER_NAME,
            jetstream::consumer::pull::Config {
                durable_name: Some(CONSUMER_NAME.into()),
                ack_policy: jetstream::consumer::AckPolicy::Explicit,
                ack_wait: Duration::from_secs(120),
                max_deliver: 3,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| anyhow!("consumer {CONSUMER_NAME}: {e}"))?;

    let config = WorkerConfig {
        nats_url: args.nats_url,
        upstream_url: args.upstream_url,
        api_key,
        model: args.model,
        max_batch_size: args.batch_size,
        batch_window: Duration::from_millis(args.batch_window_ms),
    };

    let worker = Arc::new(BatchWorker::new(nats, js, config));

    eprintln!("holon-llmbatch: workers initialized, listening on batch and realtime subjects");

    let batch_handle = {
        let w = worker.clone();
        tokio::spawn(async move {
            if let Err(e) = w.run_batch_worker(consumer).await {
                eprintln!("batch worker exited with error: {e}");
            }
        })
    };

    let realtime_handle = {
        let w = worker.clone();
        tokio::spawn(async move {
            if let Err(e) = w.run_realtime_worker().await {
                eprintln!("realtime worker exited with error: {e}");
            }
        })
    };

    tokio::select! {
        res = batch_handle => eprintln!("batch worker finished: {res:?}"),
        res = realtime_handle => eprintln!("realtime worker finished: {res:?}"),
        _ = tokio::signal::ctrl_c() => eprintln!("shutting down holon-llmbatch"),
    }

    Ok(())
}
