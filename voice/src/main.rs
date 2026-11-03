use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use holon_voice::audio::{rms_energy, start_mic_capture};
use holon_voice::client::RemoteAgentClient;
use holon_voice::control::{run_control_server, ControlState};
use holon_voice::session::{VoiceMode, VoiceSession};
use holon_voice::stt::CommandStt;
use holon_voice::tts::MacSayTts;
use holon_voice::vad::VadConfig;

#[derive(Parser, Debug)]
#[command(name = "holon-voice", about = "Native conversational voice companion for Holon on macOS")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Control server port for tray/CLI toggle (default: 18018)
    #[arg(long, default_value_t = 18018, env = "HOLON_VOICE_CONTROL_PORT")]
    control_port: u16,

    /// Start daemon in paused/muted mode (microphone released until toggled from tray or CLI)
    #[arg(long, default_value_t = false)]
    paused: bool,

    /// Holon runtime base URL (e.g. Raspberry Pi 5 / Zero or localhost)
    #[arg(short, long, default_value = "http://127.0.0.1:18017", env = "HOLON_URL")]
    url: String,

    /// Agent name to interact with
    #[arg(short, long, default_value = "bot", env = "HOLON_AGENT")]
    agent: String,

    /// Optional bearer auth token for the remote runtime
    #[arg(short, long, env = "HOLON_TOKEN")]
    token: Option<String>,

    /// Path to file containing bearer auth token
    #[arg(long)]
    token_file: Option<PathBuf>,

    /// Always-listen mode with wake-word detection (continuous duplex)
    #[arg(long, default_value_t = false)]
    always_listen: bool,

    /// Wake phrase for always-listen mode
    #[arg(long, default_value = "hey holon")]
    wake_word: String,

    /// macOS system voice for text-to-speech
    #[arg(long, default_value = "Samantha")]
    voice: String,

    /// Recognition language locale
    #[arg(long, default_value = "en-US")]
    locale: String,

    /// Path to compiled speech-to-text binary (holon-stt)
    #[arg(long)]
    stt_bin: Option<PathBuf>,

    /// Path to custom text-to-speech binary (<bin> <voice> <text>)
    #[arg(long)]
    tts_bin: Option<PathBuf>,

    /// Audio energy detection threshold for VAD (0.005 - 0.050)
    #[arg(long, default_value_t = 0.018)]
    energy_threshold: f32,

    /// Silence duration in milliseconds to end utterance
    #[arg(long, default_value_t = 1000)]
    silence_ms: u64,

    /// Specific audio input device name substring (e.g. 'Scarlett', 'iPhone')
    #[arg(long)]
    device: Option<String>,

    /// Barge-in energy threshold while assistant is speaking (prevents speaker self-interruption)
    #[arg(long, default_value_t = 0.080)]
    barge_in_threshold: f32,

    /// Disable barge-in interruption completely during speech playback
    #[arg(long, default_value_t = false)]
    no_barge_in: bool,

    /// Enable live audio level meter output in console
    #[arg(long, default_value_t = false)]
    meter: bool,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Toggle microphone listening state (on <-> off)
    Toggle,
    /// Pause microphone capture (release hardware)
    Pause,
    /// Resume microphone capture
    Resume,
    /// Get current voice companion status
    Status,
    /// Switch active default agent
    Agent {
        /// Target agent name (e.g. coach, bot, calendar, weather)
        name: String,
    },
}

