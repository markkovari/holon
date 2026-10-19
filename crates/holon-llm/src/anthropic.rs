//! Anthropic's Messages API (`POST /v1/messages`).
//!
//! What differs from the OpenAI dialect, all of it handled here:
//! * the system prompt is a top-level field, not a message;
//! * `max_tokens` is required;
//! * a message's `content` is a string or a list of typed blocks (text, image, tool_use, tool_result),
//!   and roles must alternate, so consecutive user turns — a tool result followed by the next prompt —
//!   are one;
//! * a reply is a list of blocks, with the cached prefix of the prompt counted beside `input_tokens`;
//! * a throttled request can be a `200` whose body is `{"type":"error",…}`.

use serde_json::{json, Value};

use crate::model::{
    parse_args, str_of, u64_at, Call, Message, ParseError, Part, Reply, Request, Tool, Usage,
};

/// The `anthropic-version` header every request carries.
pub const VERSION: &str = "2023-06-01";

/// What a request without `max_tokens` is given, since without one the API answers 400.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// The request body for `/v1/messages`.
///
/// A message that is a single piece of text is `"content": "<text>"`; anything else is a list of
/// blocks, in the order the caller gave the parts.
pub fn body(req: &Request) -> Value {
    let messages = merge_user_turns(req.messages.iter().map(message).collect());
    let mut v = json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "messages": messages,
    });
    if let Some(system) = req.system {
        // The system prompt is identical across every call, so with caching on it is its own
        // breakpoint; the growing tail is left to the automatic one below.
        v["system"] = if req.cache {
            json!([{"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}])
        } else {
            json!(system)
        };
    }
    if req.cache {
        v["cache_control"] = json!({"type": "ephemeral"});
    }
    if !req.tools.is_empty() {
        v["tools"] = req.tools.iter().map(tool).collect();
    }
    if let Some(t) = req.temperature {
        v["temperature"] = json!(t);
    }
    if !req.stop.is_empty() {
        v["stop_sequences"] = json!(req.stop);
    }
    if req.stream {
        v["stream"] = json!(true);
    }
    v
}

fn tool(t: &Tool) -> Value {
    json!({"name": t.name, "description": t.description, "input_schema": t.schema})
}

fn message(m: &Message) -> Value {
    let role = m.role.as_str();
    if let [Part::Text(t)] = m.parts.as_slice() {
        return json!({"role": role, "content": t});
    }
    json!({"role": role, "content": m.parts.iter().map(block).collect::<Vec<_>>()})
}

fn block(p: &Part) -> Value {
    match p {
        Part::Text(t) => json!({"type": "text", "text": t}),
        Part::Image(i) => json!({"type": "image", "source": {
            "type": "base64", "media_type": i.media_type, "data": i.base64()}}),
        Part::ToolUse(c) => {
            json!({"type": "tool_use", "id": c.id, "name": c.name, "input": c.input})
        }
        Part::ToolResult(r) => json!({
            "type": "tool_result", "tool_use_id": r.call_id, "content": r.output, "is_error": r.is_error}),
    }
}

/// Anthropic wants roles to alternate; tool results followed by the next prompt are two user turns in
/// a row, so they become one.
fn merge_user_turns(msgs: Vec<Value>) -> Vec<Value> {
    let blocks = |c: &Value| match c {
        Value::String(t) => vec![json!({"type": "text", "text": t})],
        Value::Array(a) => a.clone(),
        _ => vec![],
    };
    let mut out: Vec<Value> = Vec::new();
    for m in msgs {
        match out.last_mut() {
            Some(prev) if prev["role"] == "user" && m["role"] == "user" => {
                let mut c = blocks(&prev["content"]);
                c.extend(blocks(&m["content"]));
                prev["content"] = Value::Array(c);
            }
            _ => out.push(m),
        }
    }
    out
}

/// Read a non-streamed `/v1/messages` response.
///
/// The text is every `text` block joined; tool calls are the `tool_use` blocks; anything else (a
/// thinking block) is skipped rather than stringified. Whether an empty text is an error is the caller's
/// to say: a thinking model can spend its whole budget and reach none, which [`Reply::hit_token_limit`]
/// tells apart from a model that had nothing to say.
pub fn parse(body: &[u8]) -> Result<Reply, ParseError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| ParseError::Malformed(format!("messages json: {e}")))?;
    if v["type"] == "error" {
        let m = v["error"]["message"].as_str().unwrap_or("unknown error");
        return Err(ParseError::Api(format!("api error: {m}")));
    }
    let mut reply = Reply {
        model: str_of(&v["model"]),
        finish: v["stop_reason"].as_str().map(str::to_string),
        usage: usage(&v["usage"]),
        ..Reply::default()
    };
    for b in v["content"].as_array().into_iter().flatten() {
        match b["type"].as_str().unwrap_or_default() {
            "text" => reply.text.push_str(b["text"].as_str().unwrap_or_default()),
            "tool_use" => reply.calls.push(Call {
                id: str_of(&b["id"]),
                name: str_of(&b["name"]),
                input: b["input"].clone(),
            }),
            _ => {}
        }
    }
    Ok(reply)
}

