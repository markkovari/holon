//! One model turn, streamed: text arrives through `on_text` as it is
//! generated, tool calls and usage come back when the turn ends.
//!
//! Two wire dialects, because those are the two this repository's models
//! speak (see `goalrun --provider`): Anthropic's `/v1/messages` and the
//! OpenAI-compatible `/v1/chat/completions` that vLLM, llama.cpp, Ollama and
//! mlx_lm serve. `Mock` is scripted and free, for tests and for building a
//! client before paying for a model.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

/// One entry of a conversation, provider-neutral.
#[derive(Clone, Debug)]
pub enum Msg {
    User(String),
    Assistant { text: String, calls: Vec<Call> },
    ToolResults(Vec<ToolResult>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Clone, Debug)]
pub struct ToolResult {
    pub call_id: String,
    pub output: String,
    pub is_error: bool,
}

/// Token counts of one turn. Pricing is `cost.rs`'s job, not the provider's.
#[derive(Default, Debug, PartialEq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Default, Debug)]
pub struct Turn {
    pub text: String,
    pub calls: Vec<Call>,
    pub tokens: Tokens,
}

pub enum Provider {
    Anthropic { base: String, key: String },
    OpenAi { base: String, key: String },
    Mock,
}

pub const SYSTEM: &str = "You are an agent working in a workspace directory. Use the tools to \
inspect and change it. Paths are relative to the workspace root. When the task is done, reply \
with a short summary and no tool calls.";

const MAX_TOKENS: u32 = 8192;

impl Provider {
    pub async fn turn(
        &self,
        http: &reqwest::Client,
        model: &str,
        msgs: &[Msg],
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<Turn> {
        match self {
            Provider::Mock => Ok(mock(msgs, on_text)),
            Provider::Anthropic { base, key } => {
                let req = http
                    .post(format!("{}/v1/messages", base.trim_end_matches('/')))
                    .header("x-api-key", key)
                    .header("anthropic-version", "2023-06-01")
                    .json(&anthropic_body(model, msgs));
                let mut acc = AnthropicAcc::default();
                stream(req, |data| acc.feed(data, on_text)).await?;
                Ok(acc.finish())
            }
            Provider::OpenAi { base, key } => {
                let mut req = http
                    .post(format!("{}/chat/completions", base.trim_end_matches('/')))
                    .json(&openai_body(model, msgs));
                if !key.is_empty() {
                    req = req.bearer_auth(key);
                }
                let mut acc = OpenAiAcc::default();
                stream(req, |data| acc.feed(data, on_text)).await?;
                acc.finish()
            }
        }
    }
}

/// Send `req` and hand each SSE `data:` payload to `feed`, as it arrives.
async fn stream(
    req: reqwest::RequestBuilder,
    mut feed: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    let mut resp = req.send().await.context("model unreachable")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("model answered {status}: {}", body.chars().take(500).collect::<String>());
    }
    let mut buf = String::new();
    while let Some(chunk) = resp.chunk().await.context("model stream broke")? {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        for data in take_frames(&mut buf) {
            feed(&data)?;
        }
    }
    Ok(())
}

/// Remove every complete SSE frame from `buf` and return their `data`.
/// A frame split across chunks stays in `buf` until its blank line arrives.
pub fn take_frames(buf: &mut String) -> Vec<String> {
    if buf.contains('\r') {
        *buf = buf.replace("\r\n", "\n");
    }
    let mut out = Vec::new();
    while let Some(end) = buf.find("\n\n") {
        let frame: String = buf.drain(..end + 2).collect();
        let data: Vec<&str> = frame
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|l| l.strip_prefix(' ').unwrap_or(l))
            .collect();
        if !data.is_empty() {
            out.push(data.join("\n"));
        }
    }
    out
}

// ---- Anthropic ----

fn anthropic_body(model: &str, msgs: &[Msg]) -> Value {
    let messages: Vec<Value> = msgs
        .iter()
        .map(|m| match m {
            Msg::User(t) => json!({"role": "user", "content": t}),
            Msg::Assistant { text, calls } => {
                let mut content = Vec::new();
                if !text.is_empty() {
                    content.push(json!({"type": "text", "text": text}));
                }
                for c in calls {
                    content.push(
                        json!({"type": "tool_use", "id": c.id, "name": c.name, "input": c.input}),
                    );
                }
                json!({"role": "assistant", "content": content})
            }
            Msg::ToolResults(rs) => json!({"role": "user", "content": rs.iter().map(|r| json!({
                "type": "tool_result", "tool_use_id": r.call_id, "content": r.output, "is_error": r.is_error
            })).collect::<Vec<_>>()}),
        })
        .collect();
    let tools: Vec<Value> = super::tools::specs()
        .into_iter()
        .map(|(n, d, s)| json!({"name": n, "description": d, "input_schema": s}))
        .collect();
    json!({"model": model, "max_tokens": MAX_TOKENS, "system": SYSTEM, "messages": messages,
           "tools": tools, "stream": true})
}

