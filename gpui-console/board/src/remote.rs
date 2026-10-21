//! The board on a running `agent-runtime`, over its admin API: the runtime Element talks to is the one shown here.

use std::time::Duration;

use agent_wire::{Event, RunRecord};
use gpui::{App, Entity};
use serde_json::Value;

use super::agents::AgentInfo;
use super::keys::{KeyRow, NewKey};
use super::tree::AgentTree;
use super::view::{now_ms, Actions, AgentChoice, Scope};

pub struct Remote {
    url: String,
    token: String,
}

fn http(timeout: Option<Duration>) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(timeout).build().unwrap()
}

/// The short name a sidebar shows for a model, as the console names it.
pub fn model_name(m: &Value) -> String {
    match m["kind"].as_str().unwrap_or("local") {
        "fallback" => {
            let models = m["models"].as_array().cloned().unwrap_or_default();
            match models.split_first() {
                Some((first, [])) => model_name(first),
                Some((first, rest)) => format!("{} (+{} fallback)", model_name(first), rest.len()),
                None => "fallback (empty)".into(),
            }
        }
        "open_ai" | "anthropic" => m["model"].as_str().unwrap_or_default().to_string(),
        other => other.to_string(),
    }
}

pub fn info_of(spec: &Value, projects: &[Value]) -> AgentInfo {
    let name = spec["name"].as_str().unwrap_or_default().to_string();
    let strings = |v: &Value| -> Vec<String> {
        v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect()
    };
    AgentInfo {
        description: spec["description"].as_str().unwrap_or_default().to_string(),
        model: model_name(&spec["model"]),
        paused: spec["paused"].as_bool().unwrap_or(false),
        triggers: spec["triggers"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| match t["kind"].as_str() {
                Some("schedule") => format!("every {}", t["cron"].as_str().unwrap_or("?")),
                Some("event") => format!("on {}", t["topic"].as_str().unwrap_or("?")),
                _ => format!("on {} change", t["ns"].as_str().unwrap_or("?")),
            })
            .collect(),
        capabilities: spec["capabilities"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["name"].as_str().map(String::from))
            .collect(),
        projects: projects
            .iter()
            .filter(|p| strings(&p["agents"]).contains(&name))
            .filter_map(|p| p["name"].as_str().map(String::from))
            .collect(),
        api_keys: strings(&spec["api_keys"]),
        name,
    }
}

impl Remote {
    pub fn new(url: &str, token: &str) -> Self {
        Remote { url: url.trim_end_matches('/').to_string(), token: token.to_string() }
    }

    fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<String>,
    ) -> Result<String, String> {
        let mut req = http(Some(Duration::from_secs(10)))
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.header("content-type", "application/json").body(b);
        }
        let r = req
            .send()
            .map_err(|e| format!("The agent runtime at {} did not answer: {e}", self.url))?;
        let ok = r.status().is_success();
        let text = r.text().unwrap_or_default();
        if ok {
            Ok(text)
        } else {
            Err(text)
        }
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        serde_json::from_str(&self.call(reqwest::Method::GET, path, None)?)
            .map_err(|e| e.to_string())
    }

    /// What already happened: the runs the runtime kept, and approvals still waiting.
    pub fn history(&self) -> (Vec<RunRecord>, Vec<Event>) {
        let specs = self.get("/agents").unwrap_or_default();
        let runs = specs
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["name"].as_str())
            .flat_map(|n| {
                self.get(&format!("/agents/{n}/runs?limit=50"))
                    .ok()
                    .and_then(|v| serde_json::from_value::<Vec<RunRecord>>(v).ok())
                    .unwrap_or_default()
            })
            .collect();
        let waiting = self
            .get("/approvals")
            .unwrap_or_default()
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| {
                Some(Event::ApprovalRequested {
                    id: p["id"].as_u64()?,
                    agent: p["agent"].as_str()?.to_string(),
                    tool: p["tool"].as_str()?.to_string(),
                    args: p["args"].clone(),
                    chain: serde_json::from_value(p["chain"].clone()).unwrap_or_default(),
                })
            })
            .collect();
        (runs, waiting)
    }

    /// Seeds the tree, then applies the runtime's events as they come, reconnecting when the stream drops.
    pub fn follow(&self, tree: Entity<AgentTree>, cx: &mut App) {
        let (runs, waiting) = self.history();
        tree.update(cx, |t, cx| {
            t.seed(&runs);
            for ev in &waiting {
                t.apply(ev, now_ms());
            }
            cx.notify();
        });
        let (tx, rx) = std::sync::mpsc::channel::<Event>();
        let (url, token) = (self.url.clone(), self.token.clone());
        std::thread::spawn(move || loop {
            if let Ok(r) = http(None).get(format!("{url}/stream")).bearer_auth(&token).send() {
                use std::io::BufRead;
                for line in std::io::BufReader::new(r).lines() {
                    let Ok(line) = line else { break };
                    if let Ok(ev) = serde_json::from_str::<Event>(&line) {
                        if tx.send(ev).is_err() {
                            return;
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        });
        cx.spawn(async move |cx| loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let batch: Vec<Event> = rx.try_iter().collect();
            if batch.is_empty() {
                continue;
            }
            cx.update(|app| {
                tree.update(app, |t, cx| {
                    for ev in &batch {
                        t.apply(ev, now_ms());
                    }
                    cx.notify();
                })
            });
        })
        .detach();
    }
}

impl Actions for Remote {
    fn answer(&self, id: u64, approve: bool, scope: Scope, _cx: &mut App) {
        let verdict = if approve { "approve" } else { "deny" };
        let path = format!("/approvals/{id}/{verdict}?scope={}", scope.as_str());
        let _ = self.call(reqwest::Method::POST, &path, None);
    }

    fn agents(&self, cx: &App) -> Vec<AgentChoice> {
        let mut v: Vec<AgentChoice> = self
            .roster(cx)
            .into_iter()
            .filter(|a| !a.paused)
            .map(|a| AgentChoice {
                codes: a.capabilities.iter().any(|c| c == "session_start"),
                name: a.name,
                about: a.description,
            })
            .collect();
        v.sort_by_key(|c| !c.codes);
        v
    }

    fn start(&self, agent: &str, goal: &str, _cx: &mut App) -> Result<(), String> {
        let (url, token) = (self.url.clone(), self.token.clone());
        let (agent, goal) = (agent.to_string(), goal.to_string());
        // A run can wait minutes on an approval; its progress arrives on the stream.
        std::thread::spawn(move || {
            let _ = http(None)
                .post(format!("{url}/agents/{agent}/run"))
                .bearer_auth(token)
                .body(goal)
                .send();
        });
        Ok(())
    }

    fn roster(&self, _cx: &App) -> Vec<AgentInfo> {
        let projects = self.get("/projects").unwrap_or_default();
        let projects = projects.as_array().cloned().unwrap_or_default();
        self.get("/agents")
            .unwrap_or_default()
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| info_of(s, &projects))
            .collect()
    }

    fn keys(&self, _cx: &App) -> Result<Vec<KeyRow>, String> {
        let v = self.get("/keys")?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .map(|k| KeyRow {
                name: k["name"].as_str().unwrap_or_default().to_string(),
                reference: k["ref"].as_str().unwrap_or_default().to_string(),
            })
            .collect())
    }

    fn put_key(&self, k: &NewKey, _cx: &mut App) -> Result<(), String> {
        if k.name.is_empty() {
            return Err("A key needs a name, e.g. github.".into());
        }
        let body = serde_json::json!({ "host": k.host, "key": k.key, "header": k.header });
        self.call(reqwest::Method::PUT, &format!("/keys/{}", k.name), Some(body.to_string()))
            .map(|_| ())
    }

    fn remove_key(&self, name: &str, _cx: &mut App) -> Result<(), String> {
        self.call(reqwest::Method::DELETE, &format!("/keys/{name}"), None).map(|_| ())
    }

    fn grant(&self, key: &str, agent: &str, on: bool, _cx: &mut App) -> Result<(), String> {
        let m = if on { reqwest::Method::PUT } else { reqwest::Method::DELETE };
        self.call(m, &format!("/keys/{key}/agents/{agent}"), None).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_agent_reads_as_the_console_shows_it() {
        let spec = json!({
            "name": "rower",
            "description": "rowing",
            "model": {"kind": "fallback", "models": [{"kind": "open_ai", "model": "deepseek-chat"}, {"kind": "local"}]},
            "capabilities": [{"name": "http_get"}],
            "triggers": [{"kind": "schedule", "cron": "@daily", "prompt": "x"}],
            "api_keys": ["concept2"],
        });
        let a = info_of(&spec, &[json!({"name": "rowing", "agents": ["rower", "coach"]})]);
        assert_eq!(a.model, "deepseek-chat (+1 fallback)");
        assert_eq!(
            (a.projects, a.api_keys, a.triggers),
            (
                vec!["rowing".to_string()],
                vec!["concept2".to_string()],
                vec!["every @daily".to_string()]
            )
        );
        assert_eq!(a.capabilities, ["http_get"]);
        assert!(!a.paused);
    }
}
