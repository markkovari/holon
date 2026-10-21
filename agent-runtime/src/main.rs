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
use clap::{Parser, Subcommand};

#[derive(Subcommand)]
enum Command {
    /// Sign in to Google once, in a browser, so agents can use Google Calendar. The OAuth client
    /// Google gave must be at `<google-dir>/client.json`; the login is saved beside it.
    GoogleAuth {
        /// Where `client.json` is and `token.json` is written.
        #[arg(long, env = "HOLON_GOOGLE_DIR")]
        google_dir: Option<PathBuf>,
    },
}

#[derive(Parser)]
#[command(
    name = "agent-runtime",
    about = "Run autonomous agents: schedules, events, HTTP.",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Where agents, memory, runs and the admin token live.
    #[arg(long)]
    state_dir: Option<PathBuf>,
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
    /// Speech-to-text command: `<bin> <audio.wav> <locale>` prints the transcript. On a
    /// Mac, build `speech/holon-stt.swift` (see its header). Without it, transcription is off.
    #[arg(long, env = "HOLON_STT_BIN")]
    stt_bin: Option<PathBuf>,
    /// Text-to-speech override: `<bin> <out-audio-file> <voice>`, text on stdin. By
    /// default the system `say` is used.
    #[arg(long, env = "HOLON_TTS_BIN")]
    tts_bin: Option<PathBuf>,
    /// Embedding service (`embed/server.py`); with it `recall` finds memories by meaning.
    #[arg(long, env = "HOLON_EMBED_URL", default_value = "")]
    embed_url: String,
    /// The launchd service that runs it: started on first use, stopped when idle.
    #[arg(long, env = "HOLON_EMBED_SERVICE", default_value = "")]
    embed_service: String,
    #[arg(long, default_value = "ffmpeg")]
    ffmpeg: String,
    /// Recognition language when a caller does not say.
    #[arg(long, default_value = "en-US")]
    speech_locale: String,
    /// Where the Google Calendar login lives: `client.json` (the OAuth client Google gave) and
    /// `token.json` (written by `agent-runtime google-auth`).
    #[arg(long, env = "HOLON_GOOGLE_DIR")]
    google_dir: Option<PathBuf>,
    /// `comp-agentd`'s base URL, where an agent granted `session_start` opens coding sessions
    /// (docs/adr/0119). Without it that tool answers that sessions are not available.
    #[arg(long, env = "HOLON_AGENTD_URL")]
    agentd_url: Option<String>,
    /// The bearer token `comp-agentd` was started with, read from a file.
    #[arg(long, env = "HOLON_AGENTD_TOKEN_FILE")]
    agentd_token_file: Option<PathBuf>,
    /// The model a coding session uses unless the agent's spec names one, as `comp-agentd`'s
    /// provider names it.
    #[arg(long, env = "HOLON_SESSION_MODEL", default_value = "")]
    session_model: String,
    /// A JSON file `{"url", "email", "password", "applier"}`: the platform whose vault keeps
    /// agents' API keys, and an account on it. Without it there are no API keys to manage.
    #[arg(long, env = "HOLON_PLATFORM_FILE")]
    platform_file: Option<PathBuf>,
}

/// `agent-runtime google-auth [--google-dir DIR]`: sign in to Google once, in a browser.
fn google_auth(dir: Option<PathBuf>) -> Result<(), String> {
    let mut cfg = agent_runtime::google::GoogleConfig::default();
    if let Some(dir) = dir {
        cfg.dir = dir;
    }
    agent_runtime::google::authorize(&cfg, &|url| {
        println!("Opening Google to ask for access to your calendars. If nothing opens, go to:\n\n{url}\n");
        let _ =
            std::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" })
                .arg(url)
                .status();
    })?;
    println!(
        "Signed in. The login is saved in {} (readable only by you).",
        cfg.dir.join("token.json").display()
    );
    Ok(())
}

fn main() -> Result<(), String> {
    let cli = Cli::parse();
    if let Some(Command::GoogleAuth { google_dir }) = cli.command {
        return google_auth(google_dir);
    }
    let state_dir = cli.state_dir.ok_or("--state-dir is required")?;
    std::fs::create_dir_all(&state_dir).map_err(|e| e.to_string())?;
    let token = server::admin_token_in(&state_dir)?;

    let mut cfg = Config::new(&state_dir);
    cfg.local = LocalModel { base_url: cli.local_url, model: cli.local_model };
    cfg.otlp_endpoint = cli.otlp_endpoint.filter(|e| !e.is_empty());
    cfg.embed_url = cli.embed_url;
    cfg.embed_service = cli.embed_service;
    cfg.speech = agent_runtime::speech::SpeechConfig {
        stt: cli.stt_bin,
        tts: cli.tts_bin,
        ffmpeg: cli.ffmpeg,
        default_locale: cli.speech_locale,
    };
    if let Some(dir) = cli.google_dir {
        cfg.google.dir = dir;
    }
    if let Some(url) = cli.agentd_url.filter(|u| !u.is_empty()) {
        let mut agentd = agent_runtime::session::AgentdConfig::new(url);
        agentd.token = match cli.agentd_token_file {
            Some(f) => Some(
                std::fs::read_to_string(&f)
                    .map_err(|e| format!("--agentd-token-file {}: {e}", f.display()))?
                    .trim()
                    .to_string(),
            ),
            None => None,
        };
        agentd.model = cli.session_model;
        cfg.agentd = Some(agentd);
    }
    if let Some(f) = cli.platform_file {
        cfg.platform = Some(agent_runtime::keys::Platform::load(&f)?);
    }
    let rt = Runtime::new(cfg)?;
    let addr = server::serve(rt.clone(), &cli.listen, token)?;
    rt.start_scheduler();
    eprintln!("agent-runtime listening on http://{addr} (state: {})", state_dir.display());
    loop {
        std::thread::park();
    }
}
