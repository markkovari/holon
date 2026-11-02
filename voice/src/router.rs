/// Classifies spoken prompts to route to the optimal specialist agent.
#[derive(Debug, Clone)]
pub struct IntentRouter {
    default_agent: String,
}

impl Default for IntentRouter {
    fn default() -> Self {
        Self::new("bot")
    }
}

impl IntentRouter {
    pub fn new(default_agent: impl Into<String>) -> Self {
        Self { default_agent: default_agent.into() }
    }

    /// Evaluates prompt and returns `(target_agent, cleaned_prompt)`.
    pub fn route<'a>(&self, prompt: &'a str) -> (String, &'a str) {
        let trimmed = prompt.trim();
        let lower = trimmed.to_lowercase();

        // 1. Check for explicit prefix addressing
        if let Some((agent, rest)) = Self::extract_explicit_prefix(trimmed) {
            return (agent, rest);
        }

        // 2. Rowing & athletic coaching intent (Concept2 / Erg)
        if Self::matches_rowing_intent(&lower) {
            return ("coach".to_string(), trimmed);
        }

        // 3. Calendar & scheduling intent
        if Self::matches_calendar_intent(&lower) {
            return ("calendar".to_string(), trimmed);
        }

        // 4. Weather intent
        if Self::matches_weather_intent(&lower) {
            return ("weather".to_string(), trimmed);
        }

        // 5. Code & Pull requests intent
        if Self::matches_pr_intent(&lower) {
            return ("pr-bot".to_string(), trimmed);
        }

        // Default agent
        (self.default_agent.clone(), trimmed)
    }

    pub fn extract_explicit_prefix(original: &str) -> Option<(String, &str)> {
        let trimmed = original.trim();
        let lower = trimmed.to_lowercase();

        let greetings = ["hey", "hi", "hello", "okay", "ok", "ask", "tell"];
        let agents = [
            ("coach", "coach"),
            ("rower", "rower"),
            ("her over", "rower"),
            ("roller", "rower"),
            ("rowing", "coach"),
            ("calendar", "calendar"),
            ("weather", "weather"),
            ("bot", "bot"),
            ("dev", "pr-bot"),
            ("pr-bot", "pr-bot"),
        ];

        // 1. Check with greetings: e.g. "Hey, coach, how was my rowing?" or "Ask coach what my pace was"
        for greeting in greetings {
            if let Some(after_greeting) = lower.strip_prefix(greeting) {
                let rest_lower = after_greeting.trim_start_matches(|c: char| c == ',' || c == ':' || c.is_whitespace());
                for (name, target) in agents {
                    if let Some(after_agent) = rest_lower.strip_prefix(name) {
                        let is_delim = after_agent.is_empty()
                            || after_agent.starts_with(|c: char| c == ',' || c == ':' || c.is_whitespace() || c == '?' || c == '!' || c == '.');
                        if is_delim {
                            let consumed_len = (trimmed.len() - rest_lower.len()) + name.len();
                            let rest = trimmed[consumed_len..]
                                .trim_start_matches(|c: char| c == ':' || c == ',' || c == '?' || c == '!' || c == '.' || c.is_whitespace());
                            if !rest.is_empty() {
                                return Some((target.to_string(), rest));
                            }
                        }
                    }
                }
            }
        }

        // 2. Direct addressing: e.g. "Coach, how was my rowing?" or "Coach: ..."
        for (name, target) in agents {
            if let Some(after_agent) = lower.strip_prefix(name) {
                let is_delim = after_agent.starts_with(|c: char| c == ',' || c == ':' || c.is_whitespace());
                if is_delim {
                    let rest = trimmed[name.len()..]
                        .trim_start_matches(|c: char| c == ':' || c == ',' || c.is_whitespace());
                    if !rest.is_empty() {
                        return Some((target.to_string(), rest));
                    }
                }
            }
        }

        None
    }

    fn matches_rowing_intent(lower: &str) -> bool {
        let keywords = [
            "row",
            "rowing",
            "rower",
            "rowerg",
            "concept2",
            "erg",
            "erging",
            "split",
            "stroke",
            "stroke rate",
            "strokes",
            "spm",
            "drag factor",
            "500m",
            "10k",
            "2k",
            "5k",
            "intervals",
            "aerobic",
            "warmup",
            "recovery row",
            "training plan",
            "damper",
            "meters",
            "metres",
            "watts",
            "lactic acid",
            "metric",
            "metrics",
            "workout",
            "workouts",
        ];

        // Token or phrase matching with word-boundary awareness
        for kw in keywords {
            if contains_keyword(lower, kw) {
                return true;
            }
        }
        false
    }

    fn matches_calendar_intent(lower: &str) -> bool {
        let keywords = [
            "calendar",
            "schedule",
            "meeting",
            "appointment",
            "events",
            "agenda",
            "am i free",
            "what's on for today",
            "what's on today",
            "tomorrow's plan",
            "my schedule",
            "add event",
        ];

        for kw in keywords {
            if contains_keyword(lower, kw) {
                return true;
            }
        }
        false
    }

    fn matches_weather_intent(lower: &str) -> bool {
        let keywords = [
            "weather",
            "forecast",
            "rain",
            "raining",
            "temperature",
            "umbrella",
            "how cold",
            "how warm",
            "how hot",
            "sunny",
        ];

        for kw in keywords {
            if contains_keyword(lower, kw) {
                return true;
            }
        }
        false
    }

    fn matches_pr_intent(lower: &str) -> bool {
        let keywords =
            ["pull request", "dependabot", "open prs", "merge pr", "github pr", "review pr"];

        for kw in keywords {
            if contains_keyword(lower, kw) {
                return true;
            }
        }
        false
    }
}

