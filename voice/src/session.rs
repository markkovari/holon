use std::sync::Arc;

use crate::audio::write_wav;
use crate::client::AgentDispatcher;
use crate::stt::{extract_wake_phrase, SpeechRecognizer};
use crate::tts::{ActivePlayback, SpeechSynthesizer};
use crate::vad::{VadConfig, VadEvent, VoiceActivityDetector};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceMode {
    PushToTalk,
    AlwaysListen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    WaitingWakeWord,
    Listening,
    Processing,
    Speaking,
}

pub struct VoiceSession {
    mode: VoiceMode,
    wake_phrase: String,
    locale: String,
    vad: VoiceActivityDetector,
    stt: Arc<dyn SpeechRecognizer>,
    dispatcher: Arc<dyn AgentDispatcher>,
    tts: Arc<dyn SpeechSynthesizer>,
    state: SessionState,
    wake_word_active: bool,
    active_playback: Option<Box<dyn ActivePlayback>>,
}

impl VoiceSession {
    pub fn new(
        mode: VoiceMode,
        wake_phrase: impl Into<String>,
        locale: impl Into<String>,
        vad_config: VadConfig,
        stt: Arc<dyn SpeechRecognizer>,
        dispatcher: Arc<dyn AgentDispatcher>,
        tts: Arc<dyn SpeechSynthesizer>,
    ) -> Self {
        let initial_state = match mode {
            VoiceMode::PushToTalk => SessionState::Listening,
            VoiceMode::AlwaysListen => SessionState::WaitingWakeWord,
        };

        Self {
            mode,
            wake_phrase: wake_phrase.into(),
            locale: locale.into(),
            vad: VoiceActivityDetector::new(vad_config),
            stt,
            dispatcher,
            tts,
            state: initial_state,
            wake_word_active: false,
            active_playback: None,
        }
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    pub fn is_playback_active(&mut self) -> bool {
        if let Some(playback) = self.active_playback.as_mut() {
            let active = playback.is_active();
            if !active {
                self.active_playback = None;
                self.vad.set_playback_active(false);
                self.wake_word_active = false;
                if self.state == SessionState::Speaking {
                    self.state = match self.mode {
                        VoiceMode::PushToTalk => SessionState::Listening,
                        VoiceMode::AlwaysListen => SessionState::WaitingWakeWord,
                    };
                }
            }
            active
        } else {
            false
        }
    }

    /// Stops any ongoing TTS speech immediately (barge-in).
    pub fn interrupt_playback(&mut self) -> Result<(), String> {
        if let Some(mut playback) = self.active_playback.take() {
            playback.stop()?;
        }
        self.vad.set_playback_active(false);
        self.wake_word_active = true;
        self.state = SessionState::Listening;
        Ok(())
    }

    /// Feeds an audio frame into the session and processes transitions.
    pub fn feed_frame(&mut self, frame: &[i16]) -> Result<Option<String>, String> {
        let _ = self.is_playback_active();
        let event = self.vad.process_frame(frame);

        match event {
            VadEvent::BargeIn => {
                let _ = self.interrupt_playback();
                Ok(Some("[Barge-in: interrupted assistant]".into()))
            }
            VadEvent::SpeechStart => {
                if self.state == SessionState::WaitingWakeWord {
                    self.state = SessionState::Listening;
                }
                Ok(None)
            }
            VadEvent::Speaking => Ok(None),
            VadEvent::UtteranceComplete(samples) => self.handle_utterance(samples),
            VadEvent::Silence => Ok(None),
        }
    }

    /// Process a completed audio utterance.
    pub fn handle_utterance(&mut self, samples: Vec<i16>) -> Result<Option<String>, String> {
        if samples.is_empty() {
            return Ok(None);
        }

        self.state = SessionState::Processing;

        // Persist audio to secure temporary WAV for local STT recognizer
        let temp_wav = tempfile::Builder::new()
            .prefix("holon-voice-")
            .suffix(".wav")
            .tempfile()
            .map_err(|e| format!("failed to create temporary audio file: {e}"))?;
        let wav_path = temp_wav.path();
        write_wav(&samples, 16000, wav_path)?;

        let transcript = self.stt.transcribe(wav_path, &self.locale)?;
        let _ = temp_wav.close();

        if !transcript.trim().is_empty() {
            eprintln!("🎤 [Speech Recognizer Heard]: \"{transcript}\"");
        }

        let query = if self.mode == VoiceMode::AlwaysListen {
            if !self.wake_word_active {
                match extract_wake_phrase(&transcript, &self.wake_phrase) {
                    Some(rest) => {
                        self.wake_word_active = true;
                        eprintln!("⚡ [Wake Word Matched!]: Processing prompt...");
                        if rest.trim().is_empty() {
                            self.state = SessionState::Listening;
                            return Ok(Some(format!(
                                "Heard wake phrase '{}', listening...",
                                self.wake_phrase
                            )));
                        }
                        rest
                    }
                    None => {
                        self.state = SessionState::WaitingWakeWord;
                        eprintln!(
                            "⏳ [Wake Word Missing]: Heard \"{transcript}\" (waiting for '{}')",
                            self.wake_phrase
                        );
                        return Ok(None);
                    }
                }
            } else if let Some(rest) = extract_wake_phrase(&transcript, &self.wake_phrase) {
                if rest.trim().is_empty() {
                    return Ok(Some("Still listening for your question...".to_string()));
                }
                rest
            } else {
                transcript
            }
        } else {
            transcript
        };

        if query.trim().is_empty() {
            self.state = match self.mode {
                VoiceMode::PushToTalk => SessionState::Listening,
                VoiceMode::AlwaysListen => SessionState::WaitingWakeWord,
            };
            return Ok(None);
        }

        // Dispatch question to remote Holon agent
        let answer = self.dispatcher.dispatch_run(&query)?;

        if !answer.trim().is_empty() {
            let playback = self.tts.speak(&answer)?;
            self.active_playback = Some(playback);
            self.vad.set_playback_active(true);
            self.state = SessionState::Speaking;
        } else {
            self.state = match self.mode {
                VoiceMode::PushToTalk => SessionState::Listening,
                VoiceMode::AlwaysListen => SessionState::WaitingWakeWord,
            };
        }

        Ok(Some(format!("You: {query}\nAgent: {answer}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::MockDispatcher;
    use crate::stt::MockStt;
    use crate::tts::MockTts;

    #[test]
    fn always_listen_session_processes_wake_word_and_dispatches() {
        let stt = Arc::new(MockStt::new(vec![
            "ignore background chatter".into(),
            "hey holon what is my next task".into(),
        ]));
        let dispatcher = Arc::new(MockDispatcher::new(vec!["Your task is to write tests.".into()]));
        let tts = Arc::new(MockTts::new());

        let vad_cfg = VadConfig::default();
        let mut session = VoiceSession::new(
            VoiceMode::AlwaysListen,
            "hey holon",
            "en-US",
            vad_cfg,
            stt,
            Arc::clone(&dispatcher) as Arc<dyn AgentDispatcher>,
            Arc::clone(&tts) as Arc<dyn SpeechSynthesizer>,
        );

        assert_eq!(session.state(), SessionState::WaitingWakeWord);

        // Utterance 1 without wake word is ignored
        let dummy_audio = vec![1000i16; 16000];
        let res1 = session.handle_utterance(dummy_audio.clone()).unwrap();
        assert_eq!(res1, None);
        assert_eq!(session.state(), SessionState::WaitingWakeWord);

        // Utterance 2 with wake word triggers dispatch and speech
        let res2 = session.handle_utterance(dummy_audio).unwrap();
        assert!(res2.unwrap().contains("Agent: Your task is to write tests."));
        assert_eq!(session.state(), SessionState::Speaking);
        assert_eq!(tts.spoken_history(), vec!["Your task is to write tests."]);
    }

    #[test]
    fn barge_in_interrupts_ongoing_speaking() {
        let stt = Arc::new(MockStt::new(vec!["hey holon tell me a story".into()]));
        let dispatcher = Arc::new(MockDispatcher::new(vec!["Once upon a time...".into()]));
        let tts = Arc::new(MockTts::new());

        let vad_cfg = VadConfig {
            barge_in_consecutive_frames: 2,
            energy_threshold: 0.05,
            ..Default::default()
        };

        let mut session = VoiceSession::new(
            VoiceMode::AlwaysListen,
            "hey holon",
            "en-US",
            vad_cfg,
            stt,
            dispatcher,
            Arc::clone(&tts) as Arc<dyn SpeechSynthesizer>,
        );

        let _ = session.handle_utterance(vec![1000i16; 3200]).unwrap();
        assert_eq!(session.state(), SessionState::Speaking);

        let loud_frame = vec![16384i16; 320];
        assert_eq!(session.feed_frame(&loud_frame).unwrap(), None);

        // Second loud frame triggers BargeIn event!
        let barge_res = session.feed_frame(&loud_frame).unwrap();
        assert_eq!(barge_res, Some("[Barge-in: interrupted assistant]".into()));
        assert_eq!(session.state(), SessionState::Listening);
        assert!(tts.was_last_interrupted());
    }
}
