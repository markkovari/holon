use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::http::{bearer, enc, Http, Response};

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
    #[serde(default)]
    pub input: String,
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
    http: Arc<dyn Http>,
}

impl Runtime {
    pub fn new(http: Arc<dyn Http>, base: &str, token: &str) -> Self {
        Self { base: base.trim_end_matches('/').to_string(), token: token.to_string(), http }
    }

    fn req(
        &self,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: Option<&[u8]>,
    ) -> Result<Response, String> {
        let auth = bearer(&self.token);
        let mut headers = vec![("authorization", auth.as_str())];
        if let Some(c) = content_type {
            headers.push(("content-type", c));
        }
        self.http.send(method, &format!("{}{path}", self.base), &headers, body)
    }

    fn json<T: for<'a> Deserialize<'a>>(&self, method: &str, path: &str) -> Result<T, String> {
        let r = self.req(method, path, None, None)?;
        if !r.ok() {
            return Err(said(&r));
        }
        serde_json::from_slice(&r.body).map_err(|e| e.to_string())
    }

    fn admin<T: for<'a> Deserialize<'a>>(&self, path: &str) -> Result<T, String> {
        self.json("GET", path)
    }

    /// A `/v1` list: the array under `key`.
    fn listed<T: for<'a> Deserialize<'a>>(&self, path: &str, key: &str) -> Result<Vec<T>, String> {
        let v: Value = self.admin(path)?;
        serde_json::from_value(v[key].clone()).map_err(|e| e.to_string())
    }

    fn send(&self, method: &str, path: &str, body: Option<Value>) -> Result<(), String> {
        let body = body.map(|b| b.to_string());
        let ctype = body.as_ref().map(|_| "application/json");
        let r = self.req(method, path, ctype, body.as_deref().map(str::as_bytes))?;
        if r.ok() {
            Ok(())
        } else {
            Err(said(&r))
        }
    }

    pub fn agents(&self) -> Result<Vec<AgentInfo>, String> {
        let v: Value = self.admin("/v1/agents")?;
        v["agents"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| serde_json::from_value(a["spec"].clone()).map_err(|e| e.to_string()))
            .collect()
    }

    pub fn agent_spec(&self, name: &str) -> Result<Value, String> {
        self.admin(&format!("/v1/agents/{name}"))
    }

    pub fn update_agent_spec(&self, name: &str, spec: &Value) -> Result<(), String> {
        self.send("PUT", &format!("/v1/agents/{name}"), Some(serde_json::json!({ "spec": spec })))
    }

    pub fn agent_diff(&self, name: &str) -> Result<String, String> {
        let v: Value = self.admin(&format!("/v1/agents/{name}/diff"))?;
        Ok(v["diff"].as_str().unwrap_or_default().to_string())
    }

