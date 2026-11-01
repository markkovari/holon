use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

pub trait SpeechRecognizer: Send + Sync {
    fn transcribe(&self, wav_path: &Path, locale: &str) -> Result<String, String>;
}

pub struct CommandStt {
    bin: PathBuf,
}

impl CommandStt {
    pub fn new(bin: impl Into<PathBuf>) -> Self {
        Self { bin: bin.into() }
    }
}

impl SpeechRecognizer for CommandStt {
    fn transcribe(&self, wav_path: &Path, locale: &str) -> Result<String, String> {
        if !self.bin.exists() {
            return Err(format!("stt binary not found at {}", self.bin.display()));
        }
        let output = Command::new(&self.bin)
            .arg(wav_path)
            .arg(locale)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("failed to execute {}: {e}", self.bin.display()))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(format!("stt failed ({}): {err}", output.status));
        }

        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(text)
    }
}

pub struct MockStt {
    transcripts: Mutex<Vec<String>>,
}

impl MockStt {
    pub fn new(transcripts: Vec<String>) -> Self {
        Self { transcripts: Mutex::new(transcripts) }
    }
}

impl SpeechRecognizer for MockStt {
    fn transcribe(&self, _wav_path: &Path, _locale: &str) -> Result<String, String> {
        let mut list = self.transcripts.lock().unwrap();
        if list.is_empty() {
            Ok(String::new())
        } else {
            Ok(list.remove(0))
        }
    }
}

/// Normalizes text by lowercasing and trimming non-alphanumeric punctuation.
fn clean_token(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

/// Detects if the transcript begins with or contains the wake phrase or known phonetic variants.
/// Returns Some(rest_of_query) if wake word is found, or None.
pub fn extract_wake_phrase(transcript: &str, wake_phrase: &str) -> Option<String> {
    let clean_transcript = clean_token(transcript);
    let clean_wake = clean_token(wake_phrase);

    let parts: Vec<&str> = clean_transcript.split_whitespace().collect();
    let wake_parts: Vec<&str> = clean_wake.split_whitespace().collect();

    if wake_parts.is_empty() {
        return Some(transcript.trim().to_string());
    }

    // Direct exact match
    if let Some(pos) =
        parts.windows(wake_parts.len()).position(|window| window == wake_parts.as_slice())
    {
        let rest_words: Vec<&str> = parts[pos + wake_parts.len()..].to_vec();
        return Some(rest_words.join(" "));
    }

    // Phonetic tolerance for "holon": Apple Speech on macOS often recognizes "holon"
    // as "hold on", "holland", "holum", "holden", "colon", "whole on", "holan", etc.
    // If the wake phrase includes a prefix like "hey", phonetic variants must also require the prefix
    // to prevent false positives on ordinary words like "hold on" or "holland".
    let variants: &[&[&str]] = if clean_wake == "hey holon" {
        &[
            &["hey", "hold", "on"],
            &["hey", "holland"],
            &["hey", "holum"],
            &["hey", "holden"],
            &["hey", "colon"],
            &["hey", "whole", "on"],
            &["hey", "holan"],
            &["hey", "hall", "on"],
            &["hey", "haulin"],
            &["hey", "howlin"],
        ]
    } else if clean_wake == "holon" {
        &[
            &["holon"],
            &["holland"],
            &["holum"],
            &["holden"],
            &["colon"],
            &["whole", "on"],
            &["holan"],
            &["hall", "on"],
            &["hold", "on"],
        ]
    } else {
        &[]
    };

    for variant in variants {
        if let Some(pos) = parts.windows(variant.len()).position(|window| window == *variant) {
            let rest_words: Vec<&str> = parts[pos + variant.len()..].to_vec();
            return Some(rest_words.join(" "));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_wake_phrase_properly() {
        assert_eq!(
            extract_wake_phrase("Hey Holon, what is the temperature in the lab?", "hey holon"),
            Some("what is the temperature in the lab".to_string())
        );

        // Exact wake word with punctuation
        assert_eq!(extract_wake_phrase("Hey, Holon!", "hey holon"), Some("".to_string()));

        // Embedded wake word
        assert_eq!(
            extract_wake_phrase("umm hey holon deploy the cluster now", "hey holon"),
            Some("deploy the cluster now".to_string())
        );

        // Apple Speech phonetic homophone "Hey, hold on"
        assert_eq!(
            extract_wake_phrase("Hey, hold on, what is the weather today?", "hey holon"),
            Some("what is the weather today".to_string())
        );

        // Wake word missing
        assert_eq!(extract_wake_phrase("good morning everyone", "hey holon"), None);
    }

    #[test]
    fn mock_stt_returns_queued_transcripts() {
        let mock = MockStt::new(vec!["first audio".into(), "second audio".into()]);
        let dummy = Path::new("/dummy.wav");
        assert_eq!(mock.transcribe(dummy, "en-US").unwrap(), "first audio");
        assert_eq!(mock.transcribe(dummy, "en-US").unwrap(), "second audio");
        assert_eq!(mock.transcribe(dummy, "en-US").unwrap(), "");
    }
}
