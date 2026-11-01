use std::sync::Arc;

use holon_voice::audio::rms_energy;
use holon_voice::client::{AgentDispatcher, MockDispatcher};
use holon_voice::session::{SessionState, VoiceMode, VoiceSession};
use holon_voice::stt::{extract_wake_phrase, MockStt, SpeechRecognizer};
use holon_voice::tts::MockTts;
use holon_voice::vad::{VadConfig, VadEvent, VoiceActivityDetector};

// ============================================================================
// SCENARIO 1: Wake Word Accuracy & False Positive Rejection
// ============================================================================

#[test]
fn scenario_1_wake_word_exact_and_embedded_queries() {
    // Exact match
    assert_eq!(
        extract_wake_phrase("Hey Holon status check", "hey holon"),
        Some("status check".to_string())
    );

    // Mixed case and punctuation
    assert_eq!(
        extract_wake_phrase("HEY, HOLON! Restart container 4", "hey holon"),
        Some("restart container 4".to_string())
    );

    // Embedded in leading speech
    assert_eq!(
        extract_wake_phrase("Alright uh hey holon deploy release", "hey holon"),
        Some("deploy release".to_string())
    );

    // Bare wake word
    assert_eq!(extract_wake_phrase("Hey Holon", "hey holon"), Some("".to_string()));
}

#[test]
fn scenario_1_wake_word_phonetic_homophones() {
    // Known macOS Speech recognition homophones for "Hey Holon"
    assert_eq!(
        extract_wake_phrase("Hey, hold on, what is the server load?", "hey holon"),
        Some("what is the server load".to_string())
    );

    assert_eq!(
        extract_wake_phrase("Hey holland show recent logs", "hey holon"),
        Some("show recent logs".to_string())
    );

    assert_eq!(
        extract_wake_phrase("Hey holum check database status", "hey holon"),
        Some("check database status".to_string())
    );

    assert_eq!(
        extract_wake_phrase("Hey colon summarize metrics", "hey holon"),
        Some("summarize metrics".to_string())
    );

    assert_eq!(
        extract_wake_phrase("Hey whole on run diagnostics", "hey holon"),
        Some("run diagnostics".to_string())
    );
}

#[test]
fn scenario_1_false_positive_rejection() {
    // Ordinary conversational sentences containing homophone words MUST NOT trigger when wake phrase is "hey holon"
    let ordinary_speech = [
        "Can you hold on a minute please?",
        "Holland is a country in western Europe.",
        "We need to colonize the new environment.",
        "There is a hole on the road surface.",
        "Use the colon operator for Rust types.",
        "Hold on, let me think about that.",
        "Hello everyone in the meeting.",
        "Hey everyone look at this chart.",
        "How are you doing today?",
    ];

    for phrase in &ordinary_speech {
        assert_eq!(
            extract_wake_phrase(phrase, "hey holon"),
            None,
            "False positive triggered for ordinary speech: '{phrase}'"
        );
    }
}

// ============================================================================
// SCENARIO 2: Acoustic Echo Rejection & Instant Barge-In
// ============================================================================

#[test]
fn scenario_2_speaker_acoustic_feedback_rejection() {
    let mut vad = VoiceActivityDetector::new(VadConfig {
        sample_rate: 16000,
        frame_size: 320,
        energy_threshold: 0.018,
        barge_in_energy_threshold: 0.080,
        barge_in_guard_ms: 0,
        silence_timeout_ms: 500,
        min_speech_duration_ms: 100,
        barge_in_consecutive_frames: 4,
    });

    vad.set_playback_active(true);

    // Desktop speaker acoustic leakage into microphone is typically 0.03 - 0.065 RMS.
    // It is higher than idle room threshold (0.018), but below barge-in threshold (0.080).
    let echo_sample = (0.05 * 32768.0) as i16;
    let echo_frame = vec![echo_sample; 320];

    // Feed 30 frames (600ms) of acoustic speaker leakage
    for i in 0..30 {
        let event = vad.process_frame(&echo_frame);
        assert_eq!(
            event,
            VadEvent::Silence,
            "Acoustic feedback leaked through VAD at frame {i} with energy {:.3}",
            rms_energy(&echo_frame)
        );
    }

    assert!(vad.is_playback_active(), "Playback should still be active during speaker echo");
}

