use std::time::Instant;

use crate::audio::rms_energy;

#[derive(Debug, Clone)]
pub struct VadConfig {
    pub sample_rate: u32,
    pub frame_size: usize,
    pub energy_threshold: f32,
    pub barge_in_energy_threshold: f32,
    pub barge_in_guard_ms: u64,
    pub silence_timeout_ms: u64,
    pub min_speech_duration_ms: u64,
    pub barge_in_consecutive_frames: usize,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            frame_size: 320, // 20ms at 16kHz
            energy_threshold: 0.018,
            barge_in_energy_threshold: 0.080,
            barge_in_guard_ms: 800,
            silence_timeout_ms: 1000,
            min_speech_duration_ms: 200,
            barge_in_consecutive_frames: 8, // 160ms
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum VadEvent {
    Silence,
    SpeechStart,
    Speaking,
    UtteranceComplete(Vec<i16>),
    BargeIn,
}

pub struct VoiceActivityDetector {
    config: VadConfig,
    in_speech: bool,
    accumulated_samples: Vec<i16>,
    speech_frames_count: usize,
    trailing_silence_frames: usize,
    barge_in_streak: usize,
    playback_active: bool,
    playback_start: Option<Instant>,
}

impl VoiceActivityDetector {
    pub fn new(config: VadConfig) -> Self {
        Self {
            config,
            in_speech: false,
            accumulated_samples: Vec::new(),
            speech_frames_count: 0,
            trailing_silence_frames: 0,
            barge_in_streak: 0,
            playback_active: false,
            playback_start: None,
        }
    }

    pub fn set_playback_active(&mut self, active: bool) {
        self.playback_active = active;
        self.barge_in_streak = 0;
        self.playback_start = if active { Some(Instant::now()) } else { None };
    }

    pub fn is_playback_active(&self) -> bool {
        self.playback_active
    }

    pub fn reset(&mut self) {
        self.in_speech = false;
        self.accumulated_samples.clear();
        self.speech_frames_count = 0;
        self.trailing_silence_frames = 0;
        self.barge_in_streak = 0;
        self.playback_start = None;
    }

    pub fn process_frame(&mut self, frame: &[i16]) -> VadEvent {
        let energy = rms_energy(frame);
        let is_voice = energy >= self.config.energy_threshold;
        let ms_per_frame = (frame.len() as f64 / self.config.sample_rate as f64) * 1000.0;
        let silence_frames_threshold =
            ((self.config.silence_timeout_ms as f64) / ms_per_frame).ceil() as usize;
        let min_speech_frames =
            ((self.config.min_speech_duration_ms as f64) / ms_per_frame).ceil() as usize;

        if self.playback_active {
            // Guard initial playback against speaker acoustic feedback
            let past_guard = self
                .playback_start
                .map(|t| {
                    t.elapsed() >= std::time::Duration::from_millis(self.config.barge_in_guard_ms)
                })
                .unwrap_or(true);

            if past_guard && energy >= self.config.barge_in_energy_threshold {
                self.barge_in_streak += 1;
                if self.barge_in_streak >= self.config.barge_in_consecutive_frames {
                    self.playback_active = false;
                    self.barge_in_streak = 0;
                    self.playback_start = None;
                    self.in_speech = true;
                    self.speech_frames_count = self.config.barge_in_consecutive_frames;
                    self.trailing_silence_frames = 0;
                    self.accumulated_samples.clear();
                    self.accumulated_samples.extend_from_slice(frame);
                    return VadEvent::BargeIn;
                }
            } else {
                self.barge_in_streak = 0;
            }
            return VadEvent::Silence;
        }

        if is_voice {
            self.trailing_silence_frames = 0;
            self.accumulated_samples.extend_from_slice(frame);
            if !self.in_speech {
                self.in_speech = true;
                self.speech_frames_count = 1;
                VadEvent::SpeechStart
            } else {
                self.speech_frames_count += 1;
                VadEvent::Speaking
            }
        } else if self.in_speech {
            self.trailing_silence_frames += 1;
            self.accumulated_samples.extend_from_slice(frame);

            if self.trailing_silence_frames >= silence_frames_threshold {
                let complete = self.speech_frames_count >= min_speech_frames;
                let samples = std::mem::take(&mut self.accumulated_samples);
                self.in_speech = false;
                self.speech_frames_count = 0;
                self.trailing_silence_frames = 0;

                if complete {
                    VadEvent::UtteranceComplete(samples)
                } else {
                    VadEvent::Silence
                }
            } else {
                VadEvent::Speaking
            }
        } else {
            VadEvent::Silence
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_speech_and_silence_transition() {
        let config = VadConfig {
            sample_rate: 16000,
            frame_size: 320,
            energy_threshold: 0.05,
            silence_timeout_ms: 100,    // 5 frames of 20ms
            min_speech_duration_ms: 60, // 3 frames of 20ms
            barge_in_consecutive_frames: 3,
            ..Default::default()
        };
        let mut vad = VoiceActivityDetector::new(config);

        let silence_frame = vec![0i16; 320];
        let loud_frame = vec![16384i16; 320]; // RMS 0.5 > 0.05

        // Initial silence
        assert_eq!(vad.process_frame(&silence_frame), VadEvent::Silence);

        // Start speaking
        assert_eq!(vad.process_frame(&loud_frame), VadEvent::SpeechStart);
        assert_eq!(vad.process_frame(&loud_frame), VadEvent::Speaking);
        assert_eq!(vad.process_frame(&loud_frame), VadEvent::Speaking);

        // Trailing silence
        for _ in 0..4 {
            assert_eq!(vad.process_frame(&silence_frame), VadEvent::Speaking);
        }

        // 5th silence frame completes utterance
        match vad.process_frame(&silence_frame) {
            VadEvent::UtteranceComplete(samples) => {
                assert_eq!(samples.len(), (3 + 5) * 320);
            }
            other => panic!("expected UtteranceComplete, got {:?}", other),
        }
    }

    #[test]
    fn detects_barge_in_during_playback() {
        let config = VadConfig {
            sample_rate: 16000,
            frame_size: 320,
            energy_threshold: 0.05,
            barge_in_energy_threshold: 0.08,
            barge_in_guard_ms: 0,
            silence_timeout_ms: 100,
            min_speech_duration_ms: 60,
            barge_in_consecutive_frames: 3,
        };
        let mut vad = VoiceActivityDetector::new(config);
        vad.set_playback_active(true);

        let loud_frame = vec![16384i16; 320];

        // 1st loud frame during playback
        assert_eq!(vad.process_frame(&loud_frame), VadEvent::Silence);
        // 2nd loud frame during playback
        assert_eq!(vad.process_frame(&loud_frame), VadEvent::Silence);
        // 3rd loud frame triggers BargeIn!
        assert_eq!(vad.process_frame(&loud_frame), VadEvent::BargeIn);
        assert!(!vad.is_playback_active());
    }
}
