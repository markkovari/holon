//! The board on a running `agent-runtime`, over its `/v1` contract (ADR-0120): the runtime Element talks to.

use std::time::Duration;

use agent_wire::{Event, Pending, RunRecord, Sequenced};
use gpui::{App, Entity};
use serde_json::{json, Value};

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

/// A `/v1` agent as the board shows it.
pub fn info_of(agent: &Value) -> AgentInfo {
    let strings = |v: &Value| -> Vec<String> {
        v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect()
    };
    let spec = &agent["spec"];
    AgentInfo {
        name: agent["name"].as_str().unwrap_or_default().to_string(),
        description: agent["description"].as_str().unwrap_or_default().to_string(),
        model: agent["model_label"].as_str().unwrap_or_default().to_string(),
        paused: agent["paused"].as_bool().unwrap_or(false),
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
        projects: strings(&agent["projects"]),
        api_keys: strings(&spec["api_keys"]),
    }
}

/// What a refusal says: a problem's `detail`, else the body.
fn said(text: String) -> String {
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["detail"].as_str().map(String::from))
        .unwrap_or(text)
}

impl Remote {
    pub fn new(url: &str, token: &str) -> Self {
        Remote { url: url.trim_end_matches('/').to_string(), token: token.to_string() }
    }

    fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let mut req = http(Some(Duration::from_secs(10)))
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req
            .send()
            .map_err(|e| format!("The agent runtime at {} did not answer: {e}", self.url))?;
        let ok = r.status().is_success();
        let text = r.text().unwrap_or_default();
        if !ok {
            return Err(said(text));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        self.call(reqwest::Method::GET, path, None)
    }

    /// What already happened: the runs the runtime kept, approvals still waiting, and the seq it reflects.
    pub fn history(&self) -> (Vec<RunRecord>, Vec<Event>, u64) {
        let state = self.get("/v1/state").unwrap_or_default();
        let agents = self.get("/v1/agents").unwrap_or_default();
        let runs = agents["agents"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a["name"].as_str())
            .flat_map(|n| {
                self.get(&format!("/v1/agents/{n}/runs?limit=50"))
                    .ok()
                    .and_then(|v| serde_json::from_value::<Vec<RunRecord>>(v["runs"].clone()).ok())
                    .unwrap_or_default()
            })
            .collect();
        let waiting = serde_json::from_value::<Vec<Pending>>(state["approvals"].clone())
            .unwrap_or_default()
            .into_iter()
            .map(|p| Event::ApprovalRequested {
                id: p.id,
                agent: p.agent,
                tool: p.tool,
                args: p.args,
                chain: p.chain,
            })
            .collect();
        (runs, waiting, state["seq"].as_u64().unwrap_or(0))
    }

    /// Seeds the tree from the state, then applies the events after it, resuming from the last seq seen.
    pub fn follow(&self, tree: Entity<AgentTree>, cx: &mut App) {
        let (runs, waiting, seq) = self.history();
        tree.update(cx, |t, cx| {
            t.seed(&runs);
            for ev in &waiting {
                t.apply(ev, now_ms());
            }
            cx.notify();
        });
        let (tx, rx) = std::sync::mpsc::channel::<Event>();
        let (url, token) = (self.url.clone(), self.token.clone());
        std::thread::spawn(move || {
            use std::io::BufRead;
            let mut after = seq;
            loop {
                let r = http(None)
                    .get(format!("{url}/v1/events?after_seq={after}"))
                    .bearer_auth(&token)
                    .send();
                if let Ok(r) = r.and_then(|r| r.error_for_status()) {
                    for line in std::io::BufReader::new(r).lines().map_while(Result::ok) {
                        let Some(e) = line
                            .strip_prefix("data: ")
                            .and_then(|d| serde_json::from_str::<Sequenced>(d).ok())
                        else {
                            continue;
                        };
                        after = e.seq;
                        if tx.send(e.event).is_err() {
                            return;
                        }
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
            }
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
        let body = json!({ "approve": approve, "scope": scope.as_str() });
        let _ = self.call(reqwest::Method::POST, &format!("/v1/approvals/{id}"), Some(body));
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

    /// Returns at once: the run's progress arrives on the stream, and the bridge posts it in Element.
    fn start(&self, agent: &str, goal: &str, _cx: &mut App) -> Result<(), String> {
        let body = json!({ "input": goal, "why": "board" });
        self.call(reqwest::Method::POST, &format!("/v1/agents/{agent}/runs"), Some(body))
            .map(|_| ())
    }

    fn roster(&self, _cx: &App) -> Vec<AgentInfo> {
        let v = self.get("/v1/agents").unwrap_or_default();
        v["agents"].as_array().into_iter().flatten().map(info_of).collect()
    }

    fn keys(&self, _cx: &App) -> Result<Vec<KeyRow>, String> {
        let v = self.get("/v1/keys")?;
        Ok(v["keys"]
            .as_array()
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
        let body = json!({ "host": k.host, "key": k.key, "header": k.header });
        self.call(reqwest::Method::PUT, &format!("/v1/keys/{}", k.name), Some(body)).map(|_| ())
    }

    fn remove_key(&self, name: &str, _cx: &mut App) -> Result<(), String> {
        self.call(reqwest::Method::DELETE, &format!("/v1/keys/{name}"), None).map(|_| ())
    }

    fn grant(&self, key: &str, agent: &str, on: bool, _cx: &mut App) -> Result<(), String> {
        let m = if on { reqwest::Method::PUT } else { reqwest::Method::DELETE };
        self.call(m, &format!("/v1/keys/{key}/agents/{agent}"), None).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_reads_as_the_console_shows_it() {
        let agent = json!({
            "name": "rower",
            "description": "rowing",
            "paused": false,
            "model_label": "deepseek-chat (+1 fallback)",
            "projects": ["rowing"],
            "spec": {
                "capabilities": [{"name": "http_get"}],
                "triggers": [{"kind": "schedule", "cron": "@daily", "prompt": "x"}],
                "api_keys": ["concept2"],
            },
        });
        let a = info_of(&agent);
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

    #[test]
    fn a_refusal_says_its_detail() {
        assert_eq!(
            said(r#"{"type":"about:blank","status":404,"detail":"no agent named x"}"#.into()),
            "no agent named x"
        );
        assert_eq!(said("plain".into()), "plain");
    }
}
