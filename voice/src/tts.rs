use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::persona::Persona;

pub trait ActivePlayback: Send {
    fn is_active(&mut self) -> bool;
    fn wait(&mut self) -> Result<(), String>;
    fn stop(&mut self) -> Result<(), String>;
}

pub trait SpeechSynthesizer: Send + Sync {
    fn speak(&self, text: &str) -> Result<Box<dyn ActivePlayback>, String>;

    fn speak_with_persona(
        &self,
        _persona: &Persona,
        text: &str,
    ) -> Result<Box<dyn ActivePlayback>, String> {
        self.speak(text)
    }
}

pub struct ProcessPlayback {
    child: Option<Child>,
}

impl ProcessPlayback {
    pub fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }
}

pub type PlaybackAction = Box<dyn FnMut() -> Result<Box<dyn ActivePlayback>, String> + Send>;

pub struct ChainedPlayback {
    current: Option<Box<dyn ActivePlayback>>,
    queue: std::collections::VecDeque<PlaybackAction>,
}

impl ChainedPlayback {
    pub fn new(mut queue: std::collections::VecDeque<PlaybackAction>) -> Result<Self, String> {
        let current = if let Some(mut first) = queue.pop_front() { Some(first()?) } else { None };
        Ok(Self { current, queue })
    }
}

impl ActivePlayback for ChainedPlayback {
    fn is_active(&mut self) -> bool {
        loop {
            if let Some(current) = self.current.as_mut() {
                if current.is_active() {
                    return true;
                }
            }
            if let Some(mut next_fn) = self.queue.pop_front() {
                match next_fn() {
                    Ok(next_playback) => {
                        self.current = Some(next_playback);
                    }
                    Err(_) => {
                        self.current = None;
                        return false;
                    }
                }
            } else {
                self.current = None;
                return false;
            }
        }
    }

    fn wait(&mut self) -> Result<(), String> {
        while self.is_active() {
            if let Some(current) = self.current.as_mut() {
                current.wait()?;
            }
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), String> {
        self.queue.clear();
        if let Some(mut current) = self.current.take() {
            current.stop()?;
        }
        Ok(())
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

    fn spawn_say(
        &self,
        voice: &str,
        rate: Option<u32>,
        text: &str,
    ) -> Result<Box<dyn ActivePlayback>, String> {
        let mut cmd = Command::new("/usr/bin/say");
        cmd.arg("-v").arg(voice);
        if let Some(r) = rate {
            cmd.arg("-r").arg(r.to_string());
        }
        cmd.arg(text);
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());

        let child = cmd.spawn().map_err(|e| format!("failed to spawn say: {e}"))?;
        Ok(Box::new(ProcessPlayback::new(child)))
    }
}

impl SpeechSynthesizer for MacSayTts {
    fn speak(&self, text: &str) -> Result<Box<dyn ActivePlayback>, String> {
        self.spawn_say(&self.voice, None, text)
    }

    fn speak_with_persona(
        &self,
        persona: &Persona,
        text: &str,
    ) -> Result<Box<dyn ActivePlayback>, String> {
        let voice = if !persona.voice.is_empty() { &persona.voice } else { &self.voice };
        self.spawn_say(voice, persona.rate, text)
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

/// Sanitizes formatted markdown agent responses into natural conversational speech.
/// Strips markdown fences, code blocks, headers, styling symbols, URLs, and emojis.
pub fn sanitize_for_speech(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    let mut in_code_block = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Detect code block fences
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }

        // Skip code block content for spoken audio
        if in_code_block {
            continue;
        }

        let mut line_content = trimmed;

        // Strip markdown headers (# Header -> Header)
        if line_content.starts_with('#') {
            line_content = line_content.trim_start_matches('#').trim_start();
        }

        // Strip bullet points (- Item -> Item, * Item -> Item, + Item -> Item)
        if line_content.starts_with("- ")
            || line_content.starts_with("* ")
            || line_content.starts_with("+ ")
        {
            line_content = line_content[2..].trim_start();
        } else if let Some(dot_idx) = line_content.find(". ") {
            // Check for numbered lists: "1. Item"
            if line_content[..dot_idx].chars().all(|c| c.is_ascii_digit()) {
                line_content = line_content[dot_idx + 2..].trim_start();
            }
        }

        // Strip blockquotes (> Quote -> Quote)
        if line_content.starts_with("> ") {
            line_content = line_content[2..].trim_start();
        }

        if line_content.is_empty() {
            continue;
        }

        if !cleaned.is_empty() && !cleaned.ends_with(' ') {
            cleaned.push(' ');
        }
        cleaned.push_str(line_content);

        // Ensure lines have cadence punctuation
        if !cleaned.ends_with('.')
            && !cleaned.ends_with('!')
            && !cleaned.ends_with('?')
            && !cleaned.ends_with(':')
        {
            cleaned.push('.');
        }
    }

