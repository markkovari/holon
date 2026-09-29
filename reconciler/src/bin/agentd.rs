//! `comp-agentd` — holon as a persistent agent service (ADR-0102): sessions,
//! tasks, one resumable event stream per session, and human approval of tool
//! calls. One port serves both halves of the contract: REST + Server-Sent
//! Events (`api/openapi.yaml`) and gRPC / gRPC-web (`api/holon/v1/agent.proto`).
//!
//! ## ADR-0095's three questions
//!
//! 1. **Something WASI does not give a guest?** Yes: held connections (an SSE
//!    or gRPC stream per watcher, open for as long as a task runs), a task that
//!    waits minutes or hours for a human, and `run`, which spawns a process.
//! 2. **The smallest it could be?** No, and knowingly: the agent loop and the
//!    two model dialects live here too (`agentd/agent.rs`, `agentd/model.rs`).
//! 3. **A contract a component could have answered?** Partly, and this is the
//!    recorded EXCEPTION (ADR-0102): `llm:inference` has no tool use and no
//!    streaming, and a WASI 0.2 guest cannot stream a reply across a
//!    component boundary. When WASI 0.3 streams land, the model call moves
//!    behind a provider component and this daemon keeps only (1).
//!
//! ## Running it
//!
//!   comp-agentd --workspace-root ~/work --state-dir ~/.comp/agentd \
//!     --provider anthropic --api-key-file ~/.anthropic
//!   comp-agentd --workspace-root ~/work --provider openai \
//!     --base-url http://csatapaci:8080/v1 --price qwen=0,0
//!   comp-agentd --workspace-root /tmp --provider mock      # free, scripted
//!
//! `GET /health` is open; everything else, gRPC included, takes
//! `Authorization: Bearer <token>` when `--token`/`--token-file` is set.

#[path = "agentd/agent.rs"]
mod agent;
#[path = "agentd/grpc.rs"]
mod grpc;
#[path = "agentd/model.rs"]
mod model;
#[path = "agentd/rest.rs"]
mod rest;
#[path = "agentd/service.rs"]
mod service;
#[path = "agentd/session.rs"]
mod session;
#[cfg(test)]
#[path = "agentd/tests.rs"]
mod tests;
#[path = "agentd/tools.rs"]
mod tools;
#[path = "agentd/wire.rs"]
mod wire;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use clap::Parser;

use service::Daemon;
use session::Registry;

#[derive(Parser, Debug)]
#[command(
    name = "comp-agentd",
    about = "Holon as an agent service: sessions, tasks, streamed events, approvals (ADR-0102)."
)]
struct Args {
    /// Shared secret a caller must send as `Authorization: Bearer <token>`.
    #[arg(long)]
    token: Option<String>,
    /// Same, read from a file. Wins over `--token`.
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// Where to listen. Loopback by default.
    #[arg(long, default_value = "127.0.0.1:8016")]
    addr: String,

    /// A directory sessions may work under. Repeatable; a session whose
    /// `workspace_dir` is not inside one of these is refused.
    #[arg(long = "workspace-root", required = true)]
    workspace_roots: Vec<PathBuf>,
    /// Keep sessions, their event logs and conversations here, and load them
    /// back on start. Unset: memory only, gone when the process exits.
    #[arg(long)]
    state_dir: Option<PathBuf>,

    /// `anthropic` speaks `/v1/messages`; `openai` speaks `/chat/completions`
    /// (vLLM, llama.cpp, Ollama, mlx_lm); `mock` is scripted and free.
    #[arg(long, value_parser = ["anthropic", "openai", "mock"], default_value = "anthropic")]
    provider: String,
    /// API base. Default: `https://api.anthropic.com` or `https://api.openai.com/v1`.
    #[arg(long, default_value = "")]
    base_url: String,
    /// A file holding the API key. Optional for a local OpenAI-compatible server.
    #[arg(long)]
    api_key_file: Option<PathBuf>,
    /// `PATTERN=IN,OUT`: a model whose id contains PATTERN (ignoring case) costs IN and OUT
    /// cents per million input/output tokens, instead of `cost.rs`'s table
    /// (which charges any model it does not know as opus). Repeatable; the
    /// first match wins. `--price qwen=0,0` makes a self-hosted Qwen free.
    #[arg(long = "price", value_parser = parse_price)]
    prices: Vec<(String, u64, u64)>,