#[test]
fn scenario_2_barge_in_preserves_streak_frames_in_utterance() {
    let vad_cfg = VadConfig {
        sample_rate: 16000,
        frame_size: 320,
        energy_threshold: 0.018,
        barge_in_energy_threshold: 0.080,
        barge_in_guard_ms: 0,
        silence_timeout_ms: 60,     // 3 frames silence ends utterance
        min_speech_duration_ms: 60, // 3 frames minimum speech
        barge_in_consecutive_frames: 4,
    };
    let mut vad = VoiceActivityDetector::new(vad_cfg);
    vad.set_playback_active(true);

    let loud_frame = vec![16384i16; 320];
    let silence_frame = vec![0i16; 320];

    // User speaks 4 loud frames during playback:
    assert_eq!(vad.process_frame(&loud_frame), VadEvent::Silence);
    assert_eq!(vad.process_frame(&loud_frame), VadEvent::Silence);
    assert_eq!(vad.process_frame(&loud_frame), VadEvent::Silence);
    assert_eq!(vad.process_frame(&loud_frame), VadEvent::BargeIn);

    // Followed by 3 silence frames to end utterance
    assert_eq!(vad.process_frame(&silence_frame), VadEvent::Speaking);
    assert_eq!(vad.process_frame(&silence_frame), VadEvent::Speaking);

    match vad.process_frame(&silence_frame) {
        VadEvent::UtteranceComplete(samples) => {
            // Must contain all 4 speech frames + 3 trailing silence frames = 7 * 320 = 2240 samples.
            // If the streak frames were discarded, this would only be (1 + 3) * 320 = 1280 samples!
            assert_eq!(
                samples.len(),
                7 * 320,
                "Barge-in dropped the initial speech frames of the streak! Got {} samples, expected {}",
                samples.len(),
                7 * 320
            );
        }
        other => panic!("Expected UtteranceComplete, got {:?}", other),
    }
}

// ============================================================================
// SCENARIO 3: Pleasant Voice Synthesis & Markdown Sanitization
// ============================================================================

#[test]
fn scenario_3_markdown_sanitization_for_natural_voice() {
    // When the remote agent returns rich markdown formatting, the voice companion
    // must sanitize markdown markers, code blocks, URLs, and emojis so that
    // the TTS does not speak out "hashtag hashtag backtick backtick asterisks".

    // We simulate dispatching a response with markdown formatting
    let markdown_reply = "# Deployment Status\n\n**Service**: `web-api` is **HEALTHY** 🚀!\n\n```json\n{\"replicas\": 3, \"latency_ms\": 12.4}\n```\n\n- All probes passing\n- See https://holon.local/dash";

    let stt = Arc::new(MockStt::new(vec!["hey holon system status".into()]));
    let dispatcher = Arc::new(MockDispatcher::new(vec![markdown_reply.into()]));
    let tts = Arc::new(MockTts::new());

    let mut session = VoiceSession::new(
        VoiceMode::PushToTalk,
        "hey holon",
        "en-US",
        VadConfig::default(),
        stt,
        dispatcher,
        tts.clone(),
    );

    let output = session.handle_utterance(vec![1200i16; 16000]).unwrap();
    assert!(output.is_some());

    let spoken_history = tts.spoken_history();
    assert_eq!(spoken_history.len(), 1);
    let spoken_text = &spoken_history[0];

    // Check that raw markdown tokens are NOT passed to speech synthesizer
    assert!(!spoken_text.contains("```"), "Raw code fence ``` should be sanitized for speech");
    assert!(!spoken_text.contains("**"), "Markdown bold ** should be sanitized for speech");
    assert!(
        !spoken_text.contains("# Deployment"),
        "Markdown header # should be stripped for speech"
    );
}

// ============================================================================
// SCENARIO 4: Turn Cadence & Fast Endpointing
// ============================================================================

#[test]
fn scenario_4_turn_endpointing_and_noise_rejection() {
    let vad_cfg = VadConfig {
        sample_rate: 16000,
        frame_size: 320, // 20ms
        energy_threshold: 0.02,
        barge_in_energy_threshold: 0.08,
        barge_in_guard_ms: 0,
        silence_timeout_ms: 100,    // 5 frames of silence ends turn
        min_speech_duration_ms: 60, // 3 frames (60ms) minimum speech
        barge_in_consecutive_frames: 3,
    };
    let mut vad = VoiceActivityDetector::new(vad_cfg);

    let silence_frame = vec![0i16; 320];
    let speech_frame = vec![6000i16; 320];

    // 1. Noise click (1 frame / 20ms) followed by silence: must be rejected
    assert_eq!(vad.process_frame(&speech_frame), VadEvent::SpeechStart);
    for _ in 0..5 {
        let ev = vad.process_frame(&silence_frame);
        if let VadEvent::UtteranceComplete(_) = ev {
            panic!("Noise click of 20ms should NOT produce UtteranceComplete!");
        }
    }

    // 2. Legitimate utterance (4 frames speech = 80ms >= 60ms) followed by 5 frames silence (100ms)
    assert_eq!(vad.process_frame(&speech_frame), VadEvent::SpeechStart);
    assert_eq!(vad.process_frame(&speech_frame), VadEvent::Speaking);
    assert_eq!(vad.process_frame(&speech_frame), VadEvent::Speaking);
    assert_eq!(vad.process_frame(&speech_frame), VadEvent::Speaking);

    for _ in 0..4 {
        assert_eq!(vad.process_frame(&silence_frame), VadEvent::Speaking);
    }

    // 5th silence frame completes the turn
    match vad.process_frame(&silence_frame) {
        VadEvent::UtteranceComplete(samples) => {
            assert_eq!(samples.len(), (4 + 5) * 320);
        }
        other => panic!("Expected UtteranceComplete, got {:?}", other),
    }
}

