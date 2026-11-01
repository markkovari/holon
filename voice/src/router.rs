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
        if let Some((agent, rest)) = Self::extract_explicit_prefix(trimmed, &lower) {
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

    fn extract_explicit_prefix<'a>(original: &'a str, lower: &str) -> Option<(String, &'a str)> {
        let prefixes = [
            ("ask coach", "coach"),
            ("tell coach", "coach"),
            ("coach:", "coach"),
            ("ask rower", "rower"),
            ("rower:", "rower"),
            ("ask rowing", "coach"),
            ("rowing:", "coach"),
            ("ask calendar", "calendar"),
            ("calendar:", "calendar"),
            ("ask weather", "weather"),
            ("weather:", "weather"),
            ("ask bot", "bot"),
            ("bot:", "bot"),
        ];

        for (prefix, agent) in prefixes {
            if lower.starts_with(prefix) {
                let rest = original[prefix.len()..]
                    .trim_start_matches(|c: char| c == ':' || c == ',' || c.is_whitespace());
                if !rest.is_empty() {
                    return Some((agent.to_string(), rest));
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
        ];

        // Token or phrase matching
        for kw in keywords {
            if lower.contains(kw) {
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
            if lower.contains(kw) {
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
            if lower.contains(kw) {
                return true;
            }
        }
        false
    }

    fn matches_pr_intent(lower: &str) -> bool {
        let keywords =
            ["pull request", "dependabot", "open prs", "merge pr", "github pr", "review pr"];

        for kw in keywords {
            if lower.contains(kw) {
                return true;
            }
        }
        false
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
    }

    #[test]
    fn routes_calendar_and_weather_domains() {
        let router = IntentRouter::default();
        let (agent, _) = router.route("What's on my schedule for today?");
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
