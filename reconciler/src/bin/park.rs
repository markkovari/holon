//! `comp-park` — a durable record of an outstanding call, instead of a thread
//! blocked on its answer (ADR-0100), served over loopback HTTP for
//! `components/park-store` (which, composed with `components/park-gateway`,
//! is what `apps/park.toml` puts on the tailnet).
//!
//! ## Why this is native (ADR-0095)
//!
//! It needs a held NATS JetStream KV connection and, for `wake`, a JetStream publish to `PARK_WAKE`,
//! which a guest cannot do (same reason `comp-vcs` is native). Every decision (idempotency, state
//! machine, expiry) is `holon-park`'s engine, which also builds for `wasm32-wasip2`; this file is its
//! one native adapter, a JSON mapping, and the `PARK_WAKE` publish. The contract stays WIT
//! (`wit/park/park.wit`), the same shape as `vcs-store`/`comp-vcs`.
//!
//! ## Routes
//!
//! `POST /v1/<wit function>` (`park`, `wake`, `pending`, `take-ready`,
//! `interrupt`, `cancel`, `oplog`, `commit`), JSON in and out as `holon_park::wire`
//! defines, errors as
//! `{error, detail, message}` with a status by kind (`wire::status_of`).
//! `GET /health` is open; everything else takes `Authorization: Bearer
//! <token>` when `--token`/`--token-file` is set.
//!
//! ## The wake stream is a notification, not the record
//!
//! `wake` answers from the ticket's own record the moment it is durable —
//! publishing to `PARK_WAKE` happens AFTER that, and only when the engine says
//! this was a fresh transition (`WakeOutcome::freshly_woken`), never on an
//! idempotent redelivery. If the publish itself fails, `wake` still answers
//! `200`: the record is the truth, the stream is a nudge so a resumer does not
//! have to poll `pending`. `interrupt` works the same way
//! (`InterruptOutcome::fresh`), and a nudge that was lost is found by
//! `comp-resume`'s recovery pass (ADR-0106).
//!
//!   comp-park --addr 127.0.0.1:8015 --nats-url nats://127.0.0.1:4222
//!
//! Against a three-node cluster, keeping every ticket on all three (ADR-0111):
//!
//!   comp-park --nats-url nats://a:4222,nats://b:4222,nats://c:4222 --replicas 3

use std::path::PathBuf;

use anyhow::{Context, Result};
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use holon_park::error::ParkError;
use holon_park::model::Resolution;
use holon_park::nats::{self, NatsParkStore, WakeMessage, WakeStream};
use holon_park::wire;
use holon_park::{mem_engine, Engine, MemEngine};

type PResult<T> = std::result::Result<T, ParkError>;
type LiveEngine = Engine<NatsParkStore>;

#[derive(Parser, Debug)]
#[command(
    name = "comp-park",
    about = "A durable record of an outstanding call (ADR-0100), for components over loopback HTTP."
)]
struct Args {
    /// Shared secret a caller must send as `Authorization: Bearer <token>`.
    #[arg(long)]
    token: Option<String>,
    /// Same, read from a file. Wins over `--token`.
    #[arg(long)]
    token_file: Option<PathBuf>,

    /// Where to listen. Loopback by default.
    #[arg(long, default_value = "127.0.0.1:8015")]
    addr: String,

    /// NATS with JetStream: ticket records and indexes (KV), `PARK_WAKE` (a
    /// work-queue stream). One server, or several separated by commas: a daemon told about
    /// one cannot start while that one is down.
    #[arg(long, env = "PARK_NATS_URL", default_value = "nats://127.0.0.1:4222")]
    nats_url: String,
    /// The KV bucket ticket records and their indexes live in.
    #[arg(long, default_value = "holon-park")]
    bucket: String,
    /// The `PARK_WAKE`-shaped stream's name.
    #[arg(long, default_value = "PARK_WAKE")]
    wake_stream: String,
    /// Copies JetStream keeps of the bucket and of `PARK_WAKE` (ADR-0111): with one, the node
    /// holding them going down takes every ticket out of reach until it is back. Needs a
    /// cluster of at least that many servers. A bucket or stream that already exists with
    /// fewer copies is refused, not changed: the error names the `nats` command that raises it.
    #[arg(
        long,
        default_value_t = 1,
        value_parser = clap::value_parser!(u8).range(1..=5),
        conflicts_with = "memory"
    )]
    replicas: u8,

    /// Everything in memory, nothing durable, no `PARK_WAKE`: for trying it
    /// and for tests.
    #[arg(long)]
    memory: bool,
}