fn usage(u: &Value) -> Usage {
    Usage {
        input: u64_at(u, "/input_tokens"),
        output: u64_at(u, "/output_tokens"),
        cache_read: u64_at(u, "/cache_read_input_tokens"),
        cache_write: u64_at(u, "/cache_creation_input_tokens"),
    }
}

/// A streamed reply, assembled as its events arrive. Feed each SSE `data:` payload (see
/// [`crate::sse::take_frames`]); text is handed to `on_text` as it is generated and everything else is
/// in the [`Reply`] that [`Stream::finish`] returns.
#[derive(Default)]
pub struct Stream {
    reply: Reply,
    /// The tool_use block being streamed: (id, name, partial JSON).
    open: Option<(String, String, String)>,
}

impl Stream {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(
        &mut self,
        data: &str,
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<(), ParseError> {
        let v: Value = serde_json::from_str(data)
            .map_err(|e| ParseError::Malformed(format!("bad model event: {e}")))?;
        match v["type"].as_str().unwrap_or_default() {
            "message_start" => {
                self.reply.model = str_of(&v["message"]["model"]);
                self.reply.usage = usage(&v["message"]["usage"]);
            }
            "content_block_start" if v["content_block"]["type"] == "tool_use" => {
                let b = &v["content_block"];
                self.open = Some((str_of(&b["id"]), str_of(&b["name"]), String::new()));
            }
            "content_block_delta" => match v["delta"]["type"].as_str().unwrap_or_default() {
                "text_delta" => {
                    let t = str_of(&v["delta"]["text"]);
                    on_text(&t);
                    self.reply.text.push_str(&t);
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
                    self.reply.calls.push(Call { id, name, input: parse_args(&json) });
                }
            }
            "message_delta" => {
                // Cumulative, so it replaces rather than adds.
                self.reply.usage.output = u64_at(&v, "/usage/output_tokens");
                if let Some(s) = v["delta"]["stop_reason"].as_str() {
                    self.reply.finish = Some(s.to_string());
                }
            }
            "error" => return Err(ParseError::Api(format!("model error: {}", v["error"]))),
            _ => {}
        }
        Ok(())
    }

    pub fn finish(self) -> Reply {
        self.reply
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Image, Role, ToolResult};

    fn msgs() -> Vec<Message> {
        vec![Message::user("hi")]
    }

    #[test]
    fn a_plain_turn_is_the_string_form_and_max_tokens_is_always_there() {
        let m = msgs();
        let v = body(&Request::new("claude-x", &m));
        assert_eq!(v["messages"], json!([{"role": "user", "content": "hi"}]));
        assert_eq!(v["max_tokens"], DEFAULT_MAX_TOKENS, "a request without it is a 400");
        for absent in
            ["system", "tools", "temperature", "stop_sequences", "stream", "cache_control", "seed"]
        {
            assert!(v.get(absent).is_none(), "{absent} is only sent when asked for");
        }
        let v = body(&Request { max_tokens: Some(256), ..Request::new("claude-x", &m) });
        assert_eq!(v["max_tokens"], 256);
    }

    #[test]
    fn the_system_prompt_is_a_string_or_a_cache_breakpoint() {
        let m = msgs();
        let v = body(&Request { system: Some("You write code."), ..Request::new("m", &m) });
        assert_eq!(v["system"], "You write code.");
        assert!(v.get("cache_control").is_none());

        let v = body(&Request {
            system: Some("You write code."),
            cache: true,
            ..Request::new("m", &m)
        });
        assert_eq!(v["system"][0]["text"], "You write code.");
        assert_eq!(v["system"][0]["cache_control"]["type"], "ephemeral", "the explicit breakpoint");
        assert_eq!(v["cache_control"]["type"], "ephemeral", "and the automatic one for the tail");
    }

    #[test]
    fn temperature_and_stop_sequences_are_sent_as_given() {
        let m = msgs();
        let stop = ["END".to_string(), "\n\n".to_string()];
        let v = body(&Request { temperature: Some(0.7), stop: &stop, ..Request::new("m", &m) });
        assert_eq!(v["temperature"], 0.7, "a JSON float, not 700 milli-units");
        assert_eq!(v["stop_sequences"], json!(["END", "\n\n"]));
        assert_eq!(body(&Request { stream: true, ..Request::new("m", &m) })["stream"], true);
    }

    #[test]
    fn pictures_and_words_keep_the_order_they_were_given_in() {
        let photo = Image { media_type: "image/png".into(), data: vec![0x89, b'P', b'N', b'G'] };
        let m = vec![
            Message::new(
                Role::User,
                vec![Part::Image(photo.clone()), Part::Text("what is this?".into())],
            ),
            Message::assistant("a plate"),
        ];
        let v = body(&Request::new("m", &m));
        assert_eq!(v["messages"][0]["content"][0]["type"], "image");
        assert_eq!(
            v["messages"][0]["content"][0]["source"],
            json!({"type": "base64", "media_type": "image/png", "data": "iVBORw=="})
        );
        assert_eq!(
            v["messages"][0]["content"][1],
            json!({"type": "text", "text": "what is this?"})
        );
        assert_eq!(v["messages"][1], json!({"role": "assistant", "content": "a plate"}));
    }

    #[test]
    fn tools_are_described_by_their_schema() {
        let m = msgs();
        let tools = [Tool {
            name: "read_file".into(),
            description: "Read a file".into(),
            schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        }];
        let v = body(&Request { tools: &tools, ..Request::new("m", &m) });
        assert_eq!(v["tools"][0]["name"], "read_file");
        assert_eq!(v["tools"][0]["input_schema"]["properties"]["path"]["type"], "string");
    }

    #[test]
    fn anthropic_gets_alternating_roles_after_tool_results() {
        let call = Call { id: "t1".into(), name: "run".into(), input: json!({}) };
        let m = vec![
            Message::user("a"),
            Message::new(Role::Assistant, vec![Part::ToolUse(call)]),
            Message::new(
                Role::User,
                vec![Part::ToolResult(ToolResult {
                    call_id: "t1".into(),
                    output: "x".into(),
                    is_error: false,
                })],
            ),
            Message::user("b"),
        ];
        let v = body(&Request::new("m", &m));
        let roles: Vec<&str> =
            v["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "assistant", "user"]);
        let last = &v["messages"][2]["content"];
        assert_eq!(
            (last[0]["type"].as_str(), last[1]["text"].as_str()),
            (Some("tool_result"), Some("b"))
        );
        assert_eq!(last[0]["tool_use_id"], "t1");
        assert_eq!(last[0]["is_error"], false);
        assert_eq!(v["messages"][1]["content"][0]["type"], "tool_use");
    }

    #[test]
    fn a_reply_is_its_text_blocks_its_tool_calls_and_its_true_usage() {
        let r = parse(
            br#"{"model":"claude-x","stop_reason":"tool_use","content":[
                {"type":"thinking","thinking":"hm"},
                {"type":"text","text":"Let me "},
                {"type":"text","text":"look."},
                {"type":"tool_use","id":"tu_1","name":"read_file","input":{"path":"a.txt"}}],
              "usage":{"input_tokens":120,"output_tokens":42,
                       "cache_creation_input_tokens":5,"cache_read_input_tokens":30}}"#,
        )
        .unwrap();
        assert_eq!(r.text, "Let me look.", "a thinking block is skipped, not stringified");
        assert_eq!(
            r.calls,
            vec![Call {
                id: "tu_1".into(),
                name: "read_file".into(),
                input: json!({"path": "a.txt"})
            }]
        );
        assert_eq!((r.model.as_str(), r.finish.as_deref()), ("claude-x", Some("tool_use")));
        assert_eq!(r.usage, Usage { input: 120, output: 42, cache_read: 30, cache_write: 5 });
        assert_eq!(r.usage.total_input(), 155, "else a cached run looks nearly free");
    }

