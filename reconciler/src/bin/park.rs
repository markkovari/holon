//! `comp-park` — a durable record of an outstanding call, instead of a thread
//! blocked on its answer (ADR-0100), served over loopback HTTP for
//! `components/park-store` (which, composed with `components/park-gateway`,
//! is what `apps/park.toml` puts on the tailnet).
//!
//! ## ADR-0095's three questions
//!
//! 1. **Something WASI does not give a guest?** Yes: a held NATS JetStream KV
//!    connection and, for `wake`, a JetStream publish to `PARK_WAKE`. Same
//!    reason `comp-vcs` is native.
//! 2. **The smallest it could be?** Every decision — idempotency, the state
//!    machine, expiry — is `holon-park`'s engine, which also builds for
//!    `wasm32-wasip2`; this file is the engine's one native adapter, a JSON
//!    mapping one route per WIT function, and the `PARK_WAKE` publish.
//! 3. **A contract a component could have answered?** Yes, and it stays WIT:
//!    `wit/park/park.wit`. A `park-store` component would export it and make
//!    each call one request here, the same shape as `vcs-store`/`comp-vcs`.
//!
//! ## Routes
//!
//! `POST /v1/<wit function>` (`park`, `wake`, `pending`, `take-ready`,
//! `cancel`, `oplog`), JSON in and out as `holon_park::wire` defines, errors as
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
//! have to poll `pending`.
//!
//!   comp-park --addr 127.0.0.1:8015 --nats-url nats://127.0.0.1:4222

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
use holon_park::model::CallResult;
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
    /// work-queue stream).
    #[arg(long, env = "PARK_NATS_URL", default_value = "nats://127.0.0.1:4222")]
    nats_url: String,
    /// The KV bucket ticket records and their indexes live in.
    #[arg(long, default_value = "holon-park")]
    bucket: String,
    /// The `PARK_WAKE`-shaped stream's name.
    #[arg(long, default_value = "PARK_WAKE")]
    wake_stream: String,

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

/// Run `$body` with `$e` bound to the engine, whichever backend it is.
macro_rules! on {
    ($d:expr, |$e:ident| $body:expr) => {
        match &$d.backend {
            Backend::Live($e, _) => $body,
            Backend::Mem($e) => $body,
        }
    };
}

impl Daemon {
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
                        if outcome.freshly_woken {
                            if let Backend::Live(_, wake) = &self.backend {
                                let msg = WakeMessage {
                                    session: outcome.session.clone(),
                                    ticket: outcome.ticket.clone(),
                                };
                                if let Err(e) = wake.publish(&msg).await {
                                    // The record is already durable; a failed
                                    // nudge is not a failed `wake`. A resumer
                                    // that never sees this message still finds
                                    // the ticket via `pending`, just later.
                                    eprintln!(
                                        "comp-park: PARK_WAKE publish for {}: {e:#}",
                                        outcome.ticket
                                    );
                                }
                            }
                            (200, json_of(&outcome.ticket))
                        } else {
                            (200, json_of(&outcome.ticket))
                        }
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
                answer::<CallResult>(on!(self, |e| e.take_ready(&req.ticket, now_ms()).await))
            }
            "cancel" => {
                let req: wire::CancelRequest = parse(body)?;
                answer(on!(self, |e| e.cancel(&req.ticket, req.by, now_ms()).await))
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
        // A cheap, harmless read: a session that (almost certainly) has never
        // been parked, so this never returns a real ticket list to log.
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
        let (store, wake) = nats::connect(&args.nats_url, &args.bucket, &args.wake_stream)
            .await
            .with_context(|| {
                format!(
                    "NATS at {} (bucket {}, stream {})",
                    args.nats_url, args.bucket, args.wake_stream
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