fn resolve_stt_bin(configured: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(p) = configured {
        if p.exists() {
            return Ok(p);
        }
        return Err(format!("specified stt binary not found: {}", p.display()));
    }

    let candidates = [
        PathBuf::from("/Users/markkovari/.holon-agents/holon-stt"),
        PathBuf::from("./holon-stt"),
        PathBuf::from("agent-runtime/speech/holon-stt"),
    ];
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        let cached = PathBuf::from(home).join(".cache/holon/holon-stt");
        if cached.exists() {
            return Ok(cached);
        }
    }

    let swift_src = Path::new("agent-runtime/speech/holon-stt.swift");
    if swift_src.exists() {
        let out_dir = std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".cache/holon"))
            .unwrap_or_else(|| PathBuf::from("."));
        let _ = std::fs::create_dir_all(&out_dir);
        let out_bin = out_dir.join("holon-stt");

        eprintln!(
            "Compiling native STT helper ({}) -> {}...",
            swift_src.display(),
            out_bin.display()
        );
        let status = Command::new("swiftc")
            .arg("-parse-as-library")
            .arg("-O")
            .arg("-o")
            .arg(&out_bin)
            .arg(swift_src)
            .status();

        if let Ok(s) = status {
            if s.success() && out_bin.exists() {
                return Ok(out_bin);
            }
        }
    }

    Err("no holon-stt binary found (compile agent-runtime/speech/holon-stt.swift or specify --stt-bin)".into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Handle CLI subcommands if specified
    if let Some(cmd) = cli.command {
        let base = format!("http://127.0.0.1:{}", cli.control_port);
        let client = reqwest::Client::new();
        match cmd {
            Commands::Toggle => {
                let resp = client.post(format!("{base}/toggle")).send().await?;
                println!("{}", resp.text().await?);
            }
            Commands::Pause => {
                let resp = client.post(format!("{base}/pause")).send().await?;
                println!("{}", resp.text().await?);
            }
            Commands::Resume => {
                let resp = client.post(format!("{base}/resume")).send().await?;
                println!("{}", resp.text().await?);
            }
            Commands::Status => {
                let resp = client.get(format!("{base}/status")).send().await?;
                println!("{}", resp.text().await?);
            }
            Commands::Agent { name } => {
                let resp = client.post(format!("{base}/agent?name={name}")).send().await?;
                println!("{}", resp.text().await?);
            }
        }
        return Ok(());
    }

    println!("Holon Native Voice Companion v{}", holon_voice::version());
    println!("Connecting to remote Holon runtime: {} (agent: {})", cli.url, cli.agent);

    let stt_bin = match resolve_stt_bin(cli.stt_bin) {
        Ok(b) => {
            println!("STT Engine: Apple Neural Engine ({})", b.display());
            b
        }
        Err(e) => {
            eprintln!("Warning: STT binary resolution: {e}");
            eprintln!("Running with fallback stt placeholder for setup verification.");
            PathBuf::from("holon-stt")
        }
    };

    let barge_thresh = if cli.no_barge_in { 999.0 } else { cli.barge_in_threshold };

    let vad_cfg = VadConfig {
        sample_rate: 16000,
        frame_size: 320,
        energy_threshold: cli.energy_threshold,
        barge_in_energy_threshold: barge_thresh,
        barge_in_guard_ms: 800,
        silence_timeout_ms: cli.silence_ms,
        min_speech_duration_ms: 200,
        barge_in_consecutive_frames: 8,
    };

    let mode = if cli.always_listen { VoiceMode::AlwaysListen } else { VoiceMode::PushToTalk };

    let stt = Arc::new(CommandStt::new(stt_bin));
    let token = cli.token.or_else(|| {
        cli.token_file.and_then(|p| std::fs::read_to_string(p).ok().map(|s| s.trim().to_string()))
    });
    let client = Arc::new(RemoteAgentClient::new(&cli.url, &cli.agent, token));
    let is_kokoro =
        cli.tts_bin.as_ref().map(|b| b.to_string_lossy().contains("kokoro")).unwrap_or(false);
    let tts: Arc<dyn holon_voice::tts::SpeechSynthesizer> = if let Some(bin) = cli.tts_bin {
        println!("TTS Engine: Custom binary ({})", bin.display());
        Arc::new(holon_voice::tts::CustomCommandTts::new(bin, &cli.voice))
    } else {
        Arc::new(MacSayTts::new(&cli.voice))
    };

    let mut session =
        VoiceSession::new(mode, &cli.wake_word, &cli.locale, vad_cfg, stt, client, tts);
    session.set_default_agent(&cli.agent);
    if is_kokoro {
        session.set_personas(holon_voice::persona::PersonaRegistry::kokoro_default());
    }

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        r.store(false, Ordering::SeqCst);
    });

    let initial_listening = cli.always_listen && !cli.paused;
    let control_state = Arc::new(ControlState::new(
        initial_listening,
        &cli.agent,
        cli.device.clone().unwrap_or_else(|| "Default".to_string()),
    ));

    let ctrl_for_listener = Arc::clone(&control_state);
    let rt_handle = tokio::runtime::Handle::current();
    session.set_state_listener(move |state, agent| {
        let cs = Arc::clone(&ctrl_for_listener);
        let state_str = match state {
            holon_voice::session::SessionState::WaitingWakeWord => "WaitingWakeWord",
            holon_voice::session::SessionState::Listening => "Listening",
            holon_voice::session::SessionState::Processing => "Processing",
            holon_voice::session::SessionState::Speaking => "Speaking",
        };
        let agent_opt = agent.map(|s| s.to_string());
        rt_handle.spawn(async move {
            cs.set_state_and_active_agent(state_str, agent_opt.as_deref()).await;
        });
    });

    let ctrl_clone = Arc::clone(&control_state);
    let ctrl_port = cli.control_port;
    tokio::spawn(async move {
        if let Err(e) = run_control_server(ctrl_port, ctrl_clone).await {
            eprintln!("Control server error: {e}");
        }
    });

    if cli.always_listen {
        println!(
            "Mode: Always-Listen (say '{}' to talk, speak while playing to barge in)",
            cli.wake_word
        );
        if cli.paused {
            println!("🔇 Starting in PAUSED mode (mic released). Toggle via tray or `holon-voice toggle`.");
        }

        let mut mic_stream: Option<holon_voice::audio::AudioStream> = None;
        let mut rx_channel: Option<std::sync::mpsc::Receiver<Vec<i16>>> = None;
        let mut frame_buf = Vec::new();
        let mut last_meter = Instant::now();
        let mut max_energy: f32 = 0.0;
        let mut voice_active = false;
        let mut current_agent = cli.agent.clone();

        while running.load(Ordering::SeqCst) {
            // Check dynamic agent switch from tray
            let desired_agent = control_state.agent.read().await.clone();
            if desired_agent != current_agent {
                session.set_default_agent(&desired_agent);
                current_agent = desired_agent;
            }

            let should_listen = control_state.listening.load(Ordering::SeqCst);

            if should_listen && mic_stream.is_none() {
                let (tx, rx) = channel::<Vec<i16>>();
                match start_mic_capture(16000, cli.device.as_deref(), tx) {
                    Ok(stream) => {
                        mic_stream = Some(stream);
                        rx_channel = Some(rx);
                        frame_buf.clear();
                        eprintln!("🎙️  [Mic Activated]: Hardware stream started");
                    }
                    Err(e) => {
                        eprintln!("⚠️  Failed to start microphone: {e}");
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        continue;
                    }
                }
            } else if !should_listen && mic_stream.is_some() {
                if let Some(mut stream) = mic_stream.take() {
                    stream.stop();
                }
                rx_channel = None;
                frame_buf.clear();
                voice_active = false;
                let _ = session.interrupt_playback();
                eprintln!("🔇 [Mic Released]: Hardware capture stopped (orange indicator off)");
            }

            if should_listen {
                if let Some(ref rx) = rx_channel {
                    match rx.recv_timeout(Duration::from_millis(50)) {
                        Ok(samples) => {
                            let energy = rms_energy(&samples);
                            if energy > max_energy {
                                max_energy = energy;
                            }

                            if energy >= cli.energy_threshold {
                                if !voice_active {
                                    voice_active = true;
                                    eprintln!(
                                        "🎙️  [VAD: User voice detected (energy: {:.3})]...",
                                        energy
                                    );
                                    let cs = Arc::clone(&control_state);
                                    tokio::spawn(async move {
                                        cs.set_state_and_active_agent("HearingSpeech", None).await;
                                    });
                                }
                            } else if voice_active && energy < cli.energy_threshold / 2.0 {
                                voice_active = false;
                            }

                            if cli.meter && last_meter.elapsed() >= Duration::from_secs(2) {
                                eprintln!(
                                    "📊 [Mic Meter]: peak RMS={:.4} (threshold={:.3})",
                                    max_energy, cli.energy_threshold
                                );
                                max_energy = 0.0;
                                last_meter = Instant::now();
                            }

                            frame_buf.extend_from_slice(&samples);
                            while frame_buf.len() >= 320 {
                                let frame: Vec<i16> = frame_buf.drain(..320).collect();
                                if let Ok(Some(msg)) = session.feed_frame(&frame) {
                                    println!("{msg}");
                                    if let Some((u, a)) = msg.split_once("\nAgent: ") {
                                        let query = u.trim_start_matches("You: ");
                                        control_state.update_turn(query, a).await;
                                    }
                                }
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            let _ = session.is_playback_active();
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            if let Some(mut stream) = mic_stream.take() {
                                stream.stop();
                            }
                            rx_channel = None;
                        }
                    }
                } else {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            } else {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    } else {
        println!("Mode: Push-to-Talk");
        println!("Press [ENTER] to start speaking. Press [ENTER] again to stop and dispatch.");
        let stdin = io::stdin();
        let mut lines = stdin.lock().lines();

        while running.load(Ordering::SeqCst) {
            println!("\nReady. Press [ENTER] to record...");
            if lines.next().is_none() {
                break;
            }

            println!("* Recording... Press [ENTER] to stop *");
            let (tx, rx) = channel::<Vec<i16>>();
            let _stream = start_mic_capture(16000, cli.device.as_deref(), tx)
                .map_err(|e| format!("failed to start microphone capture: {e}"))?;

            let mut recorded = Vec::new();
            let recording = Arc::new(AtomicBool::new(true));
            let rec_flag = recording.clone();

            let drain_thread = std::thread::spawn(move || {
                let mut buf = Vec::new();
                while rec_flag.load(Ordering::SeqCst) {
                    if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(50)) {
                        buf.extend_from_slice(&chunk);
                    }
                }
                while let Ok(chunk) = rx.try_recv() {
                    buf.extend_from_slice(&chunk);
                }
                buf
            });

            let _ = lines.next();
            recording.store(false, Ordering::SeqCst);
            if let Ok(buf) = drain_thread.join() {
                recorded = buf;
            }

            println!("Processing {} audio samples...", recorded.len());
            match session.handle_utterance(recorded) {
                Ok(Some(reply)) => println!("{reply}"),
                Ok(None) => println!("(no speech recognised)"),
                Err(e) => eprintln!("Error: {e}"),
            }
        }
    }

    println!("\nHolon voice companion terminated.");
    Ok(())
}