    pub fn rank(
        &self,
        message: &str,
        candidates: &[(String, String)],
    ) -> Option<Vec<(String, f32)>> {
        let embed = |texts: Vec<String>, kind: &str| -> Option<Vec<Vec<f32>>> {
            let body = serde_json::json!({"texts": texts, "kind": kind}).to_string();
            let r =
                self.req("POST", "/embed", Some("application/json"), Some(body.as_bytes())).ok()?;
            if !r.ok() {
                return None;
            }
            let v = r.json();
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
        self.listed(&format!("/v1/agents/{agent}/runs?limit={n}"), "runs")
    }

    /// Fetches all runs finished after `after_ms` by querying with an increasing limit
    /// until reaching runs older than or at `after_ms`, or exhausting available runs.
    pub fn runs_since(&self, agent: &str, after_ms: u64) -> Result<Vec<RunInfo>, String> {
        paginate_since(after_ms, |limit| self.runs(agent, limit))
    }

    pub fn trash(&self) -> Result<Vec<Trashed>, String> {
        self.admin("/trash")
    }

    pub fn restore(&self, id: &str) -> Result<Trashed, String> {
        self.call("POST", &format!("/trash/{id}/restore"))
    }

    pub fn purge(&self, id: &str) -> Result<Trashed, String> {
        self.call("DELETE", &format!("/trash/{id}"))
    }

    fn call<T: for<'a> Deserialize<'a>>(&self, method: &str, path: &str) -> Result<T, String> {
        self.json(method, path)
    }

    pub fn archive_project(&self, name: &str) -> Result<Vec<String>, String> {
        let v: Value = self.call("POST", &format!("/projects/{name}/archive"))?;
        Ok(v["paused"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default())
    }

    pub fn unarchive_project(&self, name: &str) -> Result<Vec<String>, String> {
        let v: Value = self.call("POST", &format!("/projects/{name}/unarchive"))?;
        Ok(v["resumed"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default())
    }

    pub fn projects(&self) -> Result<Vec<ProjectInfo>, String> {
        self.listed("/v1/projects", "projects")
    }

    pub fn approvals(&self) -> Result<Vec<Approval>, String> {
        self.listed("/v1/approvals", "approvals")
    }

    pub fn resolve(&self, id: u64, approve: bool) -> Result<(), String> {
        let body = serde_json::json!({ "approve": approve });
        self.send("POST", &format!("/v1/approvals/{id}"), Some(body))
    }

    /// The project as `p` says it: its description, then its members, then its lead, who must be one.
    pub fn put_project(&self, p: &ProjectInfo) -> Result<(), String> {
        let path = format!("/v1/projects/{}", p.name);
        let description = serde_json::json!({ "description": p.description });
        self.send("PUT", &path, Some(description))?;
        let now = self.projects()?.into_iter().find(|x| x.name == p.name).unwrap_or_default();
        for a in p.agents.iter().filter(|a| !now.agents.contains(a)) {
            self.add_to_project(&p.name, a)?;
        }
        for a in now.agents.iter().filter(|a| !p.agents.contains(a)) {
            self.remove_from_project(&p.name, a)?;
        }
        if p.lead != now.lead {
            let lead = serde_json::json!({ "lead": p.lead.clone().unwrap_or_default() });
            self.send("PUT", &path, Some(lead))?;
        }
        Ok(())
    }

    pub fn delete_project(&self, name: &str) -> Result<(), String> {
        self.send("DELETE", &format!("/projects/{name}"), None)
    }

    pub fn add_to_project(&self, project: &str, agent: &str) -> Result<(), String> {
        self.send("PUT", &format!("/v1/projects/{project}/agents/{agent}"), None)
    }

    pub fn remove_from_project(&self, project: &str, agent: &str) -> Result<(), String> {
        self.send("DELETE", &format!("/v1/projects/{project}/agents/{agent}"), None)
    }

    pub fn upload_inbox(
        &self,
        agent: &str,
        filename: &str,
        bytes: Vec<u8>,
    ) -> Result<String, String> {
        let path = format!("/agents/{agent}/inbox?name={}", enc(filename));
        let r = self.req("POST", &path, None, Some(&bytes))?;
        if !r.ok() {
            return Err(r.text());
        }
        r.json()["file"].as_str().map(String::from).ok_or_else(|| "unexpected reply".into())
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

    /// Starts transcribing a recording, read back with [`Runtime::transcription`].
    pub fn start_transcribe(&self, audio: Vec<u8>) -> Result<String, String> {
        let r = self.req("POST", "/speech/transcribe?wait=false", None, Some(&audio))?;
        if !r.ok() {
            return Err(said(&r));
        }
        r.json()["job"].as_str().map(String::from).ok_or_else(|| "unexpected reply".into())
    }

    /// A transcription's end: `Done(true, the text)` or `Done(false, why not)`.
    pub fn transcription(&self, job: &str) -> Job {
        self.read_job(&format!("/speech/transcribe/{job}"), |v| {
            (true, v["text"].as_str().unwrap_or_default().into())
        })
    }

    pub fn speak(&self, text: &str) -> Result<Spoken, String> {
        let r = self.req("POST", "/speech/speak", None, Some(text.as_bytes()))?;
        if !r.ok() {
            return Err(r.text());
        }
        let duration_ms = r.header("x-holon-duration-ms").and_then(|v| v.parse().ok()).unwrap_or(0);
        let waveform = r
            .header("x-holon-waveform")
            .map(|v| v.split(',').filter_map(|n| n.parse().ok()).collect())
            .unwrap_or_default();
        Ok(Spoken { audio: r.body, duration_ms, waveform })
    }

    /// Starts a run without waiting for it (a run can wait minutes on an approval); its end is
    /// read back with [`Runtime::job`]. `Err` is what to tell the person instead.
    pub fn start_run(
        &self,
        agent: &str,
        text: &str,
        why: &str,
        project: Option<&str>,
        attach: &[String],
    ) -> Result<String, String> {
        let body = serde_json::json!({
            "input": text,
            "why": why,
            "project": project.unwrap_or_default(),
            "attachments": attach,
            "wait": false,
        })
        .to_string();
        let r = self
            .req(
                "POST",
                &format!("/v1/agents/{agent}/runs"),
                Some("application/json"),
                Some(body.as_bytes()),
            )
            .map_err(|e| format!("could not reach the agent runtime: {e}"))?;
        if !r.ok() {
            return Err(said(&r));
        }
        r.json()["job"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| format!("the agent runtime answered oddly: {}", r.text()))
    }

    pub fn job(&self, job: &str) -> Job {
        self.read_job(&format!("/v1/jobs/{job}"), |run| {
            (run["status"] == "ok", run["answer"].as_str().unwrap_or_default().into())
        })
    }

    fn read_job(&self, path: &str, done: impl Fn(&Value) -> (bool, String)) -> Job {
        match self.req("GET", path, None, None) {
            Err(_) => Job::Running,
            Ok(r) if r.status == 202 => Job::Running,
            Ok(r) if r.ok() => {
                let (ok, text) = done(&r.json());
                Job::Done(ok, text)
            }
            Ok(r) if r.status == 404 => {
                Job::Done(false, "the agent runtime restarted and lost this; send it again".into())
            }
            Ok(r) => Job::Done(false, said(&r)),
        }
    }
}

/// An unreachable runtime is `Running`: the next pass asks again.
#[derive(Debug, PartialEq)]
pub enum Job {
    Running,
    /// (ok, the answer or why not)
    Done(bool, String),
}

/// What a refusal says: a `/v1` problem's `detail`, or the text of an older route.
fn said(r: &Response) -> String {
    let text = r.text();
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["detail"].as_str().map(String::from))
        .unwrap_or(text)
}

/// Queries runs with increasing limit starting at 20, doubling up to 1000,
/// until either all runs are fetched or the oldest run in the batch finished
/// before or at `after_ms`.
pub fn paginate_since<F>(after_ms: u64, mut fetch: F) -> Result<Vec<RunInfo>, String>
where
    F: FnMut(usize) -> Result<Vec<RunInfo>, String>,
{
    let mut limit = 20;
    loop {
        let runs = fetch(limit)?;
        let exhausted = runs.len() < limit;
        let reached_cutoff = runs.last().is_none_or(|r| r.finished_at_ms() <= after_ms);
        if exhausted || reached_cutoff || limit >= 1000 {
            return Ok(runs);
        }
        limit = (limit * 2).min(1000);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_run(id: &str, finished_ms: u64) -> RunInfo {
        RunInfo {
            id: id.into(),
            trigger: "schedule: @daily".into(),
            status: "ok".into(),
            answer: "done".into(),
            finished: finished_ms / 1000,
            finished_ms,
            project: "".into(),
            input: "".into(),
        }
    }

    #[test]
    fn paginate_since_stops_when_under_limit_or_old_enough() {
        // Case 1: fewer than 20 runs, all after cutoff
        let runs_10: Vec<RunInfo> =
            (0..10).map(|i| fake_run(&format!("r{i}"), 20_000 - i * 100)).collect();
        let mut calls = 0;
        let got = paginate_since(10_000, |limit| {
            calls += 1;
            assert_eq!(limit, 20);
            Ok(runs_10.clone())
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(got.len(), 10);

        // Case 2: exactly 20 runs, oldest is before cutoff (finished_ms = 1_000 <= 10_000)
        let runs_20: Vec<RunInfo> =
            (0..20).map(|i| fake_run(&format!("r{i}"), 20_000 - i * 1_000)).collect();
        calls = 0;
        let got = paginate_since(10_000, |limit| {
            calls += 1;
            assert_eq!(limit, 20);
            Ok(runs_20.clone())
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(got.len(), 20);

        // Case 3: 50 runs all newer than cutoff (10_000). First fetch of 20 has oldest 18_100 (> 10_000),
        // so it doubles to 40 (oldest 16_100 > 10_000), then doubles to 80 (returns all 50, len < 80).
        let all_runs: Vec<RunInfo> =
            (0..50).map(|i| fake_run(&format!("r{i}"), 20_000 - i * 100)).collect();
        let mut limits_requested = Vec::new();
        let got = paginate_since(10_000, |limit| {
            limits_requested.push(limit);
            let n = limit.min(all_runs.len());
            Ok(all_runs[..n].to_vec())
        })
        .unwrap();
        assert_eq!(limits_requested, vec![20, 40, 80]);
        assert_eq!(got.len(), 50);
    }
}
