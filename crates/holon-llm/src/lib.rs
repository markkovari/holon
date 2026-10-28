//! No I/O or async: a native daemon (reqwest) and a wasm32-wasip2 component (wasi:http) both build their bytes here.

pub mod anthropic;
pub mod coalesce;
pub mod gemini;
mod model;
pub mod openai;
pub mod sse;

pub use model::{
    parse_args, Call, Image, Message, ParseError, Part, Reply, Request, Role, Tool, ToolResult,
    Usage,
};
