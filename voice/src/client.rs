use std::sync::{Arc, Mutex};
use std::time::Duration;

pub trait AgentDispatcher: Send + Sync {
    fn dispatch_run(&self, prompt: &str) -> Result<String, String>;
}

pub struct RemoteAgentClient {
    base_url: String,
    agent: String,
    token: Option<String>,
    client: reqwest::blocking::Client,
}

impl RemoteAgentClient {
    pub fn new(
        base_url: impl Into<String>,
        agent: impl Into<String>,
        token: Option<String>,
    ) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Some(Duration::from_secs(60)))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());

        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            agent: agent.into(),
            token,
            client,
        }
    }
}

impl AgentDispatcher for RemoteAgentClient {
    fn dispatch_run(&self, prompt: &str) -> Result<String, String> {
        let endpoint = format!("{}/v1/agents/{}/runs", self.base_url, self.agent);
        let payload = serde_json::json!({
            "input": prompt,
            "why": "voice",
            "wait": true
        });

        let mut req = self.client.post(&endpoint).json(&payload);
        if let Some(tok) = &self.token {
            req = req.bearer_auth(tok);
        }

        let resp = req
            .send()
            .map_err(|e| format!("failed to connect to holon runtime at {}: {e}", self.base_url))?;

        let status = resp.status();
        let body: serde_json::Value =
            resp.json().map_err(|e| format!("invalid json from {endpoint} ({status}): {e}"))?;

        if !status.is_success() {
            let detail = body["detail"].as_str().unwrap_or("unknown error");
            return Err(format!("agent run failed ({status}): {detail}"));
        }

        let answer = body["answer"]
            .as_str()
            .or_else(|| body["output"].as_str())
            .unwrap_or_default()
            .trim()
            .to_string();

        Ok(answer)
    }
}

pub struct MockDispatcher {
    responses: Arc<Mutex<Vec<String>>>,
    dispatched_prompts: Arc<Mutex<Vec<String>>>,
}

impl MockDispatcher {
    pub fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            dispatched_prompts: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn dispatched(&self) -> Vec<String> {
        self.dispatched_prompts.lock().unwrap().clone()
    }
}

impl AgentDispatcher for MockDispatcher {
    fn dispatch_run(&self, prompt: &str) -> Result<String, String> {
        self.dispatched_prompts.lock().unwrap().push(prompt.to_string());
        let mut list = self.responses.lock().unwrap();
        if list.is_empty() {
            Ok(format!("Echo: {prompt}"))
        } else {
            Ok(list.remove(0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_dispatcher_records_and_answers() {
        let dispatcher = MockDispatcher::new(vec!["Server online".into()]);
        let reply = dispatcher.dispatch_run("status").unwrap();
        assert_eq!(reply, "Server online");
        assert_eq!(dispatcher.dispatched(), vec!["status"]);

        let echo = dispatcher.dispatch_run("second query").unwrap();
        assert_eq!(echo, "Echo: second query");
        assert_eq!(dispatcher.dispatched(), vec!["status", "second query"]);
    }
}
