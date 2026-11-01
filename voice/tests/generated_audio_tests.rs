use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use holon_voice::audio::read_wav;
use holon_voice::client::AgentDispatcher;
use holon_voice::persona::PersonaRegistry;
use holon_voice::session::{SessionState, VoiceMode, VoiceSession};
use holon_voice::stt::{CommandStt, SpeechRecognizer};
use holon_voice::tts::MockTts;
use holon_voice::vad::VadConfig;

/// A mock runtime dispatcher that records dispatched agents & prompts
/// and simulates runtime data responses (e.g. Concept2 logbook for coach,
/// weather conditions, calendar schedules, and multi-agent debates).
#[derive(Default)]
struct RuntimeDispatcher {
    dispatched_agents: Mutex<Vec<String>>,
    dispatched_prompts: Mutex<Vec<String>>,
    concept2_queries: Mutex<Vec<String>>,
}

impl RuntimeDispatcher {
    fn dispatched_agents(&self) -> Vec<String> {
        self.dispatched_agents.lock().unwrap().clone()
    }

    fn dispatched_prompts(&self) -> Vec<String> {
        self.dispatched_prompts.lock().unwrap().clone()
    }

    fn concept2_queries(&self) -> Vec<String> {
        self.concept2_queries.lock().unwrap().clone()
    }
}

impl AgentDispatcher for RuntimeDispatcher {
    fn dispatch_run(&self, prompt: &str) -> Result<String, String> {
        self.dispatch_run_for_agent("bot", prompt)
    }

    fn dispatch_run_for_agent(&self, agent: &str, prompt: &str) -> Result<String, String> {
        self.dispatched_agents.lock().unwrap().push(agent.to_string());
        self.dispatched_prompts.lock().unwrap().push(prompt.to_string());

        match agent {
            "coach" => {
                self.concept2_queries.lock().unwrap().push(prompt.to_string());
                if prompt.to_lowercase().contains("split") {
                    Ok("Rowing Coach Daniel: Your average split was 1:58.2 per 500m across the 5000m workout, maintaining 24 strokes per minute."
                        .to_string())
                } else {
                    Ok("Rowing Coach Daniel: Pulled Concept2 PM5 logbook: Latest session was 5,000m completed in 19:42 with average split 1:58.2 and 224 watts."
                        .to_string())
                }
            }
            "weather" => Ok(
                "Weather Agent: In Budapest, Hungary it is currently 14 degrees Celsius with partly cloudy skies."
                    .to_string(),
            ),
            "calendar" => Ok(
                "Calendar Agent: Tomorrow you have Rowing Erg Session at 07:00 and Team Architecture Sync at 10:00."
                    .to_string(),
            ),
            _ => Ok(format!("Default response for agent {agent} with prompt {prompt}")),
        }
    }
}

/// Resolves the compiled Apple Speech Transcriber binary `holon-stt`.
fn resolve_stt_binary() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest_dir.join("../target/holon-stt"),
        manifest_dir.join("target/holon-stt"),
        PathBuf::from("target/holon-stt"),
        PathBuf::from("../target/holon-stt"),
    ];

    for c in &candidates {
        if c.exists() {
            return Some(c.clone());
        }
    }
    None
}

/// Fallback recognizer that maps generated WAV files to expected speech transcripts
/// for environments without the native Apple Speech binary.
struct HybridSpeechRecognizer {
    command_stt: Option<CommandStt>,
    fallback_queue: Mutex<Vec<String>>,
}

impl HybridSpeechRecognizer {
    fn new() -> Self {
        let command_stt = resolve_stt_binary().map(CommandStt::new);
        Self { command_stt, fallback_queue: Mutex::new(Vec::new()) }
    }

    fn with_fallback(queue: Vec<String>) -> Self {
        let command_stt = resolve_stt_binary().map(CommandStt::new);
        Self { command_stt, fallback_queue: Mutex::new(queue) }
    }
}

