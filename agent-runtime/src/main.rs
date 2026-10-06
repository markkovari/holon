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
    /// Speech-to-text command: `<bin> <audio.wav> <locale>` prints the transcript. On a
    /// Mac, build `speech/holon-stt.swift` (see its header). Without it, transcription is off.
    #[arg(long, env = "HOLON_STT_BIN")]
    stt_bin: Option<PathBuf>,
    /// Text-to-speech override: `<bin> <out-audio-file> <voice>`, text on stdin. By
    /// default the system `say` is used.
    #[arg(long, env = "HOLON_TTS_BIN")]
    tts_bin: Option<PathBuf>,
    #[arg(long, default_value = "ffmpeg")]
    ffmpeg: String,
    /// Recognition language when a caller does not say.
    #[arg(long, default_value = "en-US")]
    speech_locale: String,
}

fn main() -> Result<(), String> {
    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.state_dir).map_err(|e| e.to_string())?;
    let token = server::admin_token_in(&cli.state_dir)?;

    let mut cfg = Config::new(&cli.state_dir);
    cfg.local = LocalModel { base_url: cli.local_url, model: cli.local_model };
    cfg.otlp_endpoint = cli.otlp_endpoint.filter(|e| !e.is_empty());
    cfg.speech = agent_runtime::speech::SpeechConfig {
        stt: cli.stt_bin,
        tts: cli.tts_bin,
        ffmpeg: cli.ffmpeg,
        default_locale: cli.speech_locale,
    };
    let rt = Runtime::new(cfg)?;
    let addr = server::serve(rt.clone(), &cli.listen, token)?;
    rt.start_scheduler();
    eprintln!("agent-runtime listening on http://{addr} (state: {})", cli.state_dir.display());
    loop {
        std::thread::park();
    }
}
