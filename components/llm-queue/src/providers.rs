//! The free providers behind the queue, and when each may be asked again: from its documented
//! limits before a request, and from what it said when one failed.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Gemini,
    Openai,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Provider {
    /// Also its key's name: `<name>-api-key` in SECRETS, `<NAME>_API_KEY`, `~/.comp-secrets/<name>.key`.
    pub name: String,
    pub kind: Kind,
    pub base: String,
    /// Wanted models, best first; `*` matches any run of characters. Among the offered models
    /// one pattern matches, the highest-sorting (newest version) is taken.
    pub models: Vec<String>,
    /// Its documented free limits; 0 is "not documented".
    #[serde(default)]
    pub rpm: u32,
    #[serde(default)]
    pub rpd: u32,
    /// The UTC hour its daily quota resets.
    #[serde(default)]
    pub reset_utc_hour: u64,
}

pub fn defaults() -> Vec<Provider> {
    serde_json::from_str(include_str!("../providers.json")).expect("providers.json is valid")
}

pub fn glob(pattern: &str, id: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if parts.len() == 1 {
        return pattern == id;
    }
    if id.len() < first.len() + last.len() || !id.starts_with(first) || !id.ends_with(last) {
        return false;
    }
    let mut rest = &id[first.len()..id.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

/// The offered models worth asking, best first: by wanted pattern, newest version first within one.
pub fn candidates(wanted: &[String], offered: &[String]) -> Vec<String> {
    let mut newest_first = offered.to_vec();
    newest_first.sort_by(|a, b| b.cmp(a));
    let mut out: Vec<String> = Vec::new();
    for w in wanted {
        for o in newest_first.iter().filter(|o| glob(w, o)) {
            if !out.contains(o) {
                out.push(o.clone());
            }
        }
    }
    out
}

/// `7.66s`, `1m23.5s`, `250ms`, `2h`.
fn duration(s: &str) -> Option<Duration> {
    let (mut total, mut num, mut any) = (0f64, String::new(), false);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            continue;
        }
        let n: f64 = num.parse().ok()?;
        num.clear();
        total += n * match c {
            'h' => 3600.0,
            'm' if chars.peek() == Some(&'s') => {
                chars.next();
                0.001
            }
            'm' => 60.0,
            's' => 1.0,
            _ => return None,
        };
        any = true;
    }
    (any && num.is_empty()).then(|| Duration::from_secs_f64(total))
}

/// A wait the provider named: Gemini's `"retryDelay": "37s"`, Groq's "try again in 7.66s".
pub fn named_delay(body: &str) -> Option<Duration> {
    ["\"retryDelay\": \"", "\"retryDelay\":\"", "try again in ", "retry in "].iter().find_map(|m| {
        let i = body.to_lowercase().find(&m.to_lowercase())?;
        let token: String = body[i + m.len()..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '.')
            .collect();
        duration(token.trim_end_matches('.'))
    })
}

pub fn next_reset(now: SystemTime, hour: u64) -> SystemTime {
    let secs = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let today = secs / 86400 * 86400 + hour * 3600;
    UNIX_EPOCH + Duration::from_secs(if today > secs { today } else { today + 86400 })
}

/// When a provider that failed this way may be asked again, and why. A refusal of this one
/// request (a 400) costs it nothing: the next provider is tried, and this one stays available.
pub fn back_at(
    p: &Provider,
    status: Option<u16>,
    retry_after: Option<&str>,
    body: &str,
    now: SystemTime,
) -> (SystemTime, String) {
    let lower = body.to_lowercase();
    let daily =
        ["per day", "perday", "rpd", "daily", "tokens per day"].iter().any(|k| lower.contains(k));
    let (wait, why) = match status {
        Some(429) if daily => {
            return (next_reset(now, p.reset_utc_hour), "daily quota spent".into())
        }
        Some(429) => (
            retry_after
                .and_then(|s| s.trim().parse::<f64>().ok())
                .map(Duration::from_secs_f64)
                .or_else(|| named_delay(body))
                .unwrap_or(Duration::from_secs(60)),
            "rate limited".to_string(),
        ),
        Some(401 | 403) => (Duration::from_secs(86400), "key refused".into()),
        Some(404) => (Duration::from_secs(600), "model gone; choosing again".into()),
        Some(s) if s >= 500 => {
            (named_delay(body).unwrap_or(Duration::from_secs(30)), format!("unavailable ({s})"))
        }
        Some(s) => (Duration::ZERO, format!("refused this request ({s})")),
        None => (Duration::from_secs(60), "unreachable".into()),
    };
    (now + wait, why)
}