enum Backend {
    Live(Box<LiveEngine>, Box<WakeStream>),
    Mem(Box<MemEngine>),
}

struct Daemon {
    backend: Backend,
    bucket: String,
    wake_stream: String,
}

type Answer = (u16, Value);

fn json_of<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn refusal(e: &ParkError) -> Answer {
    let body = wire::ErrorBody::from(e);
    (wire::status_of(&body.error), json_of(&body))
}

fn bad_request(e: impl std::fmt::Display) -> Answer {
    (400, json_of(&wire::ErrorBody::new("bad-request", e.to_string())))
}

fn answer<T: Serialize>(r: PResult<T>) -> Answer {
    match r {
        Ok(v) => (200, json_of(&v)),
        Err(e) => refusal(&e),
    }
}

fn parse<T: DeserializeOwned>(body: &[u8]) -> std::result::Result<T, Answer> {
    serde_json::from_slice(body).map_err(bad_request)
}

fn now_ms() -> u64 {
    holon_park::engine::now_ms()
}

macro_rules! on {
    ($d:expr, |$e:ident| $body:expr) => {
        match &$d.backend {
            Backend::Live($e, _) => $body,
            Backend::Mem($e) => $body,
        }
    };
}

impl Daemon {
    /// Tell a resumer there is something to take. The record is already
    /// durable, so a failed nudge is logged, not returned: a resumer that
    /// misses it still finds the ticket (`pending`, or `comp-resume`'s pass).
    async fn announce(&self, msg: &WakeMessage) {
        if let Backend::Live(_, wake) = &self.backend {
            if let Err(e) = wake.publish(msg).await {
                eprintln!("comp-park: PARK_WAKE publish for {}: {e:#}", msg.ticket);
            }
        }
    }

    /// One WIT function, by name, over a JSON body.
    async fn dispatch(&self, func: &str, body: &[u8]) -> Answer {
        match self.dispatch_inner(func, body).await {
            Ok(a) | Err(a) => a,
        }
    }

