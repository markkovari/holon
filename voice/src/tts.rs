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