/// What the queue knows about one provider's model (free limits are per model), or about the
/// provider itself when it cannot be asked at all.
#[derive(Default)]
pub struct Health {
    pub until: Option<SystemTime>,
    pub why: String,
    minute: VecDeque<SystemTime>,
    day: (u64, u32),
}

impl Health {
    /// `None` if it may be asked now, else when and why not.
    pub fn blocked(&mut self, p: &Provider, now: SystemTime) -> Option<(SystemTime, String)> {
        if let Some(u) = self.until.filter(|u| *u > now) {
            return Some((u, self.why.clone()));
        }
        let minute = Duration::from_secs(60);
        while self
            .minute
            .front()
            .is_some_and(|t| now.duration_since(*t).unwrap_or_default() >= minute)
        {
            self.minute.pop_front();
        }
        if p.rpm > 0 && self.minute.len() >= p.rpm as usize {
            return Some((self.minute[0] + minute, format!("at its {} requests a minute", p.rpm)));
        }
        let day = day_of(now, p.reset_utc_hour);
        if self.day.0 != day {
            self.day = (day, 0);
        }
        if p.rpd > 0 && self.day.1 >= p.rpd {
            return Some((
                next_reset(now, p.reset_utc_hour),
                format!("at its {} requests a day", p.rpd),
            ));
        }
        None
    }

    pub fn spent(&mut self, p: &Provider, now: SystemTime) {
        self.minute.push_back(now);
        let day = day_of(now, p.reset_utc_hour);
        self.day = if self.day.0 == day { (day, self.day.1 + 1) } else { (day, 1) };
    }
}

/// `Health` as kept between requests: a component instance lives for one.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct Stored {
    until_ms: Option<u64>,
    why: String,
    minute_ms: Vec<u64>,
    day: (u64, u32),
}

