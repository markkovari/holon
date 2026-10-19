//! The OpenAI-compatible chat API (`POST …/chat/completions`), which is also what vLLM, llama.cpp,
//! Ollama and mlx_lm serve.
//!
//! What differs from the Anthropic dialect, all of it handled here:
//! * the system prompt is the first message;
//! * a tool call is a `tool_calls` entry on the assistant message whose arguments are a JSON STRING, and
//!   its answer is a message of its own with the role `tool`;
//! * a picture is an `image_url` part carrying a data URL;
//! * cached prompt tokens are a SUBSET of `prompt_tokens`, not a count beside it.

use serde_json::{json, Value};

use crate::model::{
    parse_args, str_of, u64_at, Call, Message, ParseError, Part, Reply, Request, Role, Tool, Usage,
};

/// The request body for `…/chat/completions`.
///
/// `stream` is always written out, `false` included. `max_tokens`, `temperature`, `seed` and `stop`
/// are sent when set and left to the server's defaults otherwise.
pub fn body(req: &Request) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(system) = req.system {
        messages.push(json!({"role": "system", "content": system}));
    }
    for m in req.messages {
        push(&mut messages, m);
    }
    let mut v = json!({"model": req.model, "messages": messages, "stream": req.stream});
    if req.stream {
        v["stream_options"] = json!({"include_usage": true});
    }
    if let Some(n) = req.max_tokens {
        v["max_tokens"] = json!(n);
    }
    if let Some(t) = req.temperature {
        v["temperature"] = json!(t);
    }
    if let Some(s) = req.seed {
        v["seed"] = json!(s);
    }
    if !req.stop.is_empty() {
        v["stop"] = json!(req.stop);
    }
    if !req.tools.is_empty() {
        v["tools"] = req.tools.iter().map(tool).collect();
    }
    v
}

fn tool(t: &Tool) -> Value {
    json!({"type": "function",
           "function": {"name": t.name, "description": t.description, "parameters": t.schema}})
}

fn push(out: &mut Vec<Value>, m: &Message) {
    match m.role {
        Role::User => {
            // A tool's answer is a message of its own, and goes first: it answers the call before.
            for p in &m.parts {
                if let Part::ToolResult(r) = p {
                    let content =
                        if r.is_error { format!("ERROR: {}", r.output) } else { r.output.clone() };
                    out.push(
                        json!({"role": "tool", "tool_call_id": r.call_id, "content": content}),
                    );
                }
            }
            let said: Vec<&Part> =
                m.parts.iter().filter(|p| matches!(p, Part::Text(_) | Part::Image(_))).collect();
            match said.as_slice() {
                [] => {}
                [Part::Text(t)] => out.push(json!({"role": "user", "content": t})),
                parts => out.push(json!({"role": "user", "content": parts.iter().map(|p| match p {
                    Part::Image(i) => json!({"type": "image_url", "image_url": {"url": i.data_url()}}),
                    Part::Text(t) => json!({"type": "text", "text": t}),
                    _ => unreachable!("filtered to text and pictures above"),
                }).collect::<Vec<_>>()})),
            }
        }
        Role::Assistant => {
            let text: String = m
                .parts
                .iter()
                .filter_map(|p| if let Part::Text(t) = p { Some(t.as_str()) } else { None })
                .collect();
            let mut a = json!({"role": "assistant", "content": text});
            let calls: Vec<Value> = m
                .parts
                .iter()
                .filter_map(|p| if let Part::ToolUse(c) = p { Some(c) } else { None })
                .map(|c| {
                    json!({"id": c.id, "type": "function",
                           "function": {"name": c.name, "arguments": c.input.to_string()}})
                })
                .collect();
            if !calls.is_empty() {
                a["tool_calls"] = Value::Array(calls);
            }
            out.push(a);
        }
    }
}

/// Token counts as OpenAI reports them. Cached prompt tokens are a subset of `prompt_tokens` and priced
/// apart, so they come out of `input` and into `cache_read`.
fn usage(u: &Value) -> Usage {
    let cached = u64_at(u, "/prompt_tokens_details/cached_tokens");
    Usage {
        input: u64_at(u, "/prompt_tokens").saturating_sub(cached),
        output: u64_at(u, "/completion_tokens"),
        cache_read: cached,
        cache_write: 0,
    }
}

