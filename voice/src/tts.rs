use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

pub trait ActivePlayback: Send {
    fn is_active(&mut self) -> bool;
    fn wait(&mut self) -> Result<(), String>;
    fn stop(&mut self) -> Result<(), String>;
}

pub trait SpeechSynthesizer: Send + Sync {
    fn speak(&self, text: &str) -> Result<Box<dyn ActivePlayback>, String>;
}

pub struct ProcessPlayback {
    child: Option<Child>,
}

impl ProcessPlayback {
    pub fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }
}

impl ActivePlayback for ProcessPlayback {
    fn is_active(&mut self) -> bool {
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(None) => true,
                Ok(Some(_)) => {
                    self.child = None;
                    false
                }
                Err(_) => {
                    self.child = None;
                    false
                }
            }
        } else {
            false
        }
    }

    fn wait(&mut self) -> Result<(), String> {
        if let Some(mut child) = self.child.take() {
            child.wait().map_err(|e| format!("playback wait failed: {e}"))?;
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), String> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }
}

pub struct MacSayTts {
    voice: String,
}

impl MacSayTts {
    pub fn new(voice: impl Into<String>) -> Self {
        Self { voice: voice.into() }
    }
}

impl SpeechSynthesizer for MacSayTts {
    fn speak(&self, text: &str) -> Result<Box<dyn ActivePlayback>, String> {
        let child = Command::new("/usr/bin/say")
            .arg("-v")
            .arg(&self.voice)
            .arg(text)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to spawn say: {e}"))?;

        Ok(Box::new(ProcessPlayback::new(child)))
    }
}

pub struct CustomCommandTts {
    bin: std::path::PathBuf,
    voice: String,
}

impl CustomCommandTts {
    pub fn new(bin: impl Into<std::path::PathBuf>, voice: impl Into<String>) -> Self {
        Self { bin: bin.into(), voice: voice.into() }
    }
}

impl SpeechSynthesizer for CustomCommandTts {
    fn speak(&self, text: &str) -> Result<Box<dyn ActivePlayback>, String> {
        let child = Command::new(&self.bin)
            .arg(&self.voice)
            .arg(text)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to spawn tts {}: {e}", self.bin.display()))?;

        Ok(Box::new(ProcessPlayback::new(child)))
    }
}

pub struct MockPlayback {
    active: Arc<Mutex<bool>>,
    interrupted: Arc<Mutex<bool>>,
}

impl ActivePlayback for MockPlayback {
    fn is_active(&mut self) -> bool {
        *self.active.lock().unwrap()
    }

    fn wait(&mut self) -> Result<(), String> {
        *self.active.lock().unwrap() = false;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), String> {
        *self.active.lock().unwrap() = false;
        *self.interrupted.lock().unwrap() = true;
        Ok(())
    }
}

pub struct MockTts {
    spoken: Arc<Mutex<Vec<String>>>,
    last_interrupted: Arc<Mutex<bool>>,
}

impl MockTts {
    pub fn new() -> Self {
        Self {
            spoken: Arc::new(Mutex::new(Vec::new())),
            last_interrupted: Arc::new(Mutex::new(false)),
        }
    }

    pub fn spoken_history(&self) -> Vec<String> {
        self.spoken.lock().unwrap().clone()
    }

    pub fn was_last_interrupted(&self) -> bool {
        *self.last_interrupted.lock().unwrap()
    }
}

impl Default for MockTts {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeechSynthesizer for MockTts {
    fn speak(&self, text: &str) -> Result<Box<dyn ActivePlayback>, String> {
        self.spoken.lock().unwrap().push(text.to_string());
        *self.last_interrupted.lock().unwrap() = false;
        let active = Arc::new(Mutex::new(true));
        Ok(Box::new(MockPlayback { active, interrupted: Arc::clone(&self.last_interrupted) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_tts_tracks_speech_and_interruption() {
        let tts = MockTts::new();
        let mut playback = tts.speak("Hello from test").unwrap();
        assert!(playback.is_active());
        assert_eq!(tts.spoken_history(), vec!["Hello from test"]);

        playback.stop().unwrap();
        assert!(!playback.is_active());
        assert!(tts.was_last_interrupted());
    }
}
