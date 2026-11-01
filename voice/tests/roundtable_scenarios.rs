use std::sync::Arc;
use std::time::Duration;

use holon_voice::client::MockDispatcher;
use holon_voice::persona::{Persona, PersonaRegistry, SpeakerTracker};
use holon_voice::session::{VoiceMode, VoiceSession};
use holon_voice::stt::MockStt;
use holon_voice::tts::MockTts;
use holon_voice::vad::VadConfig;

#[test]
fn roundtable_multi_agent_consensus_debate() {
    let debate_transcript = r#"
Rowing Coach Daniel: He did a 10k threshold row yesterday.
Let's keep today to an easy 30-minute recovery row.

Nutritionist Sarah: Hold on Daniel, his glycogen is depleted from the threshold work.
Have 40 grams of fast carbs before getting on the erg.

Rowing Coach Daniel: Good catch Sarah. Snack first, then 30 minutes at a 2:15 split.
"#;

    let stt = Arc::new(MockStt::new(vec!["hey holon review my recovery plan".into()]));
    let dispatcher = Arc::new(MockDispatcher::new(vec![debate_transcript.into()]));
    let tts = Arc::new(MockTts::with_auto_finish(1));

    let mut session = VoiceSession::new(
        VoiceMode::AlwaysListen,
        "hey holon",
        "en-US",
        VadConfig::default(),
        stt,
        Arc::clone(&dispatcher) as _,
        Arc::clone(&tts) as _,
    );

    // Audio input to trigger prompt
    let dummy_frame = vec![1200i16; 16000];
    let res = session.handle_utterance(dummy_frame).unwrap();
    assert!(res.is_some());

    // Play through the chained audio turns
    while session.is_playback_active() {
        std::thread::sleep(Duration::from_millis(5));
    }

    let spoken = tts.spoken_history();
    let personas = tts.persona_history();

    assert_eq!(spoken.len(), 3);
    assert_eq!(personas.len(), 3);

    // Turn 1: Rowing Coach Daniel
    assert_eq!(personas[0], "rowing");
    assert!(spoken[0].starts_with("Rowing Coach Daniel:"));
    assert!(spoken[0].contains("easy 30-minute recovery row"));

    // Turn 2: Nutritionist Sarah (switched speaker -> title prepended)
    assert_eq!(personas[1], "nutrition");
    assert!(spoken[1].starts_with("Nutritionist Sarah:"));
    assert!(spoken[1].contains("glycogen is depleted"));

    // Turn 3: Rowing Coach Daniel (switched back from Sarah -> title prepended)
    assert_eq!(personas[2], "rowing");
    assert!(spoken[2].starts_with("Rowing Coach Daniel:"));
    assert!(spoken[2].contains("Good catch Sarah"));
}

#[test]
fn roundtable_continuous_speaking_suppresses_redundant_title() {
    let mut tracker = SpeakerTracker::new(Duration::from_secs(10));
    let daniel = Persona::new("rowing", "Rowing Coach Daniel", "Daniel");

    // Turn 1: New speaker gets the rich title
    let (t1, is_new1) = tracker.format_speech(&daniel, "Keep your split at 1:58.");
    assert!(is_new1);
    assert_eq!(t1, "Rowing Coach Daniel: Keep your split at 1:58.");

    // Turn 2: Same speaker speaking continuously -> title is dropped!
    let (t2, is_new2) = tracker.format_speech(&daniel, "Also maintain 22 strokes per minute.");
    assert!(!is_new2);
    assert_eq!(t2, "Also maintain 22 strokes per minute.");

    // Turn 3: Still same speaker -> title is still dropped
    let (t3, is_new3) =
        tracker.format_speech(&daniel, "And focus on quick hands away from the chest.");
    assert!(!is_new3);
    assert_eq!(t3, "And focus on quick hands away from the chest.");
}

#[test]
fn roundtable_custom_personas_and_resolution() {
    let mut reg = PersonaRegistry::default();
    reg.register(Persona::new("physio", "Physio Elena", "Karen").with_rate(190));

    let resolved = reg.resolve("physio");
    assert_eq!(resolved.title, "Physio Elena");
    assert_eq!(resolved.voice, "Karen");
    assert_eq!(resolved.rate, Some(190));

    // Case insensitivity
    let resolved_upper = reg.resolve("PHYSIO");
    assert_eq!(resolved_upper.title, "Physio Elena");

    // Default fallback
    let fallback = reg.resolve("unknown_agent");
    assert_eq!(fallback.key, "bot");
}

#[test]
fn roundtable_barge_in_aborts_multi_turn_queue() {
    let multi_turn = r#"
Rowing Coach Daniel: First turn of advice.
Nutritionist Sarah: Second turn of advice.
"#;
    let stt = Arc::new(MockStt::new(vec!["hey holon plan".into()]));
    let dispatcher = Arc::new(MockDispatcher::new(vec![multi_turn.into()]));
    let tts = Arc::new(MockTts::new());

    let mut session = VoiceSession::new(
        VoiceMode::AlwaysListen,
        "hey holon",
        "en-US",
        VadConfig::default(),
        stt,
        Arc::clone(&dispatcher) as _,
        Arc::clone(&tts) as _,
    );

    session.handle_utterance(vec![1200i16; 16000]).unwrap();
    assert!(session.is_playback_active());

    // User interrupts assistant mid-dialogue
    session.interrupt_playback().unwrap();
    assert!(!session.is_playback_active());
    assert!(tts.was_last_interrupted());
}
