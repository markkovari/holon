//! Managing agents from an agent: the tools a `holon`-style agent uses to list, show, create,
//! change, pause and delete other agents. The registry stays the runtime's; these only build the
//! spec a call describes. Whether a change happens is up to the owner (create/change/delete are
//! sensitive tools: every call waits for approval).

use serde_json::{json, Map, Value};

use crate::spec::AgentSpec;

/// The spec a `agent_save` call means. With an existing agent, the call's fields replace the
/// same fields of it and everything else is kept; otherwise they build a new one. Capabilities
/// may be plain names (`["http_get"]`) or full objects.
pub fn build_spec(existing: Option<&AgentSpec>, args: &Value) -> Result<AgentSpec, String> {
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
        // `schedule` is the friendly spelling of one cron trigger
        if k == "schedule" {
            let cron = v.as_str().unwrap_or_default();
            if !cron.is_empty() {
                let prompt = given
                    .get("prompt")
                    .and_then(Value::as_str)
                    .unwrap_or("Do your scheduled task.");
                base.insert(
                    "triggers".into(),
                    json!([{"kind": "schedule", "cron": cron, "prompt": prompt}]),
                );
            }
        } else if k != "prompt" {
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
    serde_json::from_value(Value::Object(base)).map_err(|e| format!("not a valid agent: {e}"))
}

/// One line per agent for `agents_list`.
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