/// Read a non-streamed `…/chat/completions` response.
///
/// The text is the first choice's `content` (null is empty), its tool calls are read with their
/// arguments parsed, and `finish` is its `finish_reason`. A `200` carrying an `error` object is an
/// error. Whether an empty text is one is the caller's to say: a reasoning model can spend its whole
/// budget thinking, which [`Reply::hit_token_limit`] tells apart from a model with nothing to say.
pub fn parse(body: &[u8]) -> Result<Reply, ParseError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| ParseError::Malformed(format!("chat json: {e}")))?;
    if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
        let m = e["message"].as_str().map(str::to_string).unwrap_or_else(|| e.to_string());
        return Err(ParseError::Api(format!("api error: {m}")));
    }
    let choice = &v["choices"][0];
    let calls = choice["message"]["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, c)| {
            let args = str_of(&c["function"]["arguments"]);
            call(i, &str_of(&c["id"]), &str_of(&c["function"]["name"]), &args)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Reply {
        text: choice["message"]["content"].as_str().unwrap_or_default().to_string(),
        calls,
        finish: choice["finish_reason"].as_str().map(str::to_string),
        model: str_of(&v["model"]),
        usage: usage(&v["usage"]),
    })
}

/// A finished tool call. An id the server left out is made up from the call's position, because the
/// answer has to name the call it answers.
fn call(index: usize, id: &str, name: &str, args: &str) -> Result<Call, ParseError> {
    if name.is_empty() {
        return Err(ParseError::Malformed("model sent a tool call with no name".into()));
    }
    let id = if id.is_empty() { format!("call_{index}") } else { id.to_string() };
    Ok(Call { id, name: name.to_string(), input: parse_args(args) })
}

/// A streamed reply, assembled as its chunks arrive. Feed each SSE `data:` payload (see
/// [`crate::sse::take_frames`]); text is handed to `on_text` as it is generated and everything else is
/// in the [`Reply`] that [`Stream::finish`] returns. The last chunk carries `usage` only when the
/// request asked for it (`stream_options.include_usage`, which [`body`] sets for a stream).
#[derive(Default)]
pub struct Stream {
    reply: Reply,
    /// Tool calls by stream index: (id, name, argument JSON so far).
    calls: Vec<(String, String, String)>,
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
        if data.trim() == "[DONE]" {
            return Ok(());
        }
        let v: Value = serde_json::from_str(data)
            .map_err(|e| ParseError::Malformed(format!("bad model chunk: {e}")))?;
        if let Some(e) = v.get("error") {
            return Err(ParseError::Api(format!("model error: {e}")));
        }
        if let Some(m) = v["model"].as_str() {
            self.reply.model = m.to_string();
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.reply.usage = usage(u);
        }
        if let Some(f) = v["choices"][0]["finish_reason"].as_str() {
            self.reply.finish = Some(f.to_string());
        }
        let delta = &v["choices"][0]["delta"];
        if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            on_text(t);
            self.reply.text.push_str(t);
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

    pub fn finish(mut self) -> Result<Reply, ParseError> {
        for (i, (id, name, args)) in self.calls.into_iter().enumerate() {
            self.reply.calls.push(call(i, &id, &name, &args)?);
        }
        Ok(self.reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Image, ToolResult};

    #[test]
    fn the_system_prompt_is_the_first_message_and_stream_is_always_written() {
        let m = vec![Message::user("hi"), Message::assistant("hello")];
        let v =
            body(&Request { system: Some("You are a vet."), ..Request::new("gpt-4o-mini", &m) });
        assert_eq!(v["model"], "gpt-4o-mini");
        assert_eq!(v["messages"][0], json!({"role": "system", "content": "You are a vet."}));
        assert_eq!(v["messages"][1], json!({"role": "user", "content": "hi"}));
        assert_eq!(v["messages"][2], json!({"role": "assistant", "content": "hello"}));
        assert_eq!(v["stream"], false, "written out, not left to a server's default");
        for absent in ["temperature", "max_tokens", "seed", "stop", "tools", "stream_options"] {
            assert!(v.get(absent).is_none(), "{absent} is only sent when set");
        }
    }

    #[test]
    fn the_knobs_are_sent_as_given() {
        let m = vec![Message::user("hi")];
        let stop = ["END".to_string()];
        let v = body(&Request {
            temperature: Some(0.7),
            max_tokens: Some(256),
            seed: Some(7),
            stop: &stop,
            stream: true,
            ..Request::new("m", &m)
        });
        assert_eq!(
            (v["temperature"].clone(), v["max_tokens"].clone(), v["seed"].clone()),
            (json!(0.7), json!(256), json!(7))
        );
        assert_eq!(v["stop"], json!(["END"]));
        assert_eq!(v["stream"], true);
        assert_eq!(v["stream_options"], json!({"include_usage": true}));
    }

    #[test]
    fn content_with_quotes_and_newlines_round_trips_as_it_was() {
        let m = vec![Message::user("quote \" and \n newline")];
        let s = body(&Request::new("m", &m)).to_string();
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["messages"][0]["content"], "quote \" and \n newline");
    }

