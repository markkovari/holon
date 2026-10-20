use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct AgentInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub triggers: Vec<Value>,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct RunInfo {
    pub id: String,
    pub trigger: String,
    pub status: String,
    #[serde(default)]
    pub answer: String,
    /// Seconds; older records lack `finished_ms`.
    #[serde(default)]
    pub finished: u64,
    #[serde(default)]
    pub finished_ms: u64,
    #[serde(default)]
    pub project: String,
}

impl RunInfo {
    pub fn finished_at_ms(&self) -> u64 {
        if self.finished_ms > 0 {
            self.finished_ms
        } else {
            self.finished * 1000
        }
    }
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
    /// Round-tripped so a `put_project` never un-archives one.
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub paused_by_archive: Vec<String>,
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

pub struct Spoken {
    pub audio: Vec<u8>,
    pub duration_ms: u64,
    pub waveform: Vec<u16>,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Trashed {
    pub id: String,
    pub name: String,
    pub deleted_at: u64,
    #[serde(default)]
    pub projects: Vec<String>,
    #[serde(default)]
    pub runs: usize,
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

    pub fn rank(
        &self,
        message: &str,
        candidates: &[(String, String)],
    ) -> Option<Vec<(String, f32)>> {
        let embed = |texts: Vec<String>, kind: &str| -> Option<Vec<Vec<f32>>> {
            let r = self
                .runner
                .post(format!("{}/embed", self.base))
                .bearer_auth(&self.token)
                .json(&serde_json::json!({"texts": texts, "kind": kind}))
                .send()
                .ok()?;
            if !r.status().is_success() {
                return None;
            }
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

    pub fn runs(&self, agent: &str, n: usize) -> Result<Vec<RunInfo>, String> {
        self.admin(&format!("/agents/{agent}/runs?limit={n}"))
    }

    pub fn trash(&self) -> Result<Vec<Trashed>, String> {
        self.admin("/trash")
    }

    pub fn restore(&self, id: &str) -> Result<Trashed, String> {
        self.call(reqwest::Method::POST, &format!("/trash/{id}/restore"))
    }

    pub fn purge(&self, id: &str) -> Result<Trashed, String> {
        self.call(reqwest::Method::DELETE, &format!("/trash/{id}"))
    }

    fn call<T: for<'a> Deserialize<'a>>(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<T, String> {
        let r = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .send()
            .map_err(|e| e.to_string())?;
        if !r.status().is_success() {
            return Err(r.text().unwrap_or_default());
        }
        r.json().map_err(|e| e.to_string())
    }

    pub fn archive_project(&self, name: &str) -> Result<Vec<String>, String> {
        let v: Value = self.call(reqwest::Method::POST, &format!("/projects/{name}/archive"))?;
        Ok(v["paused"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default())
    }

    pub fn unarchive_project(&self, name: &str) -> Result<Vec<String>, String> {
        let v: Value = self.call(reqwest::Method::POST, &format!("/projects/{name}/unarchive"))?;
        Ok(v["resumed"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default())
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

    pub fn upload_inbox(
        &self,
        agent: &str,
        filename: &str,
        bytes: Vec<u8>,
    ) -> Result<String, String> {
        let r = self
            .runner
            .post(format!("{}/agents/{agent}/inbox", self.base))
            .query(&[("name", filename)])
            .bearer_auth(&self.token)
            .body(bytes)
            .send()
            .map_err(|e| e.to_string())?;
        let ok = r.status().is_success();
        let text = r.text().unwrap_or_default();
        if !ok {
            return Err(text);
        }
        serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["file"].as_str().map(String::from))
            .ok_or_else(|| "unexpected reply".into())
    }

    /// (transcribe, speak)
    pub fn speech_caps(&self) -> (bool, bool) {
        match self.admin::<Value>("/speech") {
            Ok(v) => {
                (v["transcribe"].as_bool().unwrap_or(false), v["speak"].as_bool().unwrap_or(false))
            }
            Err(_) => (false, false),
        }
    }

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

    pub fn run(
        &self,
        agent: &str,
        text: &str,
        why: &str,
        traceparent: Option<&str>,
        project: Option<&str>,
        attach: &[String],
    ) -> (bool, String) {
        // POST, so a long conversation is a body and not part of the URL.
        let mut req = self
            .runner
            .post(format!("{}/agents/{agent}/run", self.base))
            .query(&[("why", why)])
            .query(&project.map(|p| [("project", p)]).unwrap_or_default())
            .body(text.to_string());
        if !attach.is_empty() {
            req = req.query(&[("attach", attach.join(","))]);
        }
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