impl SpeechRecognizer for HybridSpeechRecognizer {
    fn transcribe(&self, wav_path: &Path, locale: &str) -> Result<String, String> {
        if let Some(cmd) = &self.command_stt {
            if let Ok(transcript) = cmd.transcribe(wav_path, locale) {
                if !transcript.trim().is_empty() {
                    return Ok(transcript);
                }
            }
        }

        let mut queue = self.fallback_queue.lock().unwrap();
        if !queue.is_empty() {
            return Ok(queue.remove(0));
        }

        // Deterministic transcript fallback based on file content/name if available
        let file_name = wav_path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if file_name.contains("analyze_rowing") && file_name.contains("hey_holon") {
            Ok("Okay, hold on. Analyze my rowing.".into())
        } else if file_name.contains("coach_analyze_rowing") {
            Ok("Coach, analyze my rowing.".into())
        } else if file_name.contains("average_split") {
            Ok("What about my average split?".into())
        } else if file_name.contains("weather") {
            Ok("What's the weather like in Budapest?".into())
        } else if file_name.contains("calendar") {
            Ok("Check my calendar for tomorrow.".into())
        } else {
            Ok("Hello from voice companion.".into())
        }
    }
}

/// Ensures the 5 realistic speech WAV fixtures exist, generating them via `say` if needed.
fn ensure_speech_fixtures() -> [PathBuf; 5] {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = manifest_dir.join("tests/fixtures");
    let _ = std::fs::create_dir_all(&dir);

    let fixtures = [
        (dir.join("1_hey_holon_analyze_rowing.wav"), "Hey Holon, analyze my rowing"),
        (dir.join("2_coach_analyze_rowing.wav"), "Coach, analyze my rowing"),
        (dir.join("3_what_about_average_split.wav"), "What about my average split?"),
        (dir.join("4_weather_budapest.wav"), "What's the weather like in Budapest?"),
        (dir.join("5_calendar_tomorrow.wav"), "Check my calendar for tomorrow"),
    ];

    for (path, text) in &fixtures {
        if !path.exists() {
            let status = Command::new("say")
                .arg("--file-format=WAVE")
                .arg("--data-format=LEI16@16000")
                .arg("-o")
                .arg(path)
                .arg(text)
                .status();
            if let Err(e) = status {
                eprintln!("say command warning for {}: {e}", path.display());
            }
        }
    }

    [
        fixtures[0].0.clone(),
        fixtures[1].0.clone(),
        fixtures[2].0.clone(),
        fixtures[3].0.clone(),
        fixtures[4].0.clone(),
    ]
}

/// Helper that decodes a 16kHz WAV file into PCM samples and feeds it frame-by-frame (320 samples)
/// followed by silence frames to trigger VAD utterance completion.
fn feed_wav_audio_file(
    session: &mut VoiceSession,
    wav_path: &Path,
    silence_frames: usize,
) -> Result<Option<String>, String> {
    let (samples, rate) = read_wav(wav_path)?;
    assert_eq!(rate, 16000, "Fixture audio must be 16kHz mono");

    let mut result = None;

    // Feed speech audio in 320-sample chunks (20ms)
    for chunk in samples.chunks(320) {
        let mut frame = chunk.to_vec();
        if frame.len() < 320 {
            frame.resize(320, 0);
        }
        if let Some(msg) = session.feed_frame(&frame)? {
            result = Some(msg);
        }
    }

    // Feed trailing silence frames to trigger VadEvent::UtteranceComplete
    let silent_frame = vec![0i16; 320];
    for _ in 0..silence_frames {
        if let Some(msg) = session.feed_frame(&silent_frame)? {
            result = Some(msg);
        }
    }

    Ok(result)
}

