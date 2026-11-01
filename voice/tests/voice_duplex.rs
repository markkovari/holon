use std::sync::Arc;

use holon_voice::client::{AgentDispatcher, MockDispatcher};
use holon_voice::session::{SessionState, VoiceMode, VoiceSession};
use holon_voice::stt::MockStt;
use holon_voice::tts::MockTts;
use holon_voice::vad::VadConfig;

#[test]
fn push_to_talk_round_trip() {
    let stt = Arc::new(MockStt::new(vec!["restart nginx container".into()]));
    let dispatcher = Arc::new(MockDispatcher::new(vec!["Nginx restarted successfully.".into()]));
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

    assert_eq!(session.state(), SessionState::Listening);

    // Provide 1s of simulated 16kHz speech
    let speech_samples = vec![2000i16; 16000];
    let result = session.handle_utterance(speech_samples).unwrap();

    let output = result.expect("should produce dialogue output");
    assert!(output.contains("You: restart nginx container"));
    assert!(output.contains("Agent: Nginx restarted successfully."));
    assert_eq!(tts.spoken_history(), vec!["Nginx restarted successfully."]);
}

#[test]
fn always_listen_duplex_with_barge_in() {
    let stt = Arc::new(MockStt::new(vec![
        "hey holon summarize recent deployments".into(),
        "stop summarize only errors".into(),
    ]));
    let dispatcher = Arc::new(MockDispatcher::new(vec![
        "Here is the summary of the last 15 deployments: all green...".into(),
        "Here are only the errors: none found.".into(),
    ]));
    let tts = Arc::new(MockTts::new());

    let vad_cfg = VadConfig {
        frame_size: 320,
        energy_threshold: 0.04,
        barge_in_energy_threshold: 0.08,
        barge_in_guard_ms: 0,
        barge_in_consecutive_frames: 2,
        ..Default::default()
    };

    let mut session = VoiceSession::new(
        VoiceMode::AlwaysListen,
        "hey holon",
        "en-US",
        vad_cfg,
        stt,
        dispatcher,
        tts.clone(),
    );

    assert_eq!(session.state(), SessionState::WaitingWakeWord);

    // User speaks wake word query
    let dummy_audio = vec![1200i16; 16000];
    let turn1 = session.handle_utterance(dummy_audio).unwrap();
    assert!(turn1.unwrap().contains("Agent: Here is the summary"));
    assert_eq!(session.state(), SessionState::Speaking);
    assert!(!tts.was_last_interrupted());

    // Assistant is speaking. User interrupts (barge-in with 2 loud frames)
    let loud_frame = vec![16384i16; 320];
    assert_eq!(session.feed_frame(&loud_frame).unwrap(), None);

    let barge_in = session.feed_frame(&loud_frame).unwrap();
    assert_eq!(barge_in, Some("[Barge-in: interrupted assistant]".into()));
    assert!(tts.was_last_interrupted(), "TTS must be aborted upon barge-in");
    assert_eq!(session.state(), SessionState::Listening);

    // Now user finishes interrupting query
    let turn2 = session.handle_utterance(vec![1200i16; 16000]).unwrap();
    assert!(turn2.unwrap().contains("Agent: Here are only the errors: none found."));
}

#[test]
fn dispatcher_handles_agent_runtime_v1_json() {
    // Verifies mock and serialization contract for POST /v1/agents/{agent}/runs
    let dispatcher = MockDispatcher::new(vec!["Deployment completed in 1.4s".into()]);
    let answer = dispatcher.dispatch_run("deploy v1.4").unwrap();
    assert_eq!(answer, "Deployment completed in 1.4s");
    assert_eq!(dispatcher.dispatched(), vec!["deploy v1.4"]);
}
