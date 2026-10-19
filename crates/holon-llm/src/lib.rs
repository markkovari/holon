//! Anthropic's Messages API and the OpenAI-compatible chat API, as data.
//!
//! One request is written once ([`Request`]) and encoded for either dialect ([`anthropic::body`],
//! [`openai::body`]); a reply is read back into one shape ([`Reply`]) from a whole response
//! ([`anthropic::parse`], [`openai::parse`]) or assembled from a stream ([`anthropic::Stream`],
//! [`openai::Stream`], fed by [`sse::take_frames`]).
//!
//! No I/O and no async: this crate does not know how bytes reach a server. A native daemon sends them
//! with `reqwest`, a wasm32-wasip2 component with `wasi:http`, and both build the bytes here. What it
//! does NOT decide, because the callers differ: URLs and headers (the endpoint is the caller's base
//! plus a path, and keys are the caller's secrets), retries, what an HTTP error means, what to do with
//! an empty reply, and what a model costs.

pub mod anthropic;
mod model;
pub mod openai;
pub mod sse;

pub use model::{
    parse_args, Call, Image, Message, ParseError, Part, Reply, Request, Role, Tool, ToolResult,
    Usage,
};