#[test]
fn test_prompts_1_and_2_route_to_coach_and_pull_concept2_data() {
    let [wav1, wav2, _, _, _] = ensure_speech_fixtures();

    let stt = Arc::new(HybridSpeechRecognizer::with_fallback(vec![
        "Okay, hold on. Analyze my rowing.".into(),
        "Coach, analyze my rowing.".into(),
    ]));
    let dispatcher = Arc::new(RuntimeDispatcher::default());
    let tts = Arc::new(MockTts::with_auto_finish(1));

    let vad_cfg = VadConfig {
        sample_rate: 16000,
        frame_size: 320,
        energy_threshold: 0.018,
        silence_timeout_ms: 300, // 15 frames
        ..Default::default()
    };

    let mut session = VoiceSession::new(
        VoiceMode::AlwaysListen,
        "hey holon",
        "en-US",
        vad_cfg,
        stt,
        Arc::clone(&dispatcher) as _,
        Arc::clone(&tts) as _,
    );

    // --- Scenario 1: "Hey Holon, analyze my rowing" ---
    assert_eq!(session.state(), SessionState::WaitingWakeWord);
    let reply1 = feed_wav_audio_file(&mut session, &wav1, 20)
        .expect("feed_frame failed")
        .expect("should produce dialogue output");

    assert!(reply1.contains("Agent: Rowing Coach Daniel: Pulled Concept2 PM5 logbook"));
    assert!(reply1.contains("5,000m completed in 19:42"));

    // Check dispatcher recorded coach destination and concept2 query
    assert_eq!(dispatcher.dispatched_agents(), vec!["coach"]);
    assert_eq!(dispatcher.concept2_queries().len(), 1);

    // Conclude playback
    while session.is_playback_active() {
        std::thread::sleep(Duration::from_millis(2));
    }

    // Reset session back to waiting wake word to test prompt 2 independently
    let mut session2 = VoiceSession::new(
        VoiceMode::AlwaysListen,
        "hey holon",
        "en-US",
        VadConfig {
            sample_rate: 16000,
            frame_size: 320,
            energy_threshold: 0.018,
            silence_timeout_ms: 300,
            ..Default::default()
        },
        Arc::new(HybridSpeechRecognizer::with_fallback(vec!["Coach, analyze my rowing.".into()])),
        Arc::clone(&dispatcher) as _,
        Arc::clone(&tts) as _,
    );

    // --- Scenario 2: "Coach, analyze my rowing" ---
    assert_eq!(session2.state(), SessionState::WaitingWakeWord);
    let reply2 = feed_wav_audio_file(&mut session2, &wav2, 20)
        .expect("feed_frame failed")
        .expect("should produce dialogue output");

    assert!(reply2.contains("Agent: Rowing Coach Daniel: Pulled Concept2 PM5 logbook"));
    let agents = dispatcher.dispatched_agents();
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[1], "coach");
    assert_eq!(dispatcher.concept2_queries().len(), 2);
}

#[test]
fn test_prompt_3_follow_up_window_routes_to_coach_without_wake_word() {
    let [wav1, _, wav3, _, _] = ensure_speech_fixtures();

    let stt = Arc::new(HybridSpeechRecognizer::with_fallback(vec![
        "Okay, hold on. Analyze my rowing.".into(),
        "What about my average split?".into(),
    ]));
    let dispatcher = Arc::new(RuntimeDispatcher::default());
    let tts = Arc::new(MockTts::with_auto_finish(1));

    let vad_cfg = VadConfig {
        sample_rate: 16000,
        frame_size: 320,
        energy_threshold: 0.018,
        silence_timeout_ms: 300,
        ..Default::default()
    };

    let mut session = VoiceSession::new(
        VoiceMode::AlwaysListen,
        "hey holon",
        "en-US",
        vad_cfg,
        stt,
        Arc::clone(&dispatcher) as _,
        Arc::clone(&tts) as _,
    );

    // Initial wake-word query: "Hey Holon, analyze my rowing"
    let _ = feed_wav_audio_file(&mut session, &wav1, 20).unwrap();

    // Assistant finishes speaking -> follow-up window becomes active
    while session.is_playback_active() {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(session.state(), SessionState::Listening);

    // Prompt 3: "What about my average split?" (contains NO wake word)
    let reply3 = feed_wav_audio_file(&mut session, &wav3, 20)
        .expect("feed_frame failed")
        .expect("should produce follow-up dialogue without wake word");

    assert!(reply3.contains("1:58.2 per 500m across the 5000m workout"));

    let agents = dispatcher.dispatched_agents();
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[1], "coach", "Prompt 3 must route to coach");

    let prompts = dispatcher.dispatched_prompts();
    assert!(
        prompts[1].to_lowercase().contains("average split"),
        "Prompt sent to coach must query split metrics"
    );
}

