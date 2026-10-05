//! The autonomous half of a Holon agent. See `runtime.rs` for the entry point
//! and `agent.rs` for the loop.

pub mod agent;
pub mod bus;
pub mod cron;
pub mod html;
pub mod kv;
pub mod model;
pub mod otlp;
pub mod runtime;
pub mod server;
pub mod spec;
pub mod store;
pub mod trace;

pub use runtime::{Config, Runtime};
pub use spec::{AgentSpec, Capability, ModelSpec, Trigger};
