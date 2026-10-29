//! Model clients. Deliberately plain text-in/text-out: tool use is a JSON
//! protocol the loop layers on top (see `agent.rs`), so the same agent runs on
//! Apple's on-device model, any OpenAI-compatible server, or Anthropic —
//! including small models with no native tool-calling.

use std::sync::Mutex;
use std::time::Duration;

use holon_llm::{anthropic, openai, Message, ParseError, Part, Request, Role};

use crate::spec::ModelSpec;

/// A picture that goes to the model with a message. The wire crate's, which never prints the bytes.
pub use holon_llm::Image;

#[derive(Clone, Debug, PartialEq)]
pub struct Msg {
    pub role: &'static str, // "user" | "assistant"
    pub content: String,
    /// Pictures that came with it. Only a user message has any.
    pub images: Vec<Image>,
}

impl Msg {
    pub fn user(c: impl Into<String>) -> Self {
        Self { role: "user", content: c.into(), images: Vec::new() }
    }
    pub fn assistant(c: impl Into<String>) -> Self {
        Self { role: "assistant", content: c.into(), images: Vec::new() }
    }
    pub fn with_images(mut self, images: Vec<Image>) -> Self {
        self.images = images;
        self
    }
}

/// The messages for a model that cannot see: the pictures are dropped and the message that carried
/// them says so, so the model asks for the thing in words instead of answering as if it had looked.
fn without_images(msgs: &[Msg]) -> std::borrow::Cow<'_, [Msg]> {
    if msgs.iter().all(|m| m.images.is_empty()) {
        return std::borrow::Cow::Borrowed(msgs);
    }
    std::borrow::Cow::Owned(
        msgs.iter()
            .map(|m| match m.images.len() {
                0 => m.clone(),
                n => Msg {
                    role: m.role,
                    content: format!(
                        "{}\n\n({n} photo{} attached, but you cannot see images. Say so, and ask \
                         Mark to describe {} in words.)",
                        m.content,
                        if n == 1 { " was" } else { "s were" },
                        if n == 1 { "it" } else { "them" },
                    ),
                    images: Vec::new(),
                },
            })
            .collect(),
    )
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
}

#[derive(Debug)]
pub struct Reply {
    pub text: String,
    pub usage: Usage,
}

/// Where `ModelSpec::Local` points — the runtime's own configuration, since
/// an agent's spec must not hard-code a port that changes every launch.
#[derive(Clone, Debug, Default)]
pub struct LocalModel {
    pub base_url: Option<String>,
    pub model: String,
}

pub fn complete(
    spec: &ModelSpec,
    local: &LocalModel,
    system: &str,
    messages: &[Msg],
    mock_cursor: &Mutex<usize>,
) -> Result<Reply, String> {
    match spec {
        ModelSpec::Local => {
            let base = local.base_url.as_deref().ok_or(
                "no local model is running (Apple `fm` is macOS-only); give this agent an \
                 OpenAi or Anthropic model instead",
            )?;
            let model = if local.model.is_empty() { "system" } else { &local.model };
            openai(base, model, "", system, &without_images(messages))
        }
        ModelSpec::OpenAi { base_url, model, api_key_env, api_key_file, service, vision } => {
            let k = if api_key_file.is_empty() {
                key(api_key_env)?
            } else {
                key_from_file(api_key_file)?
            };
            if !service.is_empty() {
                crate::lazy::ensure(service, base_url)?;
            }
            let msgs = if *vision {
                std::borrow::Cow::Borrowed(messages)
            } else {
                without_images(messages)
            };
            let r = openai(base_url, model, &k, system, &msgs);
            if !service.is_empty() {
                crate::lazy::touch(service);
            }
            r
        }
        ModelSpec::Fallback { models } => {
            let mut errors: Vec<String> = Vec::new();
            for m in models {
                match complete(m, local, system, messages, mock_cursor) {
                    Ok(r) => return Ok(r),
                    Err(e) => errors.push(e),
                }
            }
            Err(format!("no model answered: {}", errors.join(" | ")))
        }
        ModelSpec::Anthropic { model, api_key_env } => {
            anthropic(model, &key(api_key_env)?, system, messages)
        }
        ModelSpec::Gemini { model, api_key_env, api_key_file } => {
            let k = if api_key_file.is_empty() {
                key(api_key_env)?
            } else {
                key_from_file(api_key_file)?
            };
            gemini(model, &k, system, messages)
        }
        ModelSpec::Mock { replies } => {
            let mut i = mock_cursor.lock().unwrap();
            let text = replies.get(*i).cloned().ok_or("mock model ran out of replies")?;
            *i += 1;
            let words = |s: &str| s.split_whitespace().count() as u64;
            let input = system.split_whitespace().count() as u64
                + messages.iter().map(|m| words(&m.content)).sum::<u64>();
            Ok(Reply { usage: Usage { input, output: words(&text) }, text })
        }
    }
}

