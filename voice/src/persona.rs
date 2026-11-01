use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A vocal persona representing an agent in the matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Persona {
    pub key: String,
    pub title: String,
    pub voice: String,
    pub rate: Option<u32>,
}

impl Persona {
    pub fn new(key: impl Into<String>, title: impl Into<String>, voice: impl Into<String>) -> Self {
        Self { key: key.into(), title: title.into(), voice: voice.into(), rate: None }
    }

    pub fn with_rate(mut self, rate: u32) -> Self {
        self.rate = Some(rate);
        self
    }
}

/// Registry of known agent personas and default fallback.
#[derive(Debug, Clone)]
pub struct PersonaRegistry {
    personas: HashMap<String, Persona>,
    default_persona: Persona,
}

impl Default for PersonaRegistry {
    fn default() -> Self {
        let mut reg =
            Self { personas: HashMap::new(), default_persona: Persona::new("bot", "", "Alex") };

        // Register default out-of-the-box roundtable personas
        reg.register(Persona::new("rowing", "Rowing Coach Daniel", "Daniel"));
        reg.register(Persona::new("coach", "Coach Daniel", "Daniel"));
        reg.register(Persona::new("nutrition", "Nutritionist Sarah", "Samantha"));
        reg.register(Persona::new("nutritionist", "Nutritionist Sarah", "Samantha"));
        reg.register(Persona::new("coder", "Lead Engineer Oliver", "Oliver"));
        reg.register(Persona::new("dev", "Lead Engineer Oliver", "Oliver"));
        reg.register(Persona::new("bot", "", "Alex"));

        reg
    }
}

impl PersonaRegistry {
    pub fn register(&mut self, persona: Persona) {
        self.personas.insert(persona.key.to_lowercase(), persona);
    }

    pub fn resolve(&self, agent: &str) -> &Persona {
        let normalized = agent.trim().to_lowercase();
        if let Some(p) = self.personas.get(&normalized) {
            return p;
        }

        // Match by exact title
        for persona in self.personas.values() {
            if !persona.title.is_empty() && persona.title.to_lowercase() == normalized {
                return persona;
            }
        }

        // Match by substring / keyword in title or key
        for persona in self.personas.values() {
            let key = persona.key.to_lowercase();
            let title = persona.title.to_lowercase();
            if (!key.is_empty() && key != "bot" && normalized.contains(&key))
                || (!title.is_empty()
                    && (normalized.contains(&title) || title.contains(&normalized)))
            {
                return persona;
            }
        }

        &self.default_persona
    }
}

/// Tracks the active speaker at the roundtable to handle continuous speaking.
/// When the speaker switches (or after a silence gap), the rich persona title
/// is prepended. If the same agent speaks continuously, the title is omitted.
pub struct SpeakerTracker {
    last_speaker: Option<String>,
    last_spoke_at: Option<Instant>,
    silence_reset_duration: Duration,
}

impl Default for SpeakerTracker {
    fn default() -> Self {
        Self::new(Duration::from_secs(10))
    }
}

impl SpeakerTracker {
    pub fn new(silence_reset_duration: Duration) -> Self {
        Self { last_speaker: None, last_spoke_at: None, silence_reset_duration }
    }

    /// Formats spoken text based on whether the agent is speaking continuously.
    /// Returns `(spoken_text, is_new_speaker)`.
    pub fn format_speech(&mut self, persona: &Persona, raw_text: &str) -> (String, bool) {
        let now = Instant::now();
        let timed_out = match self.last_spoke_at {
            Some(t) => now.duration_since(t) > self.silence_reset_duration,
            None => true,
        };

        let is_new_speaker = timed_out
            || match &self.last_speaker {
                Some(prev) => prev != &persona.key,
                None => true,
            };

        self.last_speaker = Some(persona.key.clone());
        self.last_spoke_at = Some(now);

        let trimmed = raw_text.trim();
        let formatted = if is_new_speaker && !persona.title.trim().is_empty() {
            // Only prepend title if the text doesn't already begin with it
            if trimmed.to_lowercase().starts_with(&persona.title.to_lowercase()) {
                trimmed.to_string()
            } else {
                format!("{}: {}", persona.title, trimmed)
            }
        } else {
            trimmed.to_string()
        };

        (formatted, is_new_speaker)
    }

