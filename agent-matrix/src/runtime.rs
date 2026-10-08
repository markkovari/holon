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

    /// Runs `agent` on `text`. The runtime's answer, or its error, as text; the
    /// bool says whether it succeeded. `traceparent` joins an existing trace.
    pub fn run(&self, agent: &str, text: &str, traceparent: Option<&str>) -> (bool, String) {
        let mut req =
            self.runner.get(format!("{}/agents/{agent}/run", self.base)).query(&[("q", text)]);
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