fn key(env: &str) -> Result<String, String> {
    if env.is_empty() {
        return Ok(String::new());
    }
    std::env::var(env).map_err(|_| format!("environment variable {env} is not set"))
}

/// A key read from a file at the moment it is needed (never kept, never logged).
fn key_from_file(path: &str) -> Result<String, String> {
    let k = std::fs::read_to_string(path).map_err(|e| format!("key file {path}: {e}"))?;
    let k = k.trim().to_string();
    if k.is_empty() {
        Err(format!("key file {path} is empty"))
    } else {
        Ok(k)
    }
}

fn http() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())
}

/// A message as the wire crate writes it. The two dialects want a message's pictures in different
/// places — Anthropic's before the words about them, OpenAI's after — so the caller says which.
fn wire(msgs: &[Msg], pictures_first: bool) -> Vec<Message> {
    msgs.iter()
        .map(|m| {
            let role = if m.role == "assistant" { Role::Assistant } else { Role::User };
            let words = Part::Text(m.content.clone());
            let pictures = m.images.iter().cloned().map(Part::Image);
            let parts = if pictures_first {
                pictures.chain(std::iter::once(words)).collect()
            } else {
                std::iter::once(words).chain(pictures).collect()
            };
            Message::new(role, parts)
        })
        .collect()
}

fn openai(base: &str, model: &str, key: &str, system: &str, msgs: &[Msg]) -> Result<Reply, String> {
    let messages = wire(msgs, false);
    let req = Request {
        system: Some(system),
        // Room for a reasoning model to think AND answer; some servers default to a few hundred.
        max_tokens: Some(4096),
        ..Request::new(model, &messages)
    };
    let mut req = http()?
        .post(format!("{}/v1/chat/completions", base.trim_end_matches('/')))
        .json(&openai::body(&req));
    if !key.is_empty() {
        req = req.bearer_auth(key);
    }
    let body = send(req)?;
    let r = openai::parse(body.as_bytes()).map_err(read_error)?;
    if r.text.is_empty() {
        return Err(if r.hit_token_limit() {
            "the model ran out of tokens while thinking and never answered; \
             ask again, or use a model that thinks less"
                .into()
        } else {
            format!("unexpected model response: {}", clip(&body))
        });
    }
    Ok(Reply {
        text: r.text,
        usage: Usage { input: r.usage.total_input(), output: r.usage.output },
    })
}

fn anthropic(model: &str, key: &str, system: &str, msgs: &[Msg]) -> Result<Reply, String> {
    let messages = wire(msgs, true);
    let req =
        Request { system: Some(system), max_tokens: Some(2048), ..Request::new(model, &messages) };
    let req = http()?
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", key)
        .header("anthropic-version", anthropic::VERSION)
        .json(&anthropic::body(&req));
    let body = send(req)?;
    let r = anthropic::parse(body.as_bytes()).map_err(read_error)?;
    if r.text.is_empty() {
        return Err(format!("unexpected model response: {}", clip(&body)));
    }
    Ok(Reply {
        text: r.text,
        usage: Usage { input: r.usage.total_input(), output: r.usage.output },
    })
}

