//! The tools an agent uses to manage other agents: they only build specs; the registry stays the runtime's.

use serde_json::{json, Map, Value};

use crate::spec::AgentSpec;

pub fn build_spec(existing: Option<&AgentSpec>, args: &Value) -> Result<AgentSpec, String> {
    build_spec_with(existing, args, &|_| None)
}

pub fn build_spec_with(
    existing: Option<&AgentSpec>,
    args: &Value,
    model_alias: &dyn Fn(&str) -> Option<Value>,
) -> Result<AgentSpec, String> {
    let given = match args {
        Value::Object(m) => m,
        _ => return Err("args must be an object with at least `name`".into()),
    };
    let name = given.get("name").and_then(Value::as_str).unwrap_or_default();
    if name.is_empty() {
        return Err("`name` is required".into());
    }
    let mut base: Map<String, Value> = match existing {
        Some(s) => match serde_json::to_value(s) {
            Ok(Value::Object(m)) => m,
            _ => return Err("could not read the current spec".into()),
        },
        None => {
            if given.get("description").and_then(Value::as_str).unwrap_or_default().is_empty() {
                return Err("a new agent needs a `description` (what it is for, written to it as \
                            instructions)"
                    .into());
            }
            Map::new()
        }
    };
    for (k, v) in given {
        if k == "schedule" {
            let cron = v.as_str().unwrap_or_default();
            if !cron.is_empty() {
                let prompt = given
                    .get("prompt")
                    .and_then(Value::as_str)
                    .unwrap_or("Do your scheduled task.");
                let mut trigger = json!({"kind": "schedule", "cron": cron, "prompt": prompt});
                if let Some(p) =
                    given.get("project").and_then(Value::as_str).filter(|p| !p.is_empty())
                {
                    trigger["project"] = json!(p);
                }
                if given.get("notify").and_then(Value::as_bool) == Some(true) {
                    trigger["notify"] = json!(true);
                }
                base.insert("triggers".into(), json!([trigger]));
            }
        } else if k == "model" {
            let alias = v.as_str().map(|n| (n, model_alias(&n.to_lowercase())));
            match alias {
                Some((_, Some(block))) => base.insert(k.clone(), block),
                Some((n, None)) => return Err(format!("no model called `{n}`")),
                None => base.insert(k.clone(), v.clone()),
            };
        } else if !matches!(k.as_str(), "prompt" | "project" | "notify") {
            base.insert(k.clone(), v.clone());
        }
    }
    if let Some(Value::Array(caps)) = base.get_mut("capabilities") {
        for c in caps.iter_mut() {
            if let Value::String(n) = c {
                *c = json!({ "name": n });
            }
        }
    }
    let mut spec: AgentSpec = serde_json::from_value(Value::Object(base))
        .map_err(|e| format!("not a valid agent: {e}"))?;
    // http_get with no allowed host can do nothing: allow the hosts its own instructions name.
    if spec.has_capability("http_get") && spec.allow_hosts.is_empty() {
        let mut text = spec.description.clone();
        for t in &spec.triggers {
            if let crate::spec::Trigger::Schedule { prompt, .. } = t {
                text.push(' ');
                text.push_str(prompt);
            }
        }
        spec.allow_hosts = hosts_in(&text);
    }
    Ok(spec)
}

pub fn hosts_in(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for scheme in ["https://", "http://"] {
        for (i, _) in text.match_indices(scheme) {
            let rest = &text[i + scheme.len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == ':'))
                .unwrap_or(rest.len());
            let host = rest[..end].trim_end_matches('.').to_lowercase();
            if !host.is_empty() && !out.contains(&host) {
                out.push(host);
            }
        }
    }
    out
}

pub fn summary(s: &AgentSpec) -> String {
    let first = s.description.split(['.', '\n']).next().unwrap_or_default().trim();
    let caps: Vec<&str> = s.capabilities.iter().map(|c| c.name.as_str()).collect();
    format!(
        "{}{} — {} [{}]",
        s.name,
        if s.paused { " (paused)" } else { "" },
        first,
        caps.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_agent_comes_from_a_name_a_description_and_plain_capability_names() {
        let s = build_spec(
            None,
            &json!({"name": "news", "description": "Summarise the news.", "capabilities": ["http_get", "remember"],
                    "schedule": "@every 1h", "prompt": "Read the headlines."}),
                    )
        .unwrap();
        assert_eq!(s.name, "news");
        assert!(s.has_capability("http_get") && s.has_capability("remember"));
        assert_eq!(s.triggers.len(), 1);
        assert!(build_spec(None, &json!({"name": "x"})).unwrap_err().contains("description"));
        assert!(build_spec(None, &json!({"description": "d"})).unwrap_err().contains("name"));
        assert!(build_spec(None, &json!("nope")).is_err());
    }

    #[test]
    fn an_agent_that_may_fetch_gets_the_hosts_its_instructions_name() {
        let s = build_spec(
            None,
            &json!({"name": "jokes", "description": "Fetch https://official-joke-api.appspot.com/random_joke then riff. Also http://example.org/x.",
                    "capabilities": ["http_get"]}),
        )
        .unwrap();
        assert_eq!(s.allow_hosts, ["official-joke-api.appspot.com", "example.org"]);
        let s = build_spec(
            None,
            &json!({"name": "jokes", "description": "Fetch https://a.com/x", "capabilities": ["http_get"], "allow_hosts": ["b.com"]}),
        )
        .unwrap();
        assert_eq!(s.allow_hosts, ["b.com"]);
        assert_eq!(hosts_in("see https://a.b:8080/x and nothing"), ["a.b:8080"]);
    }

    #[test]
    fn a_model_name_stands_for_its_whole_block_and_an_unknown_one_is_refused() {
        let alias = |n: &str| {
            (n == "qwen").then(|| json!({"kind": "open_ai", "base_url": "http://x", "model": "q"}))
        };
        let old = AgentSpec::new("kevin", "Jokes.");
        let s = build_spec_with(Some(&old), &json!({"name": "kevin", "model": "Qwen"}), &alias)
            .unwrap();
        assert!(matches!(s.model, crate::spec::ModelSpec::OpenAi { .. }));
        assert!(build_spec_with(Some(&old), &json!({"name": "kevin", "model": "gpt"}), &alias)
            .unwrap_err()
            .contains("no model called"));
        let s = build_spec_with(
            Some(&old),
            &json!({"name": "kevin", "model": {"kind": "local"}}),
            &alias,
        )
        .unwrap();
        assert_eq!(s.model, crate::spec::ModelSpec::Local);
    }

    #[test]
    fn changing_an_agent_replaces_only_the_fields_given() {
        let mut old = AgentSpec::new("coach", "Coaches rowing.");
        old.capabilities.push(crate::spec::Capability::named("remember"));
        let s = build_spec(
            Some(&old),
            &json!({"name": "coach", "description": "Coaches rowing, briefly."}),
        )
        .unwrap();
        assert_eq!(s.description, "Coaches rowing, briefly.");
        assert!(s.has_capability("remember"), "untouched fields are kept");
        let s = build_spec(Some(&old), &json!({"name": "coach", "capabilities": ["http_get"]}))
            .unwrap();
        assert!(s.has_capability("http_get") && !s.has_capability("remember"));
        assert!(build_spec(Some(&old), &json!({"name": "coach", "max_steps": "many"})).is_err());
    }

    #[test]
    fn the_summary_is_one_line_with_the_state() {
        let mut s = AgentSpec::new("a", "First sentence. Second.");
        s.paused = true;
        assert_eq!(summary(&s), "a (paused) — First sentence [remember, recall, now]");
    }
}