    /// Explicitly resets the speaker state (e.g. after user input or session pause).
    pub fn reset(&mut self) {
        self.last_speaker = None;
        self.last_spoke_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_tracker_prepends_title_on_first_speak_and_switches() {
        let mut tracker = SpeakerTracker::new(Duration::from_secs(5));
        let daniel = Persona::new("rowing", "Rowing Coach Daniel", "Daniel");
        let sarah = Persona::new("nutrition", "Nutritionist Sarah", "Samantha");

        // First speaker gets title
        let (s1, is_new1) = tracker.format_speech(&daniel, "Let's do 10k.");
        assert!(is_new1);
        assert_eq!(s1, "Rowing Coach Daniel: Let's do 10k.");

        // Continuous speaking from same agent omits title
        let (s2, is_new2) = tracker.format_speech(&daniel, "Keep your split at 1:55.");
        assert!(!is_new2);
        assert_eq!(s2, "Keep your split at 1:55.");

        // Switch to Sarah gets title
        let (s3, is_new3) = tracker.format_speech(&sarah, "Hydrate before you start.");
        assert!(is_new3);
        assert_eq!(s3, "Nutritionist Sarah: Hydrate before you start.");

        // Continuous speaking from Sarah omits title
        let (s4, is_new4) = tracker.format_speech(&sarah, "At least 500ml of electrolytes.");
        assert!(!is_new4);
        assert_eq!(s4, "At least 500ml of electrolytes.");
    }

    #[test]
    fn speaker_tracker_reintroduces_after_silence_timeout() {
        let mut tracker = SpeakerTracker::new(Duration::from_millis(50));
        let daniel = Persona::new("rowing", "Rowing Coach Daniel", "Daniel");

        let (s1, is_new1) = tracker.format_speech(&daniel, "First prompt.");
        assert!(is_new1);
        assert_eq!(s1, "Rowing Coach Daniel: First prompt.");

        std::thread::sleep(Duration::from_millis(60));

        let (s2, is_new2) = tracker.format_speech(&daniel, "Second prompt after silence.");
        assert!(is_new2);
        assert_eq!(s2, "Rowing Coach Daniel: Second prompt after silence.");
    }

    #[test]
    fn parse_dialogue_turns_splits_multi_speaker_roundtable() {
        let text = r#"
Rowing Coach Daniel: He did 10k threshold yesterday.
We should keep today light.

Nutritionist Sarah: Agreed, his glycogen is depleted.
Have 40 grams of carbs before rowing.
"#;
        let turns = parse_dialogue_turns(text, "bot");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].speaker, "Rowing Coach Daniel");
        assert_eq!(turns[0].text, "He did 10k threshold yesterday. We should keep today light.");
        assert_eq!(turns[1].speaker, "Nutritionist Sarah");
        assert_eq!(
            turns[1].text,
            "Agreed, his glycogen is depleted. Have 40 grams of carbs before rowing."
        );
    }

    #[test]
    fn parse_dialogue_turns_handles_single_unprefixed_response() {
        let text = "Your workout today is an easy 30-minute row.";
        let turns = parse_dialogue_turns(text, "bot");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].speaker, "bot");
        assert_eq!(turns[0].text, "Your workout today is an easy 30-minute row.");
    }
}

/// A single spoken dialogue turn in a multi-agent roundtable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogueTurn {
    pub speaker: String,
    pub text: String,
}

/// Parses multi-agent dialogue lines or falls back to a single turn for the default speaker.
pub fn parse_dialogue_turns(raw_text: &str, default_speaker: &str) -> Vec<DialogueTurn> {
    let mut turns: Vec<DialogueTurn> = Vec::new();

    for line in raw_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some((speaker, content)) = extract_speaker_prefix(trimmed) {
            turns.push(DialogueTurn { speaker: speaker.to_string(), text: content.to_string() });
        } else if let Some(last) = turns.last_mut() {
            if !last.text.ends_with(' ') {
                last.text.push(' ');
            }
            last.text.push_str(trimmed);
        } else {
            turns.push(DialogueTurn {
                speaker: default_speaker.to_string(),
                text: trimmed.to_string(),
            });
        }
    }

    if turns.is_empty() {
        vec![DialogueTurn {
            speaker: default_speaker.to_string(),
            text: raw_text.trim().to_string(),
        }]
    } else {
        turns
    }
}

fn extract_speaker_prefix(line: &str) -> Option<(&str, &str)> {
    let (prefix, rest) = line.split_once(':')?;
    let cleaned = prefix
        .trim()
        .trim_matches(|c: char| c == '[' || c == ']' || c == '*' || c == '_' || c.is_whitespace());

    if cleaned.is_empty() || cleaned.len() > 35 {
        return None;
    }

    // A valid speaker label is 1 to 4 words with no punctuation
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    if words.is_empty() || words.len() > 4 {
        return None;
    }

    if cleaned.chars().any(|c| c == '.' || c == '?' || c == '!' || c == '/' || c == '\\') {
        return None;
    }

    let content = rest.trim();
    if content.is_empty() {
        return None;
    }

    Some((cleaned, content))
}