/// Matches either a multi-word phrase with substring search or a single-word keyword
/// on exact alphanumeric word token boundaries (preventing false positives like 'tomorrow' matching 'row').
fn contains_keyword(text: &str, kw: &str) -> bool {
    if kw.contains(' ') {
        text.contains(kw)
    } else {
        text.split(|c: char| !c.is_alphanumeric()).any(|token| token == kw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_rowing_prompts_to_coach() {
        let router = IntentRouter::default();
        let (agent, _) = router.route("Analyze my rowing");
        assert_eq!(agent, "coach");

        let (agent, _) = router.route("What was my average split on the 10k erg?");
        assert_eq!(agent, "coach");

        let (agent, _) = router.route("How is my stroke rate consistency?");
        assert_eq!(agent, "coach");
    }

    #[test]
    fn routes_explicit_prefixes_and_cleans_prompt() {
        let router = IntentRouter::default();
        let (agent, cleaned) = router.route("ask coach: do I need more volume?");
        assert_eq!(agent, "coach");
        assert_eq!(cleaned, "do I need more volume?");

        let (agent, cleaned) = router.route("calendar: what do I have at 3pm?");
        assert_eq!(agent, "calendar");
        assert_eq!(cleaned, "what do I have at 3pm?");

        // Natural spoken greetings with commas (as transcribed by Apple Neural STT)
        let (agent, cleaned) = router.route("Hey, coach, how was my rowing?");
        assert_eq!(agent, "coach");
        assert_eq!(cleaned, "how was my rowing?");

        let (agent, cleaned) = router.route("Coach, analyze my rowing");
        assert_eq!(agent, "coach");
        assert_eq!(cleaned, "analyze my rowing");

        // Apple STT phonetic homophone for 'ask rower'
        let (agent, cleaned) = router.route("ask her over about my metrics");
        assert_eq!(agent, "rower");
        assert_eq!(cleaned, "about my metrics");
    }

    #[test]
    fn routes_calendar_and_weather_domains() {
        let router = IntentRouter::default();
        let (agent, _) = router.route("What's on my schedule for today?");
        assert_eq!(agent, "calendar");

        let (agent, _) = router.route("Check my calendar for tomorrow.");
        assert_eq!(agent, "calendar");

        let (agent, _) = router.route("Is it raining outside right now?");
        assert_eq!(agent, "weather");
    }

    #[test]
    fn falls_back_to_default_for_general_queries() {
        let router = IntentRouter::default();
        let (agent, _) = router.route("Tell me a philosophy joke.");
        assert_eq!(agent, "bot");
    }
}