    #[test]
    fn a_picture_is_a_data_url_after_or_before_the_words_as_the_caller_ordered_them() {
        let photo = Image { media_type: "image/png".into(), data: vec![0x89, b'P', b'N', b'G'] };
        let m = vec![Message::new(
            Role::User,
            vec![
                Part::Text("what is this?".into()),
                Part::Image(photo.clone()),
                Part::Image(photo),
            ],
        )];
        let v = body(&Request { system: Some("sys"), ..Request::new("m", &m) });
        let content = &v["messages"][1]["content"];
        assert_eq!(content[0], json!({"type": "text", "text": "what is this?"}));
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,iVBORw==");
        assert_eq!(content[2], content[1], "each picture, in order");
        assert_eq!(v["messages"][0]["content"], "sys", "the system message stays text");
    }

    #[test]
    fn a_tool_call_and_its_answer_are_their_own_messages() {
        let m = vec![
            Message::user("go"),
            Message::new(
                Role::Assistant,
                vec![
                    Part::Text("On it".into()),
                    Part::ToolUse(Call {
                        id: "c1".into(),
                        name: "run".into(),
                        input: json!({"command": "ls"}),
                    }),
                ],
            ),
            Message::new(
                Role::User,
                vec![
                    Part::ToolResult(ToolResult {
                        call_id: "c1".into(),
                        output: "a b".into(),
                        is_error: false,
                    }),
                    Part::ToolResult(ToolResult {
                        call_id: "c2".into(),
                        output: "no".into(),
                        is_error: true,
                    }),
                    Part::Text("and now?".into()),
                ],
            ),
        ];
        let v = body(&Request::new("m", &m));
        let a = &v["messages"][1];
        assert_eq!(a["content"], "On it");
        assert_eq!(a["tool_calls"][0]["id"], "c1");
        assert_eq!(a["tool_calls"][0]["function"]["name"], "run");
        assert_eq!(
            a["tool_calls"][0]["function"]["arguments"], r#"{"command":"ls"}"#,
            "a JSON string"
        );
        assert_eq!(
            v["messages"][2],
            json!({"role": "tool", "tool_call_id": "c1", "content": "a b"})
        );
        assert_eq!(
            v["messages"][3],
            json!({"role": "tool", "tool_call_id": "c2", "content": "ERROR: no"})
        );
        assert_eq!(v["messages"][4], json!({"role": "user", "content": "and now?"}));
    }

    #[test]
    fn tools_are_functions() {
        let m = vec![Message::user("hi")];
        let tools = [Tool {
            name: "run".into(),
            description: "Run".into(),
            schema: json!({"type": "object"}),
        }];
        let v = body(&Request { tools: &tools, ..Request::new("m", &m) });
        assert_eq!(
            v["tools"][0],
            json!({"type": "function", "function": {"name": "run", "description": "Run", "parameters": {"type": "object"}}})
        );
    }