    // Strip inline formatting symbols
    let without_markdown = cleaned
        .replace("**", "")
        .replace("__", "")
        .replace('`', "")
        .replace("~~", "")
        .replace("https://", "")
        .replace("http://", "");

    // Strip emojis and non-speech symbols
    let sanitized: String = without_markdown.chars().filter(|&c| !is_emoji_or_symbol(c)).collect();

    // Normalize multiple whitespace characters
    let mut result = String::new();
    let mut last_space = false;
    for c in sanitized.chars() {
        if c.is_whitespace() {
            if !last_space {
                result.push(' ');
                last_space = true;
            }
        } else {
            result.push(c);
            last_space = false;
        }
    }

    result.trim().to_string()
}

fn is_emoji_or_symbol(c: char) -> bool {
    matches!(c,
        '\u{1F300}'..='\u{1F9FF}' // Symbols, pictographs, emoticons
        | '\u{1FA00}'..='\u{1FAFF}' // Extended-A symbols
        | '\u{2600}'..='\u{26FF}'   // Misc symbols (rocket, warning, etc.)
        | '\u{2700}'..='\u{27BF}'   // Dingbats
        | '\u{FE00}'..='\u{FE0F}'   // Variation selectors
        | '\u{1F000}'..='\u{1F02F}'
        | '\u{1F0A0}'..='\u{1F0FF}'
    )
}

pub struct MockPlayback {
    active: Arc<Mutex<bool>>,
    interrupted: Arc<Mutex<bool>>,
    auto_finish_count: Arc<Mutex<usize>>,
}

impl ActivePlayback for MockPlayback {
    fn is_active(&mut self) -> bool {
        let mut active = self.active.lock().unwrap();
        if !*active {
            return false;
        }
        let mut remaining = self.auto_finish_count.lock().unwrap();
        if *remaining > 0 {
            *remaining -= 1;
            if *remaining == 0 {
                *active = false;
                return false;
            }
        }
        *active
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
    personas: Arc<Mutex<Vec<String>>>,
    last_interrupted: Arc<Mutex<bool>>,
    auto_finish_polls: usize,
}

impl MockTts {
    pub fn new() -> Self {
        Self::with_auto_finish(0)
    }

    pub fn with_auto_finish(auto_finish_polls: usize) -> Self {
        Self {
            spoken: Arc::new(Mutex::new(Vec::new())),
            personas: Arc::new(Mutex::new(Vec::new())),
            last_interrupted: Arc::new(Mutex::new(false)),
            auto_finish_polls,
        }
    }

    pub fn spoken_history(&self) -> Vec<String> {
        self.spoken.lock().unwrap().clone()
    }

    pub fn persona_history(&self) -> Vec<String> {
        self.personas.lock().unwrap().clone()
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
        let auto_finish_count = Arc::new(Mutex::new(self.auto_finish_polls));
        Ok(Box::new(MockPlayback {
            active,
            interrupted: Arc::clone(&self.last_interrupted),
            auto_finish_count,
        }))
    }

    fn speak_with_persona(
        &self,
        persona: &Persona,
        text: &str,
    ) -> Result<Box<dyn ActivePlayback>, String> {
        self.personas.lock().unwrap().push(persona.key.clone());
        self.speak(text)
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

    #[test]
    fn sanitize_for_speech_cleans_markdown_and_emojis() {
        let input = "# System Overview\n\nStatus: **online** & `healthy` 🚀!\n\n```rust\nfn main() {}\n```\n\n- Node 1 active\n- See https://holon.local";
        let cleaned = sanitize_for_speech(input);
        assert_eq!(
            cleaned,
            "System Overview. Status: online & healthy ! Node 1 active. See holon.local."
        );
        assert!(!cleaned.contains("```"));
        assert!(!cleaned.contains("**"));
        assert!(!cleaned.contains('#'));
        assert!(!cleaned.contains("https://"));
    }
}
