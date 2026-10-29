//! The Gemini Generative AI REST API (`POST …/models/{model}:generateContent`).
//! Supports native multi-part content (text, inline image/audio), system instructions, and tool calling.

pub const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta";

use serde_json::{json, Value};

use crate::model::{
    str_of, u64_at, Call, Message, ParseError, Part, Reply, Request, Role, Tool, Usage,
};

pub fn body(req: &Request) -> Value {
    let mut v = json!({});

    if let Some(system) = req.system {
        v["system_instruction"] = json!({
            "parts": [{ "text": system }]
        });
    }

    let contents: Vec<Value> = req.messages.iter().map(content_of).collect();
    v["contents"] = json!(contents);

    let mut gen_cfg = json!({});
    if let Some(n) = req.max_tokens {
        gen_cfg["maxOutputTokens"] = json!(n);
    }
    if let Some(t) = req.temperature {
        gen_cfg["temperature"] = json!(t);
    }
    if !req.stop.is_empty() {
        gen_cfg["stopSequences"] = json!(req.stop);
    }
    if gen_cfg.as_object().is_some_and(|m| !m.is_empty()) {
        v["generationConfig"] = gen_cfg;
    }

    if !req.tools.is_empty() {
        let declarations: Vec<Value> = req.tools.iter().map(tool).collect();
        v["tools"] = json!([{ "function_declarations": declarations }]);
    }

    v
}

fn tool(t: &Tool) -> Value {
    json!({
        "name": t.name,
        "description": t.description,
        "parameters": t.schema
    })
}

fn content_of(m: &Message) -> Value {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "model",
    };
    let parts: Vec<Value> = m.parts.iter().map(part_of).collect();
    json!({
        "role": role,
        "parts": parts,
    })
}

fn part_of(p: &Part) -> Value {
    match p {
        Part::Text(t) => json!({ "text": t }),
        Part::Image(img) => json!({
            "inline_data": {
                "mime_type": img.media_type,
                "data": img.base64(),
            }
        }),
        Part::ToolUse(c) => json!({
            "function_call": {
                "name": c.name,
                "args": c.input,
            }
        }),
        Part::ToolResult(r) => json!({
            "function_response": {
                "name": r.call_id,
                "response": {
                    "name": r.call_id,
                    "content": if r.is_error { format!("ERROR: {}", r.output) } else { r.output.clone() }
                }
            }
        }),
    }
}

pub fn reply(body: &[u8]) -> Result<Reply, ParseError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| ParseError::Malformed(format!("bad Gemini JSON: {e}")))?;

    if let Some(err) = v.get("error") {
        let msg = err["message"].as_str().unwrap_or("unknown Gemini API error");
        return Err(ParseError::Api(msg.to_string()));
    }

    let candidate = v["candidates"]
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| ParseError::Malformed("Gemini reply had no candidates".into()))?;

    let parts = candidate["content"]["parts"]
        .as_array()
        .ok_or_else(|| ParseError::Malformed("candidate had no parts".into()))?;

    let mut text = String::new();
    let mut calls = Vec::new();

    for (idx, p) in parts.iter().enumerate() {
        if let Some(t) = p["text"].as_str() {
            text.push_str(t);
        } else if let Some(fc) = p.get("functionCall").or_else(|| p.get("function_call")) {
            let name = str_of(&fc["name"]);
            if name.is_empty() {
                return Err(ParseError::Malformed("functionCall has no name".into()));
            }
            let id = format!("call_{idx}");
            let input = fc.get("args").cloned().unwrap_or(Value::Null);
            calls.push(Call { id, name, input });
        }
    }

    let finish = candidate["finishReason"].as_str().map(str::to_string);
    let model = v["modelVersion"].as_str().unwrap_or_default().to_string();
    let usage = usage(v.get("usageMetadata"));

    Ok(Reply { text, calls, finish, model, usage })
}

