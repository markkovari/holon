//! `agent-runtime` — the headless daemon: schedules, events and HTTP triggers
//! for every agent in a state directory, with no UI. The console embeds the
//! same library; run this when agents should keep working with it closed.
//!
//! Traces go to an OpenTelemetry collector over OTLP/HTTP (JSON) when
//! `--otlp-endpoint` or the standard `OTEL_EXPORTER_OTLP_ENDPOINT` is set.
//!
//! The admin token is generated on first start and kept in
//! `<state-dir>/admin-token` (mode 0600); control endpoints need it.

use std::path::PathBuf;

use agent_runtime::model::LocalModel;
use agent_runtime::{server, Config, Runtime};
use clap::Parser;

#[derive(Parser)]
#[command(name = "agent-runtime", about = "Run autonomous agents: schedules, events, HTTP.")]
struct Cli {
    /// Where agents, memory, runs and the admin token live.
    #[arg(long)]
    state_dir: PathBuf,
    /// Address to serve the trigger and admin API on.
    #[arg(long, default_value = "127.0.0.1:18017")]
    listen: String,
    /// A local OpenAI-compatible model server (`fm serve`, `mlx_lm.server`, ...).
    #[arg(long)]
    local_url: Option<String>,
    /// Model name to ask that server for.
    #[arg(long, default_value = "")]
    local_model: String,
    /// An OpenTelemetry collector's OTLP/HTTP base URL; finished runs are
    /// exported to `<endpoint>/v1/traces`.
    #[arg(long, env = "OTEL_EXPORTER_OTLP_ENDPOINT")]
    otlp_endpoint: Option<String>,
}

fn main() -> Result<(), String> {
    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.state_dir).map_err(|e| e.to_string())?;
    let token = server::admin_token_in(&cli.state_dir)?;

    let mut cfg = Config::new(&cli.state_dir);
    cfg.local = LocalModel { base_url: cli.local_url, model: cli.local_model };
    cfg.otlp_endpoint = cli.otlp_endpoint.filter(|e| !e.is_empty());
    let rt = Runtime::new(cfg)?;
    let addr = server::serve(rt.clone(), &cli.listen, token)?;
    rt.start_scheduler();
    eprintln!("agent-runtime listening on http://{addr} (state: {})", cli.state_dir.display());
    loop {
        std::thread::park();
    }
}
