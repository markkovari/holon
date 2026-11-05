//! Core processing logic for NATS JetStream LLM batch coalescing and realtime routing.

use anyhow::{anyhow, Result};
use async_nats::jetstream::{self, consumer::PullConsumer};
use async_nats::Client;
use futures::StreamExt;
use holon_llm::coalesce::{self, BatchTask, TaskOutput};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

pub const BATCH_STREAM: &str = "LLM_BATCH";
pub const BATCH_SUBJECT: &str = "holon.llm.batch.>";
pub const REALTIME_STREAM: &str = "LLM_REALTIME";
pub const REALTIME_SUBJECT: &str = "holon.llm.realtime.>";
pub const DLQ_SUBJECT: &str = "holon.llm.batch.dlq";
pub const CONSUMER_NAME: &str = "llmbatch";

#[derive(Clone)]
#[allow(dead_code)]
pub struct WorkerConfig {
    pub nats_url: String,
    pub upstream_url: String,
    pub api_key: Option<String>,
    pub secrets_bucket: String,
    pub model: String,
    pub max_batch_size: usize,
    pub batch_window: Duration,
}

pub struct BatchWorker {
    pub nats: Client,
    #[allow(dead_code)]
    pub js: jetstream::Context,
    pub http: reqwest::Client,
    pub config: WorkerConfig,
    pub kv: Option<async_nats::jetstream::kv::Store>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TaskResultPayload {
    pub id: String,
    pub text: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DlqEntry {
    pub raw_payload: String,
    pub reason: String,
    pub timestamp_ms: u64,
}

impl BatchWorker {
    pub fn new(
        nats: Client,
        js: jetstream::Context,
        config: WorkerConfig,
        kv: Option<async_nats::jetstream::kv::Store>,
    ) -> Self {
        let http =
            reqwest::Client::builder().timeout(Duration::from_secs(60)).build().unwrap_or_default();
        Self { nats, js, http, config, kv }
    }

    /// Dynamically resolves the API key in priority order:
    /// 1. Directly configured key in WorkerConfig
    /// 2. Environment variables (e.g. GEMINI_API_KEY, GROQ_API_KEY, CEREBRAS_API_KEY, OPENAI_API_KEY)
    /// 3. NATS JetStream KV bucket (`SECRETS` or configured bucket)
    /// 4. Local ~/.comp-secrets/<provider>.key file
    pub async fn resolve_api_key(&self, provider: &str) -> Option<String> {
        if let Some(ref k) = self.config.api_key {
            let trimmed = k.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }

        let env_candidates: &[&str] = match provider {
            "gemini" => &["GEMINI_API_KEY", "HOLON_GEMINI_API_KEY"],
            "groq" => &["GROQ_API_KEY", "HOLON_GROQ_API_KEY"],
            "cerebras" => &["CEREBRAS_API_KEY", "HOLON_CEREBRAS_API_KEY"],
            _ => &["OPENAI_API_KEY", "LLM_API_KEY"],
        };

        for env_var in env_candidates {
            if let Ok(val) = std::env::var(env_var) {
                let trimmed = val.trim().to_string();
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            }
        }

        // Check NATS JetStream KV store if available
        if let Some(ref kv) = self.kv {
            let kv_candidates: &[&str] = match provider {
                "gemini" => &["gemini-api-key", "gemini_api_key", "GEMINI_API_KEY", "gemini.key"],
                "groq" => &["groq-api-key", "groq_api_key", "GROQ_API_KEY", "groq.key"],
                "cerebras" => {
                    &["cerebras-api-key", "cerebras_api_key", "CEREBRAS_API_KEY", "cerebras.key"]
                }
                _ => &["openai-api-key", "openai_api_key", "OPENAI_API_KEY", "openai.key"],
            };

            for key in kv_candidates {
                if let Ok(Some(bytes)) = kv.get(*key).await {
                    let val = String::from_utf8_lossy(&bytes).trim().to_string();
                    if !val.is_empty() {
                        return Some(val);
                    }
                }
            }
        }

        // Check local ~/.comp-secrets/<provider>.key
        if let Ok(home) = std::env::var("HOME") {
            let p = std::path::Path::new(&home).join(".comp-secrets");
            for file_name in [
                format!("{provider}.key"),
                format!("{provider}-api-key"),
                format!("{provider}.secret"),
            ] {
                if let Ok(content) = std::fs::read_to_string(p.join(file_name)) {
                    let trimmed = content.trim().to_string();
                    if !trimmed.is_empty() {
                        return Some(trimmed);
                    }
                }
            }
        }

        None
    }

    /// Execute prompt upstream against Gemini REST or OpenAI-compatible endpoint.
    pub async fn execute_upstream(&self, system: &str, user_prompt: &str) -> Result<String> {
        let is_gemini = self.config.upstream_url.contains("googleapis.com")
            || self.config.upstream_url.ends_with(":generateContent")
            || self.config.model.starts_with("gemini");
        let provider_tag = if is_gemini {
            "gemini"
        } else if self.config.upstream_url.contains("groq.com") {
            "groq"
        } else if self.config.upstream_url.contains("cerebras.ai") {
            "cerebras"
        } else {
            "openai"
        };

        let resolved_key = self.resolve_api_key(provider_tag).await;

        if is_gemini {
            let req = holon_llm::Request {
                model: &self.config.model,
                messages: &[holon_llm::Message {
                    role: holon_llm::Role::User,
                    parts: vec![holon_llm::Part::Text(user_prompt.to_string())],
                }],
                system: if system.is_empty() { None } else { Some(system) },
                tools: &[],
                max_tokens: Some(8192),
                temperature: Some(0.1),
                stop: &[],
                seed: None,
                stream: false,
                cache: false,
                blocks: false,
            };
            let body = holon_llm::gemini::body(&req);
            let mut url = self.config.upstream_url.clone();
            if let Some(ref key) = resolved_key {
                let sep = if url.contains('?') { "&" } else { "?" };
                url = format!("{url}{sep}key={key}");
            }
            let resp = self.http.post(&url).json(&body).send().await?;
            let status = resp.status();
            let bytes = resp.bytes().await?;
            if !status.is_success() {
                return Err(anyhow!(
                    "upstream gemini returned status {status}: {}",
                    String::from_utf8_lossy(&bytes)
                ));
            }
            let reply = holon_llm::gemini::reply(&bytes).map_err(|e| anyhow!("{e:?}"))?;
            Ok(reply.text)
        } else {
            // OpenAI-compatible chat completions endpoint (e.g. llm-router or Ollama)
            let req = holon_llm::Request {
                model: &self.config.model,
                messages: &[holon_llm::Message {
                    role: holon_llm::Role::User,
                    parts: vec![holon_llm::Part::Text(user_prompt.to_string())],
                }],
                system: if system.is_empty() { None } else { Some(system) },
                tools: &[],
                max_tokens: Some(8192),
                temperature: Some(0.1),
                stop: &[],
                seed: None,
                stream: false,
                cache: false,
                blocks: false,
            };
            let body = holon_llm::openai::body(&req);
            let mut rb = self.http.post(&self.config.upstream_url).json(&body);
            if let Some(ref key) = resolved_key {
                rb = rb.header("Authorization", format!("Bearer {key}"));
            }
            let resp = rb.send().await?;
            let status = resp.status();
            let bytes = resp.bytes().await?;
            if !status.is_success() {
                return Err(anyhow!(
                    "upstream openai returned status {status}: {}",
                    String::from_utf8_lossy(&bytes)
                ));
            }
            let reply = holon_llm::openai::parse(&bytes).map_err(|e| anyhow!("{e:?}"))?;
            Ok(reply.text)
        }
    }

    /// Process a coalesced batch of tasks.
    pub async fn process_coalesced_batch(&self, tasks: Vec<BatchTask>) -> Result<Vec<TaskOutput>> {
        if tasks.is_empty() {
            return Ok(Vec::new());
        }
        eprintln!(
            "holon-llmbatch: coalescing {} tasks into 1 upstream request (window: {:?})",
            tasks.len(),
            self.config.batch_window
        );
        let coalesced = coalesce::build_coalesced_prompt(&tasks);
        let sys = coalesce::system_prompt();
        let raw_resp = self.execute_upstream(sys, &coalesced).await?;
        let parsed = coalesce::parse_coalesced_response(&raw_resp).map_err(|e| anyhow!("{e}"))?;
        eprintln!("holon-llmbatch: successfully unpacked {} task results", parsed.len());
        Ok(parsed)
    }

    /// Distribute task answers to their reply destinations.
    pub async fn distribute_results(&self, outputs: &[TaskOutput], tasks: &[BatchTask]) {
        for out in outputs {
            let task = tasks.iter().find(|t| t.id == out.id);
            let payload = serde_json::to_vec(&TaskResultPayload {
                id: out.id.clone(),
                text: out.text.clone(),
                ok: true,
                error: None,
            })
            .unwrap_or_default();

            // Direct reply subject if specified in task
            if let Some(t) = task {
                if let Some(ref reply_sub) = t.reply_subject {
                    let _ = self.nats.publish(reply_sub.clone(), payload.clone().into()).await;
                }
            }

            // Universal reply subject
            let canonical = format!("holon.llm.reply.{}", out.id);
            let _ = self.nats.publish(canonical, payload.into()).await;
        }
    }

    /// Move unprocessable message to DLQ
    pub async fn route_to_dlq(&self, payload: &[u8], reason: &str) {
        let entry = DlqEntry {
            raw_payload: String::from_utf8_lossy(payload).to_string(),
            reason: reason.to_string(),
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        };
        if let Ok(bytes) = serde_json::to_vec(&entry) {
            let _ = self.nats.publish(DLQ_SUBJECT, bytes.into()).await;
        }
    }

    /// Realtime worker task: processes immediate prompts without batching.
    pub async fn run_realtime_worker(self: Arc<Self>) -> Result<()> {
        let mut sub = self.nats.subscribe(REALTIME_SUBJECT).await?;
        while let Some(msg) = sub.next().await {
            let this = self.clone();
            tokio::spawn(async move {
                let task_res: Result<BatchTask, _> = serde_json::from_slice(&msg.payload);
                let (task_id, prompt, system, reply_subj) = match task_res {
                    Ok(t) => (t.id, t.prompt, t.system_prompt.unwrap_or_default(), t.reply_subject),
                    Err(_) => {
                        // Plain string prompt fallback
                        let p = String::from_utf8_lossy(&msg.payload).to_string();
                        ("rt-unknown".into(), p, String::new(), None)
                    }
                };

                let result = this.execute_upstream(&system, &prompt).await;
                let payload = match result {
                    Ok(text) => {
                        TaskResultPayload { id: task_id.clone(), text, ok: true, error: None }
                    }
                    Err(e) => TaskResultPayload {
                        id: task_id.clone(),
                        text: String::new(),
                        ok: false,
                        error: Some(e.to_string()),
                    },
                };
                let bytes = serde_json::to_vec(&payload).unwrap_or_default();

                if let Some(reply) = msg.reply {
                    let _ = this.nats.publish(reply, bytes.clone().into()).await;
                }
                if let Some(target) = reply_subj {
                    let _ = this.nats.publish(target, bytes.clone().into()).await;
                }
                let canonical = format!("holon.llm.reply.{task_id}");
                let _ = this.nats.publish(canonical, bytes.into()).await;
            });
        }
        Ok(())
    }

    /// Main batch drain loop: pulls from JetStream WorkQueue and executes multi-task batch.
    pub async fn run_batch_worker(self: Arc<Self>, consumer: PullConsumer) -> Result<()> {
        let max_items = self.config.max_batch_size;
        let window = self.config.batch_window;

        loop {
            let batch = consumer.batch().max_messages(max_items).expires(window).messages().await;
            let mut batch = match batch {
                Ok(b) => b,
                Err(_e) => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            };

            let mut tasks: Vec<BatchTask> = Vec::new();
            let mut msgs = Vec::new();

            while let Some(msg) = batch.next().await {
                let msg = match msg {
                    Ok(m) => m,
                    Err(_) => continue,
                };

                match serde_json::from_slice::<BatchTask>(&msg.payload) {
                    Ok(task) => {
                        tasks.push(task);
                        msgs.push(msg);
                    }
                    Err(e) => {
                        self.route_to_dlq(&msg.payload, &format!("unparseable task: {e}")).await;
                        let _ = msg.ack().await;
                    }
                }
            }

            if tasks.is_empty() {
                continue;
            }

            match self.process_coalesced_batch(tasks.clone()).await {
                Ok(outputs) => {
                    self.distribute_results(&outputs, &tasks).await;
                    for m in msgs {
                        let _ = m.ack().await;
                    }
                }
                Err(e) => {
                    eprintln!("batch execution failed: {e}; retrying");
                    // On failure, negative ack messages so they can be redelivered
                    for m in msgs {
                        let _ = m.ack_with(async_nats::jetstream::AckKind::Nak(None)).await;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn task_serialization_round_trip() {
        let task = BatchTask {
            id: "task-42".into(),
            task_type: Some("summarize".into()),
            prompt: "Summarize this diff".into(),
            system_prompt: Some("You are a concise summarizer".into()),
            max_tokens: Some(512),
            reply_subject: Some("reply.channel.42".into()),
        };

        let json = serde_json::to_string(&task).unwrap();
        let parsed: BatchTask = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, task);
    }

    #[test]
    fn dlq_entry_format() {
        let entry = DlqEntry {
            raw_payload: "invalid json string".into(),
            reason: "malformed JSON syntax".into(),
            timestamp_ms: 1700000000000,
        };
        let bytes = serde_json::to_vec(&entry).unwrap();
        let val: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(val["reason"], "malformed JSON syntax");
        assert_eq!(val["raw_payload"], "invalid json string");
    }
}