#[derive(Default)]
struct AnthropicAcc {
    turn: Turn,
    /// The tool_use block being streamed: (id, name, partial JSON).
    open: Option<(String, String, String)>,
}

impl AnthropicAcc {
    fn feed(&mut self, data: &str, on_text: &mut (dyn FnMut(&str) + Send)) -> Result<()> {
        let v: Value = serde_json::from_str(data).context("bad model event")?;
        let n = |p: &str| v.pointer(p).and_then(Value::as_u64).unwrap_or(0);
        match v["type"].as_str().unwrap_or_default() {
            "message_start" => {
                let t = &mut self.turn.tokens;
                t.input = n("/message/usage/input_tokens");
                t.cache_read = n("/message/usage/cache_read_input_tokens");
                t.cache_write = n("/message/usage/cache_creation_input_tokens");
                t.output = n("/message/usage/output_tokens");
            }
            "content_block_start" if v["content_block"]["type"] == "tool_use" => {
                let b = &v["content_block"];
                self.open = Some((str_of(&b["id"]), str_of(&b["name"]), String::new()));
            }
            "content_block_delta" => match v["delta"]["type"].as_str().unwrap_or_default() {
                "text_delta" => {
                    let t = str_of(&v["delta"]["text"]);
                    on_text(&t);
                    self.turn.text.push_str(&t);
                }
                "input_json_delta" => {
                    if let Some((_, _, json)) = &mut self.open {
                        json.push_str(v["delta"]["partial_json"].as_str().unwrap_or_default());
                    }
                }
                _ => {}
            },
            "content_block_stop" => {
                if let Some((id, name, json)) = self.open.take() {
                    self.turn.calls.push(Call { id, name, input: parse_args(&json) });
                }
            }
            // Cumulative, so it replaces rather than adds.
            "message_delta" => self.turn.tokens.output = n("/usage/output_tokens"),
            "error" => bail!("model error: {}", v["error"]),
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Turn {
        self.turn
    }
}

// ---- OpenAI-compatible ----

fn openai_body(model: &str, msgs: &[Msg]) -> Value {
    let mut messages = vec![json!({"role": "system", "content": SYSTEM})];
    for m in msgs {
        match m {
            Msg::User(t) => messages.push(json!({"role": "user", "content": t})),
            Msg::Assistant { text, calls } => {
                let mut a = json!({"role": "assistant", "content": text});
                if !calls.is_empty() {
                    a["tool_calls"] = calls
                        .iter()
                        .map(|c| {
                            json!({"id": c.id, "type": "function",
                        "function": {"name": c.name, "arguments": c.input.to_string()}})
                        })
                        .collect();
                }
                messages.push(a);
            }
            Msg::ToolResults(rs) => {
                for r in rs {
                    let content =
                        if r.is_error { format!("ERROR: {}", r.output) } else { r.output.clone() };
                    messages.push(
                        json!({"role": "tool", "tool_call_id": r.call_id, "content": content}),
                    );
                }
            }
        }
    }
    let tools: Vec<Value> = super::tools::specs()
        .into_iter()
        .map(|(n, d, s)| {
            json!({"type": "function", "function": {"name": n, "description": d, "parameters": s}})
        })
        .collect();
    json!({"model": model, "max_tokens": MAX_TOKENS, "messages": messages, "tools": tools,
           "stream": true, "stream_options": {"include_usage": true}})
}

#[derive(Default)]
struct OpenAiAcc {
    turn: Turn,
    /// Tool calls by stream index: (id, name, argument JSON so far).
    calls: Vec<(String, String, String)>,
}

impl OpenAiAcc {
    fn feed(&mut self, data: &str, on_text: &mut (dyn FnMut(&str) + Send)) -> Result<()> {
        if data.trim() == "[DONE]" {
            return Ok(());
        }
        let v: Value = serde_json::from_str(data).context("bad model chunk")?;
        if let Some(e) = v.get("error") {
            bail!("model error: {e}");
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            let n = |p: &str| u.pointer(p).and_then(Value::as_u64).unwrap_or(0);
            // Cached prompt tokens are a subset of prompt_tokens here, priced apart.
            let cached = n("/prompt_tokens_details/cached_tokens");
            self.turn.tokens = Tokens {
                input: n("/prompt_tokens").saturating_sub(cached),
                output: n("/completion_tokens"),
                cache_read: cached,
                cache_write: 0,
            };
        }
        let delta = &v["choices"][0]["delta"];
        if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            on_text(t);
            self.turn.text.push_str(t);
        }
        for tc in delta["tool_calls"].as_array().into_iter().flatten() {
            let i = tc["index"].as_u64().unwrap_or(0) as usize;
            if self.calls.len() <= i {
                self.calls.resize(i + 1, Default::default());
            }
            let slot = &mut self.calls[i];
            if let Some(id) = tc["id"].as_str() {
                slot.0 = id.to_string();
            }
            if let Some(name) = tc["function"]["name"].as_str() {
                slot.1.push_str(name);
            }
            slot.2.push_str(tc["function"]["arguments"].as_str().unwrap_or_default());
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Turn> {
        for (i, (id, name, args)) in self.calls.into_iter().enumerate() {
            if name.is_empty() {
                bail!("model sent a tool call with no name");
            }
            let id = if id.is_empty() { format!("call_{i}") } else { id };
            self.turn.calls.push(Call { id, name, input: parse_args(&args) });
        }
        Ok(self.turn)
    }
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// Tool arguments as the model sent them. Unparseable JSON is kept, as a
/// string under `_raw`, so the tool fails on it visibly instead of silently.
fn parse_args(json: &str) -> Value {
    if json.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(json).unwrap_or_else(|_| json!({ "_raw": json }))
}

// ---- Mock ----

/// Lists the workspace, then answers with what it found. Deterministic, so a
/// test (or a bot author) sees the whole event vocabulary without a model.
fn mock(msgs: &[Msg], on_text: &mut (dyn FnMut(&str) + Send)) -> Turn {
    let tokens = Tokens { input: 1000, output: 50, cache_read: 0, cache_write: 0 };
    let say = |on_text: &mut (dyn FnMut(&str) + Send), parts: &[&str]| {
        parts.iter().for_each(|p| on_text(p));
        parts.concat()
    };
    match msgs.last() {
        Some(Msg::ToolResults(rs)) => {
            let listing = rs.first().map(|r| r.output.replace('\n', ", ")).unwrap_or_default();
            let text = say(on_text, &["The workspace holds: ", &listing, "."]);
            Turn { text, calls: vec![], tokens }
        }
        _ => {
            let text = say(on_text, &["Let me look ", "at the workspace."]);
            let calls = vec![Call {
                id: super::wire::new_id("call"),
                name: "list_dir".into(),
                input: json!({"path": "."}),
            }];
            Turn { text, calls, tokens }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(buf: &mut String, chunk: &str) -> Vec<String> {
        buf.push_str(chunk);
        take_frames(buf)
    }

    #[test]
    fn a_frame_split_across_chunks_waits_for_its_blank_line() {
        let mut buf = String::new();
        assert!(collect(&mut buf, "event: x\ndata: {\"a\"").is_empty());
        assert_eq!(collect(&mut buf, ":1}\n\ndata: [DONE]\r\n\r\n"), vec!["{\"a\":1}", "[DONE]"]);
        assert!(buf.is_empty());
    }

    #[test]
    fn anthropic_stream_yields_text_a_tool_call_and_tokens() {
        let frames = [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":120,"cache_read_input_tokens":30,"cache_creation_input_tokens":5,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu_1","name":"read_file","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"a.txt\"}"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}"#,
            r#"{"type":"message_stop"}"#,
        ];
        let mut acc = AnthropicAcc::default();
        let mut seen = String::new();
        for f in frames {
            acc.feed(f, &mut |t| seen.push_str(t)).unwrap();
        }
        let t = acc.finish();
        assert_eq!((seen.as_str(), t.text.as_str()), ("Hello", "Hello"));
        assert_eq!(
            t.calls,
            vec![Call {
                id: "tu_1".into(),
                name: "read_file".into(),
                input: json!({"path": "a.txt"})
            }]
        );
        assert_eq!(t.tokens, Tokens { input: 120, output: 42, cache_read: 30, cache_write: 5 });
    }

    #[test]
    fn openai_stream_assembles_tool_arguments_by_index() {
        let frames = [
            r#"{"choices":[{"delta":{"role":"assistant","content":"On it"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"run","arguments":"{\"comm"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"and\":\"ls\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":7,"prompt_tokens_details":{"cached_tokens":40}}}"#,
            "[DONE]",
        ];
        let mut acc = OpenAiAcc::default();
        let mut seen = String::new();
        for f in frames {
            acc.feed(f, &mut |t| seen.push_str(t)).unwrap();
        }
        let t = acc.finish().unwrap();
        assert_eq!(seen, "On it");
        assert_eq!(
            t.calls,
            vec![Call { id: "c1".into(), name: "run".into(), input: json!({"command": "ls"}) }]
        );
        assert_eq!(t.tokens, Tokens { input: 60, output: 7, cache_read: 40, cache_write: 0 });
    }

    #[test]
    fn a_model_error_event_fails_the_turn() {
        let mut acc = AnthropicAcc::default();
        let e = acc.feed(r#"{"type":"error","error":{"type":"overloaded_error"}}"#, &mut |_| {});
        assert!(e.is_err());
    }
}