    #[test]
    fn a_reply_is_its_text_model_finish_and_usage() {
        let r = parse(
            br#"{"model":"gpt-4o-mini","choices":[{"message":{"role":"assistant","content":"Bella is limping."},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":7}}"#,
        )
        .unwrap();
        assert_eq!(r.text, "Bella is limping.");
        assert_eq!((r.model.as_str(), r.finish.as_deref()), ("gpt-4o-mini", Some("stop")));
        assert_eq!(r.usage, Usage { input: 11, output: 7, cache_read: 0, cache_write: 0 });
    }

    #[test]
    fn cached_prompt_tokens_come_out_of_input_and_the_prompt_is_still_the_whole() {
        let r = parse(
            br#"{"choices":[{"message":{"content":"x"}}],"usage":{"prompt_tokens":100,"completion_tokens":7,"prompt_tokens_details":{"cached_tokens":40}}}"#,
        )
        .unwrap();
        assert_eq!(r.usage, Usage { input: 60, output: 7, cache_read: 40, cache_write: 0 });
        assert_eq!(r.usage.total_input(), 100);
    }

    #[test]
    fn null_content_with_tool_calls_is_an_empty_text_and_the_calls_parsed() {
        let r = parse(
            br#"{"choices":[{"message":{"content":null,"tool_calls":[
                {"id":"c1","type":"function","function":{"name":"run","arguments":"{\"command\":\"ls\"}"}},
                {"type":"function","function":{"name":"read","arguments":""}}]},"finish_reason":"tool_calls"}]}"#,
        )
        .unwrap();
        assert_eq!(r.text, "");
        assert_eq!(
            r.calls[0],
            Call { id: "c1".into(), name: "run".into(), input: json!({"command": "ls"}) }
        );
        assert_eq!(r.calls[1], Call { id: "call_1".into(), name: "read".into(), input: json!({}) });
    }

    #[test]
    fn nothing_to_read_is_an_empty_reply_and_the_caller_decides_what_that_means() {
        assert_eq!(parse(br#"{"choices":[{"message":{"content":""}}]}"#).unwrap().text, "");
        assert_eq!(parse(br#"{"choices":[]}"#).unwrap(), Reply::default());
        let r = parse(br#"{"choices":[{"message":{"content":null},"finish_reason":"length"}]}"#)
            .unwrap();
        assert!(r.hit_token_limit(), "a reasoning model that thought until the budget ran out");
    }

    #[test]
    fn an_error_object_is_an_error_and_bad_json_is_malformed() {
        assert_eq!(
            parse(br#"{"error":{"message":"rate limited","type":"x"}}"#).unwrap_err(),
            ParseError::Api("api error: rate limited".into())
        );
        assert!(matches!(parse(b"not json"), Err(ParseError::Malformed(_))));
    }

    #[test]
    fn openai_stream_assembles_tool_arguments_by_index() {
        let frames = [
            r#"{"model":"m1","choices":[{"delta":{"role":"assistant","content":"On it"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"run","arguments":"{\"comm"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"and\":\"ls\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":7,"prompt_tokens_details":{"cached_tokens":40}}}"#,
            "[DONE]",
        ];
        let mut acc = Stream::new();
        let mut seen = String::new();
        for f in frames {
            acc.feed(f, &mut |t| seen.push_str(t)).unwrap();
        }
        let r = acc.finish().unwrap();
        assert_eq!(seen, "On it");
        assert_eq!(
            r.calls,
            vec![Call { id: "c1".into(), name: "run".into(), input: json!({"command": "ls"}) }]
        );
        assert_eq!(r.usage, Usage { input: 60, output: 7, cache_read: 40, cache_write: 0 });
        assert_eq!((r.model.as_str(), r.finish.as_deref()), ("m1", Some("tool_calls")));
    }

    #[test]
    fn a_stream_error_fails_it_and_a_nameless_tool_call_fails_the_finish() {
        let mut acc = Stream::new();
        assert!(matches!(
            acc.feed(r#"{"error":{"message":"boom"}}"#, &mut |_| {}),
            Err(ParseError::Api(_))
        ));
        assert!(matches!(acc.feed("not json", &mut |_| {}), Err(ParseError::Malformed(_))));

        let mut acc = Stream::new();
        acc.feed(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"arguments":"{}"}}]}}]}"#, &mut |_| {})
            .unwrap();
        assert!(matches!(acc.finish(), Err(ParseError::Malformed(_))));
    }
}