fn usage(u: Option<&Value>) -> Usage {
    let Some(u) = u else {
        return Usage::default();
    };
    Usage {
        input: u64_at(u, "/promptTokenCount"),
        output: u64_at(u, "/candidatesTokenCount"),
        cache_read: u64_at(u, "/cachedContentTokenCount"),
        cache_write: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Image;

    #[test]
    fn body_formats_system_and_messages_correctly() {
        let stop_seq = vec!["\nUser:".to_string()];
        let req = Request {
            model: "gemini-2.5-flash",
            system: Some("You are a helpful assistant."),
            messages: &[Message::user("Hello!"), Message::assistant("Hi there!")],
            tools: &[],
            max_tokens: Some(512),
            temperature: Some(0.7),
            seed: None,
            stop: &stop_seq,
            stream: false,
            cache: false,
            blocks: false,
        };

        let b = body(&req);
        assert_eq!(b["system_instruction"]["parts"][0]["text"], "You are a helpful assistant.");
        assert_eq!(b["contents"].as_array().unwrap().len(), 2);
        assert_eq!(b["contents"][0]["role"], "user");
        assert_eq!(b["contents"][0]["parts"][0]["text"], "Hello!");
        assert_eq!(b["contents"][1]["role"], "model");
        assert_eq!(b["contents"][1]["parts"][0]["text"], "Hi there!");
        assert_eq!(b["generationConfig"]["maxOutputTokens"], 512);
        assert_eq!(b["generationConfig"]["temperature"], 0.7);
        assert_eq!(b["generationConfig"]["stopSequences"][0], "\nUser:");
    }

    #[test]
    fn body_formats_images_and_tools() {
        let img = Image { media_type: "image/png".into(), data: vec![1, 2, 3] };
        let tool_def = Tool {
            name: "get_weather".into(),
            description: "Fetch weather".into(),
            schema: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
        };

        let req = Request {
            model: "gemini-2.5-flash",
            system: None,
            messages: &[Message::new(
                Role::User,
                vec![Part::Text("Look:".into()), Part::Image(img)],
            )],
            tools: &[tool_def],
            max_tokens: None,
            temperature: None,
            seed: None,
            stop: &[],
            stream: false,
            cache: false,
            blocks: false,
        };

        let b = body(&req);
        assert_eq!(b["contents"][0]["parts"][1]["inline_data"]["mime_type"], "image/png");
        assert_eq!(b["contents"][0]["parts"][1]["inline_data"]["data"], "AQID");
        assert_eq!(b["tools"][0]["function_declarations"][0]["name"], "get_weather");
    }

    #[test]
    fn reply_parses_content_and_usage() {
        let json_resp = br#"{
            "candidates": [
                {
                    "content": {
                        "parts": [
                            { "text": "The weather is sunny." },
                            {
                                "functionCall": {
                                    "name": "log_result",
                                    "args": { "status": "ok" }
                                }
                            }
                        ],
                        "role": "model"
                    },
                    "finishReason": "STOP"
                }
            ],
            "usageMetadata": {
                "promptTokenCount": 25,
                "candidatesTokenCount": 14,
                "cachedContentTokenCount": 10
            },
            "modelVersion": "gemini-2.5-flash-001"
        }"#;

        let r = reply(json_resp).unwrap();
        assert_eq!(r.text, "The weather is sunny.");
        assert_eq!(r.finish.as_deref(), Some("STOP"));
        assert_eq!(r.model, "gemini-2.5-flash-001");
        assert_eq!(r.usage.input, 25);
        assert_eq!(r.usage.output, 14);
        assert_eq!(r.usage.cache_read, 10);
        assert_eq!(r.calls.len(), 1);
        assert_eq!(r.calls[0].name, "log_result");
        assert_eq!(r.calls[0].input["status"], "ok");
    }

    #[test]
    fn reply_handles_error() {
        let err_resp = br#"{
            "error": {
                "code": 429,
                "message": "Resource has been exhausted (e.g. check quota)."
            }
        }"#;

        let err = reply(err_resp).unwrap_err();
        match err {
            ParseError::Api(m) => assert!(m.contains("Resource has been exhausted")),
            _ => panic!("expected Api error"),
        }
    }
}