fn gemini(model: &str, key: &str, system: &str, msgs: &[Msg]) -> Result<Reply, String> {
    let messages = wire(msgs, false);
    let req =
        Request { system: Some(system), max_tokens: Some(4096), ..Request::new(model, &messages) };
    let url = format!("{}/models/{}:generateContent?key={key}", holon_llm::gemini::ENDPOINT, model);
    let req = http()?.post(url).json(&holon_llm::gemini::body(&req));
    let body = send(req)?;
    let r = holon_llm::gemini::reply(body.as_bytes()).map_err(read_error)?;
    if r.text.is_empty() {
        return Err(format!("unexpected model response: {}", clip(&body)));
    }
    Ok(Reply {
        text: r.text,
        usage: Usage { input: r.usage.total_input(), output: r.usage.output },
    })
}

/// The body of a successful response; a failed request or a non-2xx status is the error.
fn send(req: reqwest::blocking::RequestBuilder) -> Result<String, String> {
    let r = req.send().map_err(|e| format!("model request failed: {e}"))?;
    let status = r.status();
    let body = r.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("model returned {status}: {}", clip(&body)));
    }
    Ok(body)
}

fn read_error(e: ParseError) -> String {
    match e {
        ParseError::Malformed(m) => format!("model sent invalid JSON: {m}"),
        ParseError::Api(m) => m,
    }
}

fn clip(s: &str) -> String {
    s.chars().take(300).collect()
}

#[cfg(test)]
mod vision_tests {
    use super::*;
    use serde_json::{json, Value};

    fn photo() -> Image {
        Image { media_type: "image/png".into(), data: vec![0x89, b'P', b'N', b'G'] }
    }