    /// Model turns one task may take before it fails with `max_turns`.
    #[arg(long, default_value_t = 50)]
    max_turns: u32,
    /// Deny a pending tool call nobody answers within this many seconds.
    /// Unset: wait forever.
    #[arg(long)]
    approval_timeout_secs: Option<u64>,
}

fn parse_price(s: &str) -> std::result::Result<(String, u64, u64), String> {
    let bad = || format!("{s}: expected PATTERN=IN,OUT, e.g. qwen=0,0");
    let (pat, prices) = s.split_once('=').ok_or_else(bad)?;
    let (i, o) = prices.split_once(',').ok_or_else(bad)?;
    if pat.is_empty() {
        return Err(bad());
    }
    Ok((
        pat.to_string(),
        i.trim().parse().map_err(|_| bad())?,
        o.trim().parse().map_err(|_| bad())?,
    ))
}

/// REST and gRPC on one router, both behind the token check.
fn app(d: Arc<Daemon>, token: Option<String>) -> Router {
    let grpc = tonic::service::Routes::new(grpc::server(d.clone()))
        .into_axum_router()
        .layer(tonic_web::GrpcWebLayer::new());
    let api = rest::routes(d.clone())
        .merge(grpc)
        .layer(axum::middleware::from_fn(comp_reconciler::daemon_auth::require_token))
        .layer(axum::Extension(Arc::new(token)));
    rest::health_route(d).merge(api)
}

fn off_peak_now() -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    comp_reconciler::offpeak::deepseek_off_peak(now, &[])
}

fn provider(args: &Args) -> Result<model::Provider> {
    let key = match &args.api_key_file {
        Some(p) => std::fs::read_to_string(p)
            .with_context(|| format!("reading {}", p.display()))?
            .trim()
            .to_string(),
        None => String::new(),
    };
    let base = |default: &str| {
        if args.base_url.is_empty() {
            default.to_string()
        } else {
            args.base_url.clone()
        }
    };
    Ok(match args.provider.as_str() {
        "mock" => model::Provider::Mock,
        "openai" => model::Provider::OpenAi { base: base("https://api.openai.com/v1"), key },
        _ => {
            anyhow::ensure!(!key.is_empty(), "--provider anthropic needs --api-key-file");
            model::Provider::Anthropic { base: base("https://api.anthropic.com"), key }
        }
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token =
        comp_reconciler::daemon_auth::resolve_token(args.token.clone(), args.token_file.clone());
    comp_reconciler::daemon_auth::warn_if_unauthenticated("comp-agentd", &token);
    let roots = args
        .workspace_roots
        .iter()
        .map(|r| r.canonicalize().with_context(|| format!("--workspace-root {}", r.display())))
        .collect::<Result<Vec<_>>>()?;
    let sessions = match &args.state_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("--state-dir {}", dir.display()))?;
            session::load_all(dir)?
        }
        None => Default::default(),
    };
    let ctx = agent::Ctx {
        provider: provider(&args)?,
        http: reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build()?,
        max_turns: args.max_turns,
        approval_timeout: args.approval_timeout_secs.map(Duration::from_secs),
        prices: args.prices.clone(),
        off_peak: off_peak_now,
    };
    let loaded = sessions.len();
    let d = Arc::new(Daemon {
        sessions: Registry::new(sessions),
        roots,
        state_dir: args.state_dir.clone(),
        ctx: Arc::new(ctx),
    });
    let listener = tokio::net::TcpListener::bind(&args.addr)
        .await
        .with_context(|| format!("binding {}", args.addr))?;
    println!(
        "comp-agentd: listening on http://{} (REST + gRPC) | provider {} | {}",
        args.addr,
        args.provider,
        match &args.state_dir {
            Some(d) => format!("state {} ({loaded} sessions loaded)", d.display()),
            None => "memory only".into(),
        }
    );
    axum::serve(listener, app(d, token)).await?;
    Ok(())
}