    async fn dispatch_inner(&self, func: &str, body: &[u8]) -> std::result::Result<Answer, Answer> {
        Ok(match func {
            "park" => {
                let req: wire::ParkRequest = parse(body)?;
                answer(on!(self, |e| e.park(&req.session, req.call, req.by, now_ms()).await))
            }
            "wake" => {
                let req: wire::WakeRequest = parse(body)?;
                match on!(self, |e| e.wake(&req.correlation, req.answer, now_ms()).await) {
                    Ok(outcome) => {
                        // What the engine says to announce: the ticket if this woke it, or the
                        // group this resolved (ADR-0109); never a child on its own.
                        for msg in &outcome.announce {
                            self.announce(msg).await;
                        }
                        (200, json_of(&outcome.ticket))
                    }
                    Err(e) => refusal(&e),
                }
            }
            "pending" => {
                let req: wire::Session = parse(body)?;
                answer(on!(self, |e| e.pending(&req.session, now_ms()).await))
            }
            "take-ready" => {
                let req: wire::Ticket = parse(body)?;
                answer::<Resolution>(on!(self, |e| e.take_ready(&req.ticket, now_ms()).await))
            }
            "interrupt" => {
                let req: wire::InterruptRequest = parse(body)?;
                match on!(self, |e| e.interrupt(&req.ticket, &req.reason, req.by, now_ms()).await) {
                    Ok(outcome) => {
                        for msg in &outcome.announce {
                            self.announce(msg).await;
                        }
                        (200, json_of(&outcome.status))
                    }
                    Err(e) => refusal(&e),
                }
            }
            "cancel" => {
                let req: wire::CancelRequest = parse(body)?;
                // Cancelling a child can resolve its group, which is then what is announced.
                match on!(self, |e| e.cancel_announcing(&req.ticket, req.by, now_ms()).await) {
                    Ok(announce) => {
                        for msg in &announce {
                            self.announce(msg).await;
                        }
                        (200, Value::Null)
                    }
                    Err(e) => refusal(&e),
                }
            }
            "commit" => {
                let req: wire::CommitRequest = parse(body)?;
                match on!(self, |e| e.commit(&req.session, req.effects, req.by, now_ms()).await) {
                    Ok(committed) => {
                        // What the commit's effects resolved: a group, never a child.
                        for msg in &committed.announce {
                            self.announce(msg).await;
                        }
                        (200, json_of(&committed.result))
                    }
                    Err(e) => refusal(&e),
                }
            }
            "oplog" => {
                let req: wire::OplogRequest = parse(body)?;
                answer(on!(self, |e| e.oplog(&req.session, req.after, req.limit, now_ms()).await))
            }
            other => (
                404,
                json_of(&wire::ErrorBody::new("not-found", format!("no such route: {other}"))),
            ),
        })
    }

    async fn healthy(&self) -> bool {
        // A cheap, harmless read: a session that has almost certainly never been parked.
        on!(self, |e| e.pending(&"comp-park/health-probe".to_string(), now_ms()).await).is_ok()
    }
}

type Shared = std::sync::Arc<Daemon>;

fn respond((status, body): Answer) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (code, Json(body)).into_response()
}

async fn call(
    State(d): State<Shared>,
    UrlPath(func): UrlPath<String>,
    body: axum::body::Bytes,
) -> Response {
    respond(d.dispatch(&func, &body).await)
}

async fn health(State(d): State<Shared>) -> Json<Value> {
    let ok = d.healthy().await;
    let backend = match d.backend {
        Backend::Live(..) => "nats",
        Backend::Mem(_) => "memory",
    };
    Json(json!({
        "ok": ok,
        "backend": backend,
        "bucket": d.bucket,
        "wake-stream": match d.backend { Backend::Live(..) => Some(d.wake_stream.clone()), Backend::Mem(_) => None },
    }))
}

fn app(d: Shared, token: Option<String>) -> Router {
    let api = Router::new()
        .route("/v1/{func}", post(call))
        .layer(axum::middleware::from_fn(comp_reconciler::daemon_auth::require_token))
        .layer(axum::Extension(std::sync::Arc::new(token)))
        .with_state(d.clone());
    Router::new().route("/health", get(health)).with_state(d).merge(api)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token =
        comp_reconciler::daemon_auth::resolve_token(args.token.clone(), args.token_file.clone());
    comp_reconciler::daemon_auth::warn_if_unauthenticated("comp-park", &token);

    let backend = if args.memory {
        eprintln!("comp-park: --memory: nothing is durable, no PARK_WAKE");
        Backend::Mem(Box::new(mem_engine()))
    } else {
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
        Backend::Live(Box::new(Engine::new(store)), Box::new(wake))
    };

    let d = std::sync::Arc::new(Daemon {
        backend,
        bucket: args.bucket.clone(),
        wake_stream: args.wake_stream.clone(),
    });

    let listener = tokio::net::TcpListener::bind(&args.addr)
        .await
        .with_context(|| format!("binding {}", args.addr))?;
    println!(
        "comp-park: listening on http://{} | {} | bucket {} | wake stream {}",
        args.addr,
        if args.memory { "memory".to_string() } else { args.nats_url.clone() },
        args.bucket,
        args.wake_stream
    );
    axum::serve(listener, app(d, token)).await?;
    Ok(())
}