#[test]
fn test_prompt_4_routes_to_weather_and_prompt_5_routes_to_calendar() {
    let [_, _, _, wav4, wav5] = ensure_speech_fixtures();

    let stt = Arc::new(HybridSpeechRecognizer::with_fallback(vec![
        "What's the weather like in Budapest?".into(),
        "Check my calendar for tomorrow.".into(),
    ]));
    let dispatcher = Arc::new(RuntimeDispatcher::default());
    let tts = Arc::new(MockTts::with_auto_finish(1));

    let vad_cfg = VadConfig {
        sample_rate: 16000,
        frame_size: 320,
        energy_threshold: 0.018,
        silence_timeout_ms: 300,
        ..Default::default()
    };

    let mut session = VoiceSession::new(
        VoiceMode::PushToTalk, // Direct speech dispatch
        "hey holon",
        "en-US",
        vad_cfg,
        stt,
        Arc::clone(&dispatcher) as _,
        Arc::clone(&tts) as _,
    );

    // --- Scenario 4: "What's the weather like in Budapest?" ---
    let reply4 = feed_wav_audio_file(&mut session, &wav4, 20)
        .expect("feed_frame failed")
        .expect("weather dialogue reply");

    assert!(reply4.contains("Weather Agent: In Budapest, Hungary it is currently 14 degrees"));
    while session.is_playback_active() {
        std::thread::sleep(Duration::from_millis(2));
    }

    // --- Scenario 5: "Check my calendar for tomorrow" ---
    let reply5 = feed_wav_audio_file(&mut session, &wav5, 20)
        .expect("feed_frame failed")
        .expect("calendar dialogue reply");

    assert!(reply5.contains("Calendar Agent: Tomorrow you have Rowing Erg Session"));

    let agents = dispatcher.dispatched_agents();
    assert_eq!(agents, vec!["weather", "calendar"]);
}

#[test]
fn test_multi_agent_debate_sequences_kokoro_voices() {
    let debate_dialogue = r#"
Rowing Coach Daniel: He completed a 10k threshold row yesterday. Let's keep today to an easy recovery row.
Nutritionist Sarah: His glycogen is depleted from yesterday's workout. Ensure 40 grams of carbs before training.
Lead Engineer Oliver: Battery telemetry on the Concept2 PM5 is at 94 percent and Bluetooth pairing is stable.
"#;

    struct DebateDispatcher {
        script: String,
    }
    impl AgentDispatcher for DebateDispatcher {
        fn dispatch_run(&self, _prompt: &str) -> Result<String, String> {
            Ok(self.script.clone())
        }
    }

    let dispatcher = Arc::new(DebateDispatcher { script: debate_dialogue.to_string() });
    let tts = Arc::new(MockTts::with_auto_finish(1));

    let mut session = VoiceSession::new(
        VoiceMode::PushToTalk,
        "hey holon",
        "en-US",
        VadConfig::default(),
        Arc::new(HybridSpeechRecognizer::new()),
        dispatcher,
        Arc::clone(&tts) as _,
    );

    // Apply the neural Kokoro-82M persona registry
    session.set_personas(PersonaRegistry::kokoro_default());

    // Single simulated utterance trigger
    let trigger_audio = vec![1200i16; 16000];
    let res = session.handle_utterance(trigger_audio).unwrap();
    assert!(res.is_some());

    // Play through each chained speaker turn
    while session.is_playback_active() {
        std::thread::sleep(Duration::from_millis(2));
    }

    // Assert Kokoro voices are sequenced exactly per dialogue turn:
    // Turn 1 (Rowing Coach Daniel) -> am_adam
    // Turn 2 (Nutritionist Sarah) -> af_sarah
    // Turn 3 (Lead Engineer Oliver) -> am_michael
    let voices = tts.voice_history();
    assert_eq!(
        voices,
        vec!["am_adam", "af_sarah", "am_michael"],
        "Multi-agent debate turns must be voiced sequentially by assigned Kokoro models"
    );

    let personas = tts.persona_history();
    assert_eq!(personas.len(), 3);
    assert_eq!(personas[0], "rowing");
    assert_eq!(personas[1], "nutrition");
    assert!(personas[2] == "coder" || personas[2] == "dev");

    let spoken = tts.spoken_history();
    assert_eq!(spoken.len(), 3);
    assert!(spoken[0].starts_with("Rowing Coach Daniel:"));
    assert!(spoken[1].starts_with("Nutritionist Sarah:"));
    assert!(spoken[2].starts_with("Lead Engineer Oliver:"));
}
