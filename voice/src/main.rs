use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use holon_voice::audio::start_mic_capture;
use holon_voice::client::RemoteAgentClient;
use holon_voice::session::{VoiceMode, VoiceSession};
use holon_voice::stt::CommandStt;
use holon_voice::tts::MacSayTts;
use holon_voice::vad::VadConfig;

#[derive(Parser, Debug)]
#[command(name = "holon-voice", about = "Native conversational voice companion for Holon on macOS")]
struct Cli {
    /// Holon runtime base URL (e.g. Raspberry Pi 5 / Zero or localhost)
    #[arg(short, long, default_value = "http://127.0.0.1:18017", env = "HOLON_URL")]
    url: String,

    /// Agent name to interact with
    #[arg(short, long, default_value = "bot", env = "HOLON_AGENT")]
    agent: String,

    /// Optional bearer auth token for the remote runtime
    #[arg(short, long, env = "HOLON_TOKEN")]
    token: Option<String>,

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

    /// Audio energy detection threshold for VAD (0.01 - 0.10)
    #[arg(long, default_value_t = 0.035)]
    energy_threshold: f32,

    /// Silence duration in milliseconds to end utterance
    #[arg(long, default_value_t = 1000)]
    silence_ms: u64,
}

fn resolve_stt_bin(configured: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(p) = configured {
        if p.exists() {
            return Ok(p);
        }
        return Err(format!("specified stt binary not found: {}", p.display()));
    }

    // Check common search paths
    let candidates =
        [PathBuf::from("./holon-stt"), PathBuf::from("agent-runtime/speech/holon-stt")];
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }

    // Try ~/.cache/holon/holon-stt
    if let Some(home) = std::env::var_os("HOME") {
        let cached = PathBuf::from(home).join(".cache/holon/holon-stt");
        if cached.exists() {
            return Ok(cached);
        }
    }

    // Attempt compiling from agent-runtime/speech/holon-stt.swift if available
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

    println!("Holon Native Voice Companion v{}", holon_voice::version());
    println!("Connecting to remote Holon runtime: {} (agent: {})", cli.url, cli.agent);

    let stt_bin = match resolve_stt_bin(cli.stt_bin) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Warning: STT binary resolution: {e}");
            eprintln!("Running with fallback stt placeholder for setup verification.");
            PathBuf::from("holon-stt")
        }
    };

    let vad_cfg = VadConfig {
        sample_rate: 16000,
        frame_size: 320,
        energy_threshold: cli.energy_threshold,
        silence_timeout_ms: cli.silence_ms,
        min_speech_duration_ms: 200,
        barge_in_consecutive_frames: 6,
    };

    let mode = if cli.always_listen { VoiceMode::AlwaysListen } else { VoiceMode::PushToTalk };

    let stt = Arc::new(CommandStt::new(stt_bin));
    let client = Arc::new(RemoteAgentClient::new(&cli.url, &cli.agent, cli.token));
    let tts = Arc::new(MacSayTts::new(&cli.voice));

    let mut session =
        VoiceSession::new(mode, &cli.wake_word, &cli.locale, vad_cfg, stt, client, tts);

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        r.store(false, Ordering::SeqCst);
    });

    if cli.always_listen {
        println!(
            "Mode: Always-Listen (say '{}' to talk, speak while playing to barge in)",
            cli.wake_word
        );
        let (tx, rx) = channel::<Vec<i16>>();
        let _stream = start_mic_capture(16000, tx)
            .map_err(|e| format!("failed to start microphone capture: {e}"))?;

        let mut frame_buf = Vec::new();
        while running.load(Ordering::SeqCst) {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(samples) => {
                    frame_buf.extend_from_slice(&samples);
                    while frame_buf.len() >= 320 {
                        let frame: Vec<i16> = frame_buf.drain(..320).collect();
                        if let Ok(Some(msg)) = session.feed_frame(&frame) {
                            println!("{msg}");
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    let _ = session.is_playback_active();
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
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
            let _stream = start_mic_capture(16000, tx)
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
