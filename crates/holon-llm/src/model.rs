use std::fmt;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub output: String,
    pub is_error: bool,
}

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

/// No system role: one dialect lists the system prompt and the other puts it beside, so it is `Request::system`.
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

/// Order is kept as given: dialects disagree on whether pictures go before or after the words.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Text(String),
    Image(Image),
    ToolUse(Call),
    /// Travels in a user message.
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

#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// `input` excludes cached tokens in both dialects (OpenAI's `prompt_tokens` includes them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    pub fn total_input(&self) -> u64 {
        self.input.saturating_add(self.cache_read).saturating_add(self.cache_write)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reply {
    /// No tool calls or thinking mixed in.
    pub text: String,
    pub calls: Vec<Call>,
    /// The provider's own word (`end_turn`, `length`, …), not normalised.
    pub finish: Option<String>,
    pub model: String,
    pub usage: Usage,
}

impl Reply {
    pub fn hit_token_limit(&self) -> bool {
        matches!(self.finish.as_deref(), Some("max_tokens") | Some("length"))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParseError {
    Malformed(String),
    /// Also sent under a success status (Anthropic when it throttles) or mid-stream.
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

pub struct Request<'a> {
    pub model: &'a str,
    pub system: Option<&'a str>,
    pub messages: &'a [Message],
    pub tools: &'a [Tool],
    /// Anthropic requires it, so its encoder falls back to [`crate::anthropic::DEFAULT_MAX_TOKENS`].
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub stop: &'a [String],
    /// OpenAI only; Anthropic's API has none.
    pub seed: Option<u64>,
    pub stream: bool,
    /// Anthropic only: cache breakpoint on the system block plus automatic caching of the tail.
    pub cache: bool,
    /// Anthropic only: content always as blocks, for stand-ins that read only blocks (`tools/claude-shim.mjs`).
    pub blocks: bool,
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
            blocks: false,
        }
    }
}

/// Unparseable JSON is kept under `_raw` so the tool fails on it visibly.
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
