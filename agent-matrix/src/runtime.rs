//! The agent runtime's HTTP API, as the bridge uses it. The bridge shares no code
//! with the runtime: this contract (agents, projects, run, approvals) is all it knows.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct AgentInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct ProjectInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub lead: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Approval {
    pub id: u64,
    pub agent: String,
    pub tool: String,
    pub args: Value,
    #[serde(default)]
    pub chain: Vec<String>,
}

/// Speech the runtime produced, with what a chat client shows for a voice message.
pub struct Spoken {
    pub audio: Vec<u8>,
    pub duration_ms: u64,
    pub waveform: Vec<u16>,
}

pub struct Runtime {
    base: String,
    token: String,
    http: reqwest::blocking::Client,
    /// A run can wait minutes on an approval, so runs get a long timeout.
    runner: reqwest::blocking::Client,
}

impl Runtime {
    pub fn new(base: &str, token: &str) -> Self {
        let client = |secs| {
            reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(secs))
                .build()
                .expect("http client")
        };
        Self {
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: client(15),
            runner: client(900),
        }
    }

    fn admin<T: for<'a> Deserialize<'a>>(&self, path: &str) -> Result<T, String> {
        let r = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .send()
            .map_err(|e| e.to_string())?;
        if !r.status().is_success() {
            return Err(format!("{}: {}", r.status(), r.text().unwrap_or_default()));
        }
        r.json().map_err(|e| e.to_string())
    }

    fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<(), String> {
        let mut req =
            self.http.request(method, format!("{}{path}", self.base)).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().map_err(|e| e.to_string())?;
        if r.status().is_success() {
            Ok(())
        } else {
            Err(r.text().unwrap_or_default())
        }
    }

    pub fn agents(&self) -> Result<Vec<AgentInfo>, String> {
        self.admin("/agents")
    }

    /// Rank `candidates` (name, description) by how well each fits `message`, best first,
    /// using the embedding service. `None` when it is unreachable.
    pub fn rank(
        &self,
        embed_url: &str,
        message: &str,
        candidates: &[(String, String)],
    ) -> Option<Vec<(String, f32)>> {
        let embed = |texts: Vec<String>, kind: &str| -> Option<Vec<Vec<f32>>> {
            let r = self
                .http
                .post(format!("{}/embed", embed_url.trim_end_matches('/')))
                .json(&serde_json::json!({"texts": texts, "kind": kind, "dim": 256}))
                .send()
                .ok()?;
            let v: Value = r.json().ok()?;
            v["vectors"]
                .as_array()?
                .iter()
                .map(|row| {
                    row.as_array()
                        .map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect())
                })
                .collect()
        };
        let docs =
            embed(candidates.iter().map(|(n, d)| format!("{n}: {d}")).collect(), "document")?;
        let query = embed(vec![message.to_string()], "query")?.pop()?;
        let mut out: Vec<(String, f32)> = candidates
            .iter()
            .zip(docs)
            .map(|((n, _), d)| (n.clone(), query.iter().zip(&d).map(|(a, b)| a * b).sum()))
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1));
        Some(out)
    }

    pub fn projects(&self) -> Result<Vec<ProjectInfo>, String> {
        self.admin("/projects")
    }

    pub fn approvals(&self) -> Result<Vec<Approval>, String> {
        self.admin("/approvals")
    }

    pub fn resolve(&self, id: u64, approve: bool) -> Result<(), String> {
        self.send(
            reqwest::Method::POST,
            &format!("/approvals/{id}/{}", if approve { "approve" } else { "deny" }),
            None,
        )
    }

    pub fn put_project(&self, p: &ProjectInfo) -> Result<(), String> {
        self.send(
            reqwest::Method::PUT,
            &format!("/projects/{}", p.name),
            Some(serde_json::to_value(p).unwrap()),
        )
    }

    pub fn delete_project(&self, name: &str) -> Result<(), String> {
        self.send(reqwest::Method::DELETE, &format!("/projects/{name}"), None)
    }

    pub fn add_to_project(&self, project: &str, agent: &str) -> Result<(), String> {
        self.send(reqwest::Method::POST, &format!("/projects/{project}/agents/{agent}"), None)
    }

    pub fn remove_from_project(&self, project: &str, agent: &str) -> Result<(), String> {
        self.send(reqwest::Method::DELETE, &format!("/projects/{project}/agents/{agent}"), None)
    }

    /// Whether the runtime can transcribe and speak (`GET /speech`).
    pub fn speech_caps(&self) -> (bool, bool) {
        match self.admin::<Value>("/speech") {
            Ok(v) => {
                (v["transcribe"].as_bool().unwrap_or(false), v["speak"].as_bool().unwrap_or(false))
            }
            Err(_) => (false, false),
        }
    }

    /// Audio to text. `Err` carries the runtime's reason (not configured, bad audio,
    /// a language it has no model for).
    pub fn transcribe(&self, audio: Vec<u8>) -> Result<String, String> {
        let r = self
            .runner
            .post(format!("{}/speech/transcribe", self.base))
            .bearer_auth(&self.token)
            .body(audio)
            .send()
            .map_err(|e| e.to_string())?;
        let status = r.status();
        let text = r.text().unwrap_or_default();
        if !status.is_success() {
            return Err(text);
        }
        serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["text"].as_str().map(String::from))
            .ok_or_else(|| "unexpected reply".into())
    }

    /// Text to a voice message (Opus in Ogg), with its length and waveform.
    pub fn speak(&self, text: &str) -> Result<Spoken, String> {
        let r = self
            .runner
            .post(format!("{}/speech/speak", self.base))
            .bearer_auth(&self.token)
            .body(text.to_string())
            .send()
            .map_err(|e| e.to_string())?;
        if !r.status().is_success() {
            return Err(r.text().unwrap_or_default());
        }
        let duration_ms = r
            .headers()
            .get("x-holon-duration-ms")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let waveform = r
            .headers()
            .get("x-holon-waveform")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(',').filter_map(|n| n.parse().ok()).collect())
            .unwrap_or_default();
        let audio = r.bytes().map_err(|e| e.to_string())?.to_vec();
        Ok(Spoken { audio, duration_ms, waveform })
    }

    /// Runs `agent` on `text`. The runtime's answer, or its error, as text; the
    /// bool says whether it succeeded. `traceparent` joins an existing trace.
    pub fn run(
        &self,
        agent: &str,
        text: &str,
        why: &str,
        traceparent: Option<&str>,
    ) -> (bool, String) {
        // POST, so a long conversation is a body and not part of the URL.
        let mut req = self
            .runner
            .post(format!("{}/agents/{agent}/run", self.base))
            .query(&[("why", why)])
            .body(text.to_string());
        if let Some(tp) = traceparent {
            req = req.header("traceparent", tp);
        }
        match req.send() {
            Ok(r) => {
                let ok = r.status().is_success();
                (ok, r.text().unwrap_or_default())
            }
            Err(e) => (false, format!("could not reach the agent runtime: {e}")),
        }
    }
}
