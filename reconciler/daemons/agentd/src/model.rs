//! One model turn, streamed: text arrives through `on_text` as it is
//! generated, tool calls and usage come back when the turn ends.
//!
//! Two wire dialects, because those are the two this repository's models
//! speak (see `goalrun --provider`): Anthropic's `/v1/messages` and the
//! OpenAI-compatible `/v1/chat/completions` that vLLM, llama.cpp, Ollama and
//! mlx_lm serve. What those dialects look like on the wire is `holon-llm`'s;
//! this file is the HTTP, the system prompt and the session's own record of
//! the conversation. `Mock` is scripted and free, for tests and for building
//! a client before paying for a model.

use anyhow::{bail, Context, Result};
use holon_llm::{anthropic, openai, sse, Message, Part, Request, Role, Tool};
pub use holon_llm::{Call, ToolResult, Usage as Tokens};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// One entry of a conversation, provider-neutral. This is the session's record of it, which is
/// persisted; `messages` turns it into what `holon-llm` writes to a wire.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Msg {
    User(String),
    Assistant { text: String, calls: Vec<Call> },
    ToolResults(Vec<ToolResult>),
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
    Gemini { base: String, key: String },
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
                    .header("anthropic-version", anthropic::VERSION)
                    .json(&anthropic::body(&request(model, &messages(msgs), &tools())));
                let mut acc = anthropic::Stream::new();
                stream(req, |data| Ok(acc.feed(data, on_text)?)).await?;
                Ok(turn_of(acc.finish()))
            }
            Provider::OpenAi { base, key } => {
                let mut req = http
                    .post(format!("{}/chat/completions", base.trim_end_matches('/')))
                    .json(&openai::body(&request(model, &messages(msgs), &tools())));
                if !key.is_empty() {
                    req = req.bearer_auth(key);
                }
                let mut acc = openai::Stream::new();
                stream(req, |data| Ok(acc.feed(data, on_text)?)).await?;
                Ok(turn_of(acc.finish()?))
            }
            Provider::Gemini { base, key } => {
                let base = if base.is_empty() {
                    holon_llm::gemini::ENDPOINT
                } else {
                    base.trim_end_matches('/')
                };
                let url = format!("{base}/models/{model}:generateContent?key={key}");
                let ms = messages(msgs);
                let ts = tools();
                let mut req_payload = request(model, &ms, &ts);
                req_payload.stream = false;
                let resp = http
                    .post(url)
                    .json(&holon_llm::gemini::body(&req_payload))
                    .send()
                    .await
                    .context("Gemini unreachable")?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    bail!(
                        "Gemini answered {status}: {}",
                        body.chars().take(500).collect::<String>()
                    );
                }
                let bytes = resp.bytes().await.context("reading Gemini response")?;
                let reply = holon_llm::gemini::reply(&bytes)
                    .map_err(|e| anyhow::anyhow!("Gemini error: {e:?}"))?;
                if !reply.text.is_empty() {
                    on_text(&reply.text);
                }
                Ok(turn_of(reply))
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
        for data in sse::take_frames(&mut buf) {
            feed(&data)?;
        }
    }
    Ok(())
}

/// The request both dialects are written from: the system prompt, this session's tools and a stream.
fn request<'a>(model: &'a str, messages: &'a [Message], tools: &'a [Tool]) -> Request<'a> {
    Request {
        system: Some(SYSTEM),
        tools,
        max_tokens: Some(MAX_TOKENS),
        stream: true,
        ..Request::new(model, messages)
    }
}

fn tools() -> Vec<Tool> {
    super::tools::specs()
        .into_iter()
        .map(|(name, description, schema)| Tool {
            name: name.into(),
            description: description.into(),
            schema,
        })
        .collect()
}

/// The session's record of a conversation, as `holon-llm`'s messages: a tool's answers are a user
/// message of their own, which the Anthropic encoder folds into the prompt that follows them.
fn messages(msgs: &[Msg]) -> Vec<Message> {
    msgs.iter()
        .map(|m| match m {
            Msg::User(t) => Message::user(t),
            Msg::Assistant { text, calls } => {
                let mut parts = Vec::new();
                if !text.is_empty() {
                    parts.push(Part::Text(text.clone()));
                }
                parts.extend(calls.iter().cloned().map(Part::ToolUse));
                Message::new(Role::Assistant, parts)
            }
            Msg::ToolResults(rs) => {
                Message::new(Role::User, rs.iter().cloned().map(Part::ToolResult).collect())
            }
        })
        .collect()
}

fn turn_of(r: holon_llm::Reply) -> Turn {
    Turn { text: r.text, calls: r.calls, tokens: r.usage }
}

/// Lists the workspace, then answers with what it found. Deterministic, so a
/// test (or a bot author) sees the whole event vocabulary without a model.
/// A user message that begins `steer:` is what a steered task is handed: it is
/// answered, with no tool call, by repeating it, so a test can see the model read it.
fn mock(msgs: &[Msg], on_text: &mut (dyn FnMut(&str) + Send)) -> Turn {
    let tokens = Tokens { input: 1000, output: 50, cache_read: 0, cache_write: 0 };
    let say = |on_text: &mut (dyn FnMut(&str) + Send), parts: &[&str]| {
        parts.iter().for_each(|p| on_text(p));
        parts.concat()
    };
    match msgs.last() {
        Some(Msg::User(t)) if t.starts_with("steer:") => {
            let text = say(on_text, &["Noted: ", t]);
            Turn { text, calls: vec![], tokens }
        }
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

    #[test]
    fn a_session_record_becomes_the_messages_a_wire_is_written_from() {
        let call = Call { id: "t1".into(), name: "run".into(), input: json!({"command": "ls"}) };
        let m = messages(&[
            Msg::User("a".into()),
            Msg::Assistant { text: String::new(), calls: vec![call.clone()] },
            Msg::ToolResults(vec![ToolResult {
                call_id: "t1".into(),
                output: "x".into(),
                is_error: true,
            }]),
            Msg::Assistant { text: "done".into(), calls: vec![] },
        ]);
        assert_eq!(m[0], Message::user("a"));
        assert_eq!(m[1], Message::new(Role::Assistant, vec![Part::ToolUse(call)]));
        assert!(
            matches!(&m[2].parts[..], [Part::ToolResult(r)] if r.is_error)
                && m[2].role == Role::User
        );
        assert_eq!(m[3], Message::assistant("done"));
    }

    #[test]
    fn both_dialects_are_written_from_the_same_request() {
        let m = messages(&[Msg::User("hi".into())]);
        let tools = tools();
        let r = request("m", &m, &tools);
        let a = anthropic::body(&r);
        let o = openai::body(&r);
        assert_eq!(
            (a["system"].as_str(), a["stream"].as_bool(), a["max_tokens"].as_u64()),
            (Some(SYSTEM), Some(true), Some(8192))
        );
        assert_eq!(o["messages"][0], json!({"role": "system", "content": SYSTEM}));
        assert_eq!((o["stream"].as_bool(), o["max_tokens"].as_u64()), (Some(true), Some(8192)));
        assert_eq!(a["tools"].as_array().map(Vec::len), o["tools"].as_array().map(Vec::len));
        assert!(!tools.is_empty());
    }
}