fn ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn time(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

impl From<&Health> for Stored {
    fn from(h: &Health) -> Self {
        Stored {
            until_ms: h.until.map(ms),
            why: h.why.clone(),
            minute_ms: h.minute.iter().copied().map(ms).collect(),
            day: h.day,
        }
    }
}

impl From<Stored> for Health {
    fn from(s: Stored) -> Self {
        Health {
            until: s.until_ms.map(time),
            why: s.why,
            minute: s.minute_ms.into_iter().map(time).collect(),
            day: s.day,
        }
    }
}

fn day_of(now: SystemTime, hour: u64) -> u64 {
    now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs().saturating_sub(hour * 3600) / 86400
}

/// gpt-oss writes tool calls natively; with no tools declared the provider refuses the answer
/// (`tool_use_failed`) yet returns it. In the agent's own protocol that call is the answer.
pub fn salvaged_tool_call(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    let e = &v["error"];
    if e["code"] != "tool_use_failed" {
        return None;
    }
    let g: Value = serde_json::from_str(e["failed_generation"].as_str()?).ok()?;
    Some(json!({"tool": g["name"].as_str()?, "args": g["arguments"]}).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(rpm: u32, rpd: u32) -> Provider {
        Provider {
            name: "p".into(),
            kind: Kind::Openai,
            base: String::new(),
            models: vec![],
            rpm,
            rpd,
            reset_utc_hour: 8,
        }
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn the_defaults_parse() {
        assert!(defaults().len() >= 4);
    }

    #[test]
    fn wanted_models_come_best_first_and_newest_first() {
        let offered: Vec<String> =
            ["gemini-2.5-flash", "gemini-3.8-flash", "gemini-3.8-flash-preview", "gemma-3"]
                .map(String::from)
                .to_vec();
        let wanted = ["gemma*".to_string(), "gemini-3*-flash".into(), "gemini-*-flash".into()];
        assert_eq!(
            candidates(&wanted, &offered),
            ["gemma-3", "gemini-3.8-flash", "gemini-2.5-flash"]
        );
        assert!(candidates(&["*:free".into()], &offered).is_empty());
        assert!(glob("qwen/*:free", "qwen/qwen3-coder:free") && !glob("qwen/*:free", "qwen/x"));
    }

    #[test]
    fn a_named_wait_is_believed() {
        let p = provider(0, 0);
        let groq = r#"{"error":{"message":"Rate limit reached. Please try again in 7.66s."}}"#;
        assert_eq!(
            back_at(&p, Some(429), None, groq, at(0)).0,
            at(0) + Duration::from_millis(7660)
        );
        let gemini = r#"{"details":[{"retryDelay": "37s"}]}"#;
        assert_eq!(back_at(&p, Some(429), None, gemini, at(0)).0, at(37));
        assert_eq!(back_at(&p, Some(429), Some("12"), "", at(0)).0, at(12));
        assert_eq!(named_delay("try again in 1m23.5s"), Some(Duration::from_secs_f64(83.5)));
    }

    #[test]
    fn a_spent_day_waits_for_the_providers_reset_and_overload_is_short() {
        let p = provider(0, 0);
        let body =
            "Quota exceeded for metric: generate_content_free_tier_requests, limit: 250 PerDay";
        // 10:00 UTC on day 1: the 08:00 UTC reset has passed, so the next is day 2 08:00.
        let (back, why) = back_at(&p, Some(429), None, body, at(86400 + 36000));
        assert_eq!((back, why.as_str()), (at(2 * 86400 + 28800), "daily quota spent"));
        assert_eq!(back_at(&p, Some(503), None, "high demand", at(0)).0, at(30));
        assert_eq!(back_at(&p, Some(400), None, "", at(5)).0, at(5));
        assert_eq!(back_at(&p, Some(401), None, "", at(0)).0, at(86400));
    }

    #[test]
    fn documented_limits_are_kept_before_asking() {
        let p = provider(2, 3);
        let mut h = Health::default();
        h.spent(&p, at(100));
        h.spent(&p, at(110));
        assert_eq!(h.blocked(&p, at(120)).map(|b| b.0), Some(at(160)));
        assert_eq!(h.blocked(&p, at(161)), None);
        h.spent(&p, at(161));
        // Day 0's third request: blocked until the 08:00 UTC reset.
        let (back, why) = h.blocked(&p, at(200)).unwrap();
        assert_eq!((back, why.as_str()), (at(28800), "at its 3 requests a day"));
    }

    #[test]
    fn health_survives_being_stored() {
        let p = provider(2, 3);
        let mut h = Health::default();
        h.spent(&p, at(100));
        h.spent(&p, at(110));
        let json = serde_json::to_string(&Stored::from(&h)).unwrap();
        let mut back: Health = serde_json::from_str::<Stored>(&json).unwrap().into();
        assert_eq!(back.blocked(&p, at(120)).map(|b| b.0), Some(at(160)));
    }

    #[test]
    fn a_refused_native_tool_call_becomes_the_agents_json() {
        let body = r#"{"error":{"message":"Tool choice is none, but model called a tool","code":"tool_use_failed","failed_generation":"{\"name\": \"tool_build\", \"arguments\": {\"name\": \"primes\", \"source\": \"pub fn call() {\\n}\"}}"}}"#;
        let got: Value = serde_json::from_str(&salvaged_tool_call(body).unwrap()).unwrap();
        assert_eq!(got["tool"], "tool_build");
        assert_eq!(got["args"]["source"], "pub fn call() {\n}");
        assert_eq!(salvaged_tool_call(r#"{"error":{"code":"other"}}"#), None);
    }
}