// ============================================================================
// SCENARIO 5: Fault Resilience & State Recovery
// ============================================================================

struct FailingStt;
impl SpeechRecognizer for FailingStt {
    fn transcribe(&self, _wav_path: &std::path::Path, _locale: &str) -> Result<String, String> {
        Err("Simulated STT engine crash or missing binary".into())
    }
}

struct FailingDispatcher;
impl AgentDispatcher for FailingDispatcher {
    fn dispatch_run(&self, _prompt: &str) -> Result<String, String> {
        Err("502 Bad Gateway: remote agent runtime unreachable".into())
    }
}

#[test]
fn scenario_5_stt_failure_does_not_brick_session_state() {
    let stt = Arc::new(FailingStt);
    let dispatcher = Arc::new(MockDispatcher::new(vec!["ok".into()]));
    let tts = Arc::new(MockTts::new());

    let mut session = VoiceSession::new(
        VoiceMode::AlwaysListen,
        "hey holon",
        "en-US",
        VadConfig::default(),
        stt,
        dispatcher,
        tts,
    );

    assert_eq!(session.state(), SessionState::WaitingWakeWord);

    // Audio fed to session with broken STT
    let res = session.handle_utterance(vec![1000i16; 16000]);
    assert!(res.is_err(), "Session should report the STT error");

    // Session state MUST NOT remain stuck in Processing!
    assert_ne!(
        session.state(),
        SessionState::Processing,
        "Session was left bricked in Processing state after STT failure!"
    );
    assert_eq!(
        session.state(),
        SessionState::WaitingWakeWord,
        "Session should recover to WaitingWakeWord after STT failure"
    );
}

#[test]
fn scenario_5_dispatcher_failure_does_not_brick_session_state() {
    let stt = Arc::new(MockStt::new(vec!["hey holon run failing job".into()]));
    let dispatcher = Arc::new(FailingDispatcher);
    let tts = Arc::new(MockTts::new());

    let mut session = VoiceSession::new(
        VoiceMode::AlwaysListen,
        "hey holon",
        "en-US",
        VadConfig::default(),
        stt,
        dispatcher,
        tts,
    );

    assert_eq!(session.state(), SessionState::WaitingWakeWord);

    let res = session.handle_utterance(vec![1000i16; 16000]);
    assert!(res.is_err(), "Session should report the dispatch failure");

    // Session state MUST NOT remain stuck in Processing!
    assert_ne!(
        session.state(),
        SessionState::Processing,
        "Session was left bricked in Processing state after remote agent network failure!"
    );
    assert_eq!(
        session.state(),
        SessionState::WaitingWakeWord,
        "Session should recover to WaitingWakeWord after dispatcher failure"
    );
}

#[test]
fn scenario_5_empty_transcripts_and_silent_utterances_handled_cleanly() {
    let stt = Arc::new(MockStt::new(vec!["".into(), "   ".into()]));
    let dispatcher = Arc::new(MockDispatcher::new(vec!["should not be called".into()]));
    let tts = Arc::new(MockTts::new());

    let mut session = VoiceSession::new(
        VoiceMode::AlwaysListen,
        "hey holon",
        "en-US",
        VadConfig::default(),
        stt,
        dispatcher.clone(),
        tts,
    );

    // Empty audio samples
    let res1 = session.handle_utterance(vec![]).unwrap();
    assert_eq!(res1, None);
    assert_eq!(session.state(), SessionState::WaitingWakeWord);

    // Audio resulting in empty transcript
    let res2 = session.handle_utterance(vec![500i16; 3200]).unwrap();
    assert_eq!(res2, None);
    assert_eq!(session.state(), SessionState::WaitingWakeWord);

    // Audio resulting in whitespace transcript
    let res3 = session.handle_utterance(vec![500i16; 3200]).unwrap();
    assert_eq!(res3, None);
    assert_eq!(session.state(), SessionState::WaitingWakeWord);

    assert_eq!(dispatcher.dispatched().len(), 0, "No dispatch on empty audio");
}