    #[test]
    fn a_thinking_model_that_never_wrote_any_text_says_it_hit_the_limit() {
        let r = parse(
            br#"{"content":[{"type":"thinking","thinking":"..."}],"stop_reason":"max_tokens"}"#,
        )
        .unwrap();
        assert!(r.text.is_empty() && r.hit_token_limit());
    }

    #[test]
    fn an_error_envelope_is_an_error_even_under_a_success_status() {
        let e = parse(
            br#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        )
        .unwrap_err();
        assert_eq!(e, ParseError::Api("api error: Overloaded".into()));
        assert_eq!(
            parse(br#"{"type":"error"}"#).unwrap_err(),
            ParseError::Api("api error: unknown error".into())
        );
        assert!(matches!(parse(b"not json"), Err(ParseError::Malformed(_))));
    }

    #[test]
    fn anthropic_stream_yields_text_a_tool_call_and_tokens() {
        let frames = [
            r#"{"type":"message_start","message":{"model":"claude-x","usage":{"input_tokens":120,"cache_read_input_tokens":30,"cache_creation_input_tokens":5,"output_tokens":1}}}"#,
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
        let mut acc = Stream::new();
        let mut seen = String::new();
        for f in frames {
            acc.feed(f, &mut |t| seen.push_str(t)).unwrap();
        }
        let r = acc.finish();
        assert_eq!((seen.as_str(), r.text.as_str()), ("Hello", "Hello"));
        assert_eq!(
            r.calls,
            vec![Call {
                id: "tu_1".into(),
                name: "read_file".into(),
                input: json!({"path": "a.txt"})
            }]
        );
        assert_eq!(r.usage, Usage { input: 120, output: 42, cache_read: 30, cache_write: 5 });
        assert_eq!((r.model.as_str(), r.finish.as_deref()), ("claude-x", Some("tool_use")));
    }

    #[test]
    fn a_model_error_event_fails_the_turn() {
        let mut acc = Stream::new();
        let e = acc.feed(r#"{"type":"error","error":{"type":"overloaded_error"}}"#, &mut |_| {});
        assert!(matches!(e, Err(ParseError::Api(_))));
        assert!(matches!(acc.feed("not json", &mut |_| {}), Err(ParseError::Malformed(_))));
    }
}
