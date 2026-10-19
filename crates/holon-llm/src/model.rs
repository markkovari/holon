//! The provider-neutral shapes both dialects are written from and read into.

use std::fmt;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tool call the model made.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// What a tool call came to, handed back to the model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub output: String,
    pub is_error: bool,
}

/// A picture that goes to the model with a message.
#[derive(Clone, PartialEq)]
pub struct Image {
    /// `image/jpeg`, `image/png`, `image/webp` or `image/gif`.
    pub media_type: String,
    pub data: Vec<u8>,
}

// Never the bytes: a message is logged and compared in tests, and a photo is megabytes.
impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Image({}, {} bytes)", self.media_type, self.data.len())
    }
}

impl Image {
    pub fn base64(&self) -> String {
        B64.encode(&self.data)
    }

    /// `data:image/png;base64,…`, the form OpenAI-compatible servers take.
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, self.base64())
    }
}

/// Who spoke. The system prompt is not a message: it is `Request::system`, because one dialect puts it
/// in the list and the other beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// One piece of a message. Parts keep the order the caller gave them: some dialects want the pictures
/// before the words about them and some after, and that is the caller's to say.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Text(String),
    Image(Image),
    /// An assistant's tool call.
    ToolUse(Call),
    /// A tool's answer, which travels in a user message.
    ToolResult(ToolResult),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl Message {
    pub fn new(role: Role, parts: Vec<Part>) -> Self {
        Self { role, parts }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::new(Role::User, vec![Part::Text(text.into())])
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self::new(Role::Assistant, vec![Part::Text(text.into())])
    }
}

/// A tool the model may call: a name, what it is for, and the JSON Schema of its input.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// Token counts of one reply, priced apart by the caller: pricing is not the wire's business.
///
/// `input` is what was read fresh. The prompt as a whole is `total_input()`: Anthropic reports the
/// cached prefix beside `input_tokens`, OpenAI reports cached tokens as a subset of `prompt_tokens`,
/// and both are normalised here to the same four numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    /// Every prompt token, however it was billed.
    pub fn total_input(&self) -> u64 {
        self.input.saturating_add(self.cache_read).saturating_add(self.cache_write)
    }
}

/// What a model said, in either dialect.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reply {
    /// All the text, in order, with nothing else mixed in (no tool calls, no thinking).
    pub text: String,
    pub calls: Vec<Call>,
    /// The provider's own reason it stopped (`end_turn`, `max_tokens`, `tool_use`, `stop`, `length`,
    /// `tool_calls`, …), as it said it.
    pub finish: Option<String>,
    pub model: String,
    pub usage: Usage,
}

impl Reply {
    /// The model ran out of tokens, in either dialect's word for it.
    pub fn hit_token_limit(&self) -> bool {
        matches!(self.finish.as_deref(), Some("max_tokens") | Some("length"))
    }
}

/// Why a response could not be read.
#[derive(Clone, Debug, PartialEq)]
pub enum ParseError {
    /// Not the JSON the dialect promises.
    Malformed(String),
    /// The provider answered with an error, possibly under a success status (Anthropic does when it
    /// throttles) or in the middle of a stream.
    Api(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Malformed(m) => write!(f, "{m}"),
            ParseError::Api(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// One request, written once and encoded for either dialect.
pub struct Request<'a> {
    pub model: &'a str,
    pub system: Option<&'a str>,
    pub messages: &'a [Message],
    pub tools: &'a [Tool],
    /// Anthropic requires it (a request without is a 400), so its encoder falls back to
    /// [`crate::anthropic::DEFAULT_MAX_TOKENS`]; OpenAI leaves it to the server when unset.
    pub max_tokens: Option<u32>,
    /// A JSON number as the server will read it: pass `0.7`, not a fraction of a thousand.
    pub temperature: Option<f64>,
    pub stop: &'a [String],
    /// OpenAI only; Anthropic's API has none.
    pub seed: Option<u64>,
    pub stream: bool,
    /// Anthropic only: mark the system block as a cache breakpoint and ask for automatic caching of
    /// the growing tail. The system prompt is the part identical across every call.
    pub cache: bool,
}

impl<'a> Request<'a> {
    pub fn new(model: &'a str, messages: &'a [Message]) -> Self {
        Self {
            model,
            system: None,
            messages,
            tools: &[],
            max_tokens: None,
            temperature: None,
            stop: &[],
            seed: None,
            stream: false,
            cache: false,
        }
    }
}

/// Tool arguments as the model sent them. Unparseable JSON is kept, as a string under `_raw`, so the
/// tool fails on it visibly instead of silently.
pub fn parse_args(json: &str) -> Value {
    if json.trim().is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(json).unwrap_or_else(|_| serde_json::json!({ "_raw": json }))
}

pub(crate) fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

pub(crate) fn u64_at(v: &Value, pointer: &str) -> u64 {
    v.pointer(pointer).and_then(Value::as_u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_is_never_printed_as_its_bytes() {
        let m = Message::new(
            Role::User,
            vec![Part::Image(Image { media_type: "image/jpeg".into(), data: vec![0; 5000] })],
        );
        let shown = format!("{m:?}");
        assert!(shown.contains("Image(image/jpeg, 5000 bytes)") && shown.len() < 200, "{shown}");
    }

    #[test]
    fn an_image_is_base64_and_a_data_url() {
        let i = Image { media_type: "image/png".into(), data: vec![0x89, b'P', b'N', b'G'] };
        assert_eq!(i.base64(), "iVBORw==");
        assert_eq!(i.data_url(), "data:image/png;base64,iVBORw==");
    }

    #[test]
    fn the_prompt_is_everything_read_however_it_was_billed() {
        let u = Usage { input: 100, output: 9, cache_read: 30, cache_write: 5 };
        assert_eq!(u.total_input(), 135);
        assert_eq!(
            Usage { input: u64::MAX, cache_read: 1, ..Default::default() }.total_input(),
            u64::MAX
        );
    }

    #[test]
    fn running_out_of_tokens_is_one_question_with_two_answers() {
        let r = |f: &str| Reply { finish: Some(f.into()), ..Default::default() };
        assert!(r("max_tokens").hit_token_limit() && r("length").hit_token_limit());
        assert!(!r("end_turn").hit_token_limit() && !Reply::default().hit_token_limit());
    }

    #[test]
    fn unparseable_tool_arguments_are_kept_not_dropped() {
        assert_eq!(parse_args(""), serde_json::json!({}));
        assert_eq!(parse_args("  "), serde_json::json!({}));
        assert_eq!(parse_args(r#"{"a":1}"#), serde_json::json!({"a": 1}));
        assert_eq!(parse_args("{\"a\":"), serde_json::json!({"_raw": "{\"a\":"}));
    }
}
