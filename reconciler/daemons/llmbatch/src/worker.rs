//! Core processing logic for NATS JetStream LLM batch coalescing and realtime routing.

use crate::providers::{self, Health, Kind, Provider};
use anyhow::{anyhow, Result};
use async_nats::jetstream::{self, consumer::PullConsumer};
use async_nats::Client;
use futures::StreamExt;
use holon_llm::coalesce::{self, BatchTask, TaskOutput};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

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
    /// Asked in order; see `providers.json`.
    pub providers: Vec<Provider>,
    health: Mutex<HashMap<String, Health>>,
    models: Mutex<HashMap<String, Vec<String>>>,
}

/// How many of one provider's models a request may try before moving on.
const MODELS_PER_PROVIDER: usize = 3;

/// A provider's own entry carries no request limits: those are per model.
fn unlimited(p: &Provider) -> Provider {
    Provider { rpm: 0, rpd: 0, ..p.clone() }
}

/// A provider's refusal, as it said it.
struct Failure {
    status: Option<u16>,
    retry_after: Option<String>,
    body: String,
}

fn secs_until(t: SystemTime, now: SystemTime) -> u64 {
    t.duration_since(now).unwrap_or_default().as_secs()
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

/// An answer with no text is a failure the caller should see, not a blank reply.
fn non_empty(reply: holon_llm::Reply) -> Result<String> {
    if reply.text.trim().is_empty() {
        return Err(anyhow!(
            "upstream answered no text (finish_reason {}, {} tool call(s))",
            reply.finish.as_deref().unwrap_or("none"),
            reply.calls.len()
        ));
    }
    Ok(reply.text)
}

/// The key goes in a header, never the URL: a failed request's error names its URL, and that
/// error travels back to the caller (and into an agent's run log).
fn gemini_request(
    http: &reqwest::Client,
    url: &str,
    key: Option<&str>,
    body: &serde_json::Value,
) -> reqwest::RequestBuilder {
    let rb = http.post(url).json(body);
    match key {
        Some(k) => rb.header("x-goog-api-key", k),
        None => rb,
    }
}

impl BatchWorker {
    pub fn new(
        nats: Client,
        js: jetstream::Context,
        config: WorkerConfig,
        kv: Option<async_nats::jetstream::kv::Store>,
        providers: Vec<Provider>,
    ) -> Self {
        // Thinking models take over a minute on an agent's long transcript; callers wait 180 s.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(170))
            .build()
            .unwrap_or_default();
        Self {
            nats,
            js,
            http,
            config,
            kv,
            providers,
            health: Mutex::default(),
            models: Mutex::default(),
        }
    }

    /// A provider's key: `<NAME>_API_KEY`, then `<name>-api-key` in the SECRETS bucket, then
    /// `~/.comp-secrets/<name>.key`. `--api-key` only for a single `--upstream-url`.
    pub async fn resolve_api_key(&self, provider: &str) -> Option<String> {
        let clean = |s: String| Some(s.trim().to_string()).filter(|s| !s.is_empty());
        if self.providers.len() == 1 {
            if let Some(k) = self.config.api_key.clone().and_then(clean) {
                return Some(k);
            }
        }
        let upper = provider.to_uppercase().replace('-', "_");
        for var in [format!("{upper}_API_KEY"), format!("HOLON_{upper}_API_KEY")] {
            if let Some(k) = std::env::var(var).ok().and_then(clean) {
                return Some(k);
            }
        }
        if let Some(ref kv) = self.kv {
            for key in [
                format!("{provider}-api-key"),
                format!("{provider}_api_key"),
                format!("{upper}_API_KEY"),
            ] {
                if let Ok(Some(bytes)) = kv.get(&key).await {
                    if let Some(k) = clean(String::from_utf8_lossy(&bytes).into_owned()) {
                        return Some(k);
                    }
                }
            }
        }
        let home = std::env::var("HOME").ok()?;
        let dir = std::path::Path::new(&home).join(".comp-secrets");
        [format!("{provider}.key"), format!("{provider}-api-key")]
            .iter()
            .find_map(|f| std::fs::read_to_string(dir.join(f)).ok().and_then(clean))
    }

    /// `name` is a provider (for what stops it as a whole) or `provider/model`.
    fn health<R>(&self, name: &str, f: impl FnOnce(&mut Health) -> R) -> R {
        f(self.health.lock().unwrap().entry(name.to_string()).or_default())
    }

    /// Asks the free providers' models in order, skipping any cooling down, at its documented
    /// limits or without a key. A failure sets when that model (or, for a refused key, that
    /// provider) may be asked again; the next one is tried at once.
    pub async fn execute_upstream(&self, system: &str, user_prompt: &str) -> Result<String> {
        let mut passed = Vec::new();
        for p in &self.providers {
            let now = SystemTime::now();
            if let Some((back, why)) = self.health(&p.name, |h| h.blocked(&unlimited(p), now)) {
                passed.push(format!("{} {why}, back in {}s", p.name, secs_until(back, now)));
                continue;
            }
            let Some(key) = self.resolve_api_key(&p.name).await else {
                passed.push(format!("{} has no key", p.name));
                continue;
            };
            let models = match self.models_for(p, &key).await {
                Ok(m) => m,
                Err(why) => {
                    passed.push(format!("{} {why}", p.name));
                    continue;
                }
            };
            for model in models.iter().take(MODELS_PER_PROVIDER) {
                let slot = format!("{}/{model}", p.name);
                let now = SystemTime::now();
                if let Some((back, why)) = self.health(&slot, |h| h.blocked(p, now)) {
                    passed.push(format!("{slot} {why}, back in {}s", secs_until(back, now)));
                    continue;
                }
                self.health(&slot, |h| h.spent(p, now));
                let f = match self.ask(p, model, &key, system, user_prompt).await {
                    Ok(text) => return Ok(text),
                    Err(f) => f,
                };
                if let Some(call) = providers::salvaged_tool_call(&f.body) {
                    return Ok(call);
                }
                let now = SystemTime::now();
                let (back, why) =
                    providers::back_at(p, f.status, f.retry_after.as_deref(), &f.body, now);
                let detail: String = f.body.chars().take(200).collect();
                eprintln!(
                    "holon-llmbatch: {slot} {why}, back in {}s: {detail}",
                    secs_until(back, now)
                );
                passed.push(format!("{slot} {why}"));
                let whole = matches!(f.status, Some(401 | 403));
                self.health(if whole { &p.name } else { &slot }, |h| {
                    h.until = Some(back);
                    h.why = why;
                });
                if f.status == Some(404) {
                    self.models.lock().unwrap().remove(&p.name);
                }
                if whole {
                    break;
                }
            }
        }
        Err(anyhow!("no free provider answered: {}", passed.join("; ")))
    }

    /// The provider's offered models worth asking, best first, listed once and kept.
    async fn models_for(
        &self,
        p: &Provider,
        key: &str,
    ) -> std::result::Result<Vec<String>, String> {
        if let Some(m) = self.models.lock().unwrap().get(&p.name) {
            return Ok(m.clone());
        }
        let url = format!(
            "{}/models{}",
            p.base,
            if p.kind == Kind::Gemini { "?pageSize=1000" } else { "" }
        );
        let rb = self.http.get(url);
        let rb = match p.kind {
            Kind::Gemini => rb.header("x-goog-api-key", key),
            Kind::Openai => rb.bearer_auth(key),
        };
        let fail = |status: Option<u16>, body: &str| {
            let now = SystemTime::now();
            let (back, why) = providers::back_at(p, status, None, body, now);
            let why = format!("cannot list its models ({why})");
            self.health(&p.name, |h| {
                h.until = Some(back.max(now + Duration::from_secs(60)));
                h.why = why.clone();
            });
            why
        };
        let resp = match rb.send().await {
            Ok(r) => r,
            Err(e) => return Err(fail(None, &e.without_url().to_string())),
        };
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(fail(Some(status), &body));
        }
        let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        let offered: Vec<String> = match p.kind {
            Kind::Gemini => v["models"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|m| m["supportedGenerationMethods"].to_string().contains("generateContent"))
                .filter_map(|m| {
                    m["name"].as_str().map(|n| n.trim_start_matches("models/").to_string())
                })
                .collect(),
            Kind::Openai => v["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m["id"].as_str().map(String::from))
                .collect(),
        };
        let wanted = providers::candidates(&p.models, &offered);
        eprintln!(
            "holon-llmbatch: {} will ask {} (of what it offers: {})",
            p.name,
            wanted.join(", "),
            offered.join(", ")
        );
        if wanted.is_empty() {
            let why = "offers none of its wanted models".to_string();
            self.health(&p.name, |h| {
                h.until = Some(SystemTime::now() + Duration::from_secs(3600));
                h.why = why.clone();
            });
            return Err(why);
        }
        self.models.lock().unwrap().insert(p.name.clone(), wanted.clone());
        Ok(wanted)
    }

    async fn ask(
        &self,
        p: &Provider,
        model: &str,
        key: &str,
        system: &str,
        prompt: &str,
    ) -> std::result::Result<String, Failure> {
        let messages = [holon_llm::Message {
            role: holon_llm::Role::User,
            parts: vec![holon_llm::Part::Text(prompt.to_string())],
        }];
        let req = holon_llm::Request {
            model,
            messages: &messages,
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
        let rb = match p.kind {
            Kind::Gemini => gemini_request(
                &self.http,
                &format!("{}/models/{model}:generateContent", p.base),
                Some(key),
                &holon_llm::gemini::body(&req),
            ),
            Kind::Openai => self
                .http
                .post(format!("{}/chat/completions", p.base))
                .bearer_auth(key)
                .json(&holon_llm::openai::body(&req)),
        };
        let resp = rb.send().await.map_err(|e| Failure {
            status: None,
            retry_after: None,
            body: e.without_url().to_string(),
        })?;
        let status = resp.status().as_u16();
        let retry_after =
            resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).map(String::from);
        let bytes = resp.bytes().await.unwrap_or_default();
        let body = String::from_utf8_lossy(&bytes).into_owned();
        if !(200..300).contains(&status) {
            return Err(Failure { status: Some(status), retry_after, body });
        }
        let reply = match p.kind {
            Kind::Gemini => holon_llm::gemini::reply(&bytes).map_err(|e| format!("{e:?}")),
            Kind::Openai => holon_llm::openai::parse(&bytes).map_err(|e| format!("{e:?}")),
        };
        reply
            .map_err(|e| e.to_string())
            .and_then(|r| non_empty(r).map_err(|e| e.to_string()))
            .map_err(|e| Failure { status: Some(status), retry_after: None, body: e })
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
    fn the_key_never_rides_in_the_url_or_an_error() {
        let http = reqwest::Client::new();
        let url = "http://127.0.0.1:1/v1beta/models/m:generateContent";
        let rb = gemini_request(&http, url, Some("SECRET-KEY"), &serde_json::json!({}));
        let req = rb.try_clone().unwrap().build().unwrap();
        assert!(!req.url().as_str().contains("SECRET-KEY"));
        assert_eq!(req.headers()["x-goog-api-key"], "SECRET-KEY");
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let err = rt.block_on(rb.send()).unwrap_err();
        assert!(!format!("{}", err.without_url()).contains("SECRET-KEY"));
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