    /// A one-request OpenAI-compatible server: gives back the JSON body it was sent.
    fn stand_in() -> (String, std::sync::mpsc::Receiver<Value>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Ok(mut req) = server.recv() {
                let mut body = String::new();
                std::io::Read::read_to_string(req.as_reader(), &mut body).unwrap();
                let _ = tx.send(serde_json::from_str(&body).unwrap());
                let reply = json!({"choices": [{"message": {"content": "a plate of rice"}}],
                                   "usage": {"prompt_tokens": 7, "completion_tokens": 4}});
                let _ = req.respond(tiny_http::Response::from_string(reply.to_string()));
            }
        });
        (base, rx)
    }

    fn openai_spec(base: &str, vision: bool) -> ModelSpec {
        ModelSpec::OpenAi {
            base_url: base.into(),
            model: "deepseek-chat".into(),
            api_key_env: String::new(),
            api_key_file: String::new(),
            service: String::new(),
            vision,
        }
    }

    fn ask(spec: &ModelSpec, msgs: &[Msg]) -> Result<Reply, String> {
        complete(spec, &LocalModel::default(), "sys", msgs, &Mutex::new(0))
    }

    #[test]
    fn a_vision_model_gets_the_words_then_each_picture_as_a_data_url() {
        let (base, rx) = stand_in();
        let r = ask(
            &openai_spec(&base, true),
            &[Msg::user("what is this?").with_images(vec![photo(), photo()])],
        )
        .unwrap();
        assert_eq!(r.text, "a plate of rice");
        let sent = rx.recv().unwrap();
        let content = &sent["messages"][1]["content"];
        assert_eq!(content[0], json!({"type": "text", "text": "what is this?"}));
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,iVBORw==");
        assert_eq!(content[2], content[1], "each picture, in order");
        assert_eq!(sent["messages"][0]["content"], "sys", "the system message stays text");
    }

    #[test]
    fn a_message_without_pictures_is_exactly_what_it_always_was() {
        let (base, rx) = stand_in();
        ask(&openai_spec(&base, true), &[Msg::user("hi"), Msg::assistant("hello")]).unwrap();
        let sent = rx.recv().unwrap();
        assert_eq!(sent["messages"][1], json!({"role": "user", "content": "hi"}));
        assert_eq!(sent["messages"][2], json!({"role": "assistant", "content": "hello"}));
    }

    #[test]
    fn a_model_that_cannot_see_is_told_so_and_never_sent_a_picture() {
        let (base, rx) = stand_in();
        ask(&openai_spec(&base, false), &[Msg::user("what is this?").with_images(vec![photo()])])
            .unwrap();
        let sent = rx.recv().unwrap();
        let text = sent["messages"][1]["content"].as_str().expect("plain text, not a parts list");
        assert!(
            text.starts_with("what is this?")
                && text.contains("1 photo was attached, but you cannot see images"),
            "{text}"
        );
        assert!(!sent.to_string().contains("base64"), "no picture left the runtime");

        // the same for the local model
        let (base, rx) = stand_in();
        let local = LocalModel { base_url: Some(base), model: String::new() };
        complete(
            &ModelSpec::Local,
            &local,
            "sys",
            &[Msg::user("x").with_images(vec![photo(), photo()])],
            &Mutex::new(0),
        )
        .unwrap();
        let text = rx.recv().unwrap()["messages"][1]["content"].as_str().unwrap().to_string();
        assert!(
            text.contains("2 photos were attached") && text.contains("describe them in words"),
            "{text}"
        );
    }

    #[test]
    fn in_a_fallback_chain_each_model_decides_for_itself() {
        let (seeing, rx_seeing) = stand_in();
        let (blind, rx_blind) = stand_in();
        // the first (blind) model answers: it must have been sent text only
        let chain = ModelSpec::Fallback {
            models: vec![openai_spec(&blind, false), openai_spec(&seeing, true)],
        };
        ask(&chain, &[Msg::user("x").with_images(vec![photo()])]).unwrap();
        assert!(!rx_blind.recv().unwrap().to_string().contains("base64"));
        assert!(rx_seeing.try_recv().is_err(), "the second was never needed");
    }

    #[test]
    fn anthropic_gets_the_pictures_first_as_image_blocks() {
        let body = anthropic::body(&Request::new(
            "m",
            &wire(
                &[Msg::user("what is this?").with_images(vec![photo()]), Msg::assistant("a plate")],
                true,
            ),
        ));
        let m = &body["messages"];
        assert_eq!(m[0]["content"][0]["type"], "image");
        assert_eq!(
            m[0]["content"][0]["source"],
            json!({"type": "base64", "media_type": "image/png", "data": "iVBORw=="})
        );
        assert_eq!(m[0]["content"][1], json!({"type": "text", "text": "what is this?"}));
        assert_eq!(m[1], json!({"role": "assistant", "content": "a plate"}));
    }

    #[test]
    fn a_picture_is_never_printed_as_its_bytes() {
        let m = Msg::user("x")
            .with_images(vec![Image { media_type: "image/jpeg".into(), data: vec![0; 5000] }]);
        let shown = format!("{m:?}");
        assert!(shown.contains("Image(image/jpeg, 5000 bytes)") && shown.len() < 200, "{shown}");
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::*;

    fn mock(replies: &[&str]) -> ModelSpec {
        ModelSpec::Mock { replies: replies.iter().map(|r| r.to_string()).collect() }
    }

    fn ask(spec: &ModelSpec) -> Result<Reply, String> {
        complete(
            spec,
            &LocalModel::default(),
            "sys",
            &[Msg::user("hi".to_string())],
            &Mutex::new(0),
        )
    }

    #[test]
    fn the_first_model_that_answers_is_used_and_all_failing_is_an_error_naming_each() {
        // the first has no replies (it errors), the second answers
        let chain = ModelSpec::Fallback { models: vec![mock(&[]), mock(&["second"])] };
        // each mock shares the cursor of the call, so the second starts at 0 too
        assert_eq!(ask(&chain).unwrap().text, "second");
        let dead = ModelSpec::Fallback {
            models: vec![
                ModelSpec::OpenAi {
                    base_url: "http://127.0.0.1:1".into(),
                    model: "m".into(),
                    api_key_env: String::new(),
                    api_key_file: String::new(),
                    service: String::new(),
                    vision: false,
                },
                ModelSpec::Local,
            ],
        };
        let e = ask(&dead).unwrap_err();
        assert!(e.starts_with("no model answered: ") && e.contains(" | "), "{e}");
    }

    #[test]
    fn a_key_file_is_read_trimmed_and_a_missing_or_empty_one_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("k");
        std::fs::write(&f, "  sk-test\n").unwrap();
        assert_eq!(key_from_file(f.to_str().unwrap()).unwrap(), "sk-test");
        std::fs::write(&f, "\n").unwrap();
        assert!(key_from_file(f.to_str().unwrap()).unwrap_err().contains("empty"));
        assert!(key_from_file("/nonexistent/key").is_err());
    }
}
