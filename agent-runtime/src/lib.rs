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

#[cfg(test)]
pub(crate) mod testutil {
    /// A uniquely named, securely created directory for one test. (`tempfile`
    /// creates it 0700 with a random name; a predictable path under the shared
    /// temp dir invites the classic insecure-temp-file race.)
    pub fn dir(prefix: &str) -> std::path::PathBuf {
        tempfile::Builder::new()
            .prefix(&format!("ar-{prefix}-"))
            .tempdir()
            .expect("creating a temp dir")
            .keep()
    }
}

pub use runtime::{Config, Runtime};
pub use spec::{AgentSpec, Capability, ModelSpec, Trigger};

#[cfg(test)]
pub(crate) mod testutil {
    /// A uniquely named, securely created directory for one test. (`tempfile`
    /// creates it 0700 with a random name; a predictable path under the shared
    /// temp dir invites the classic insecure-temp-file race.)
    pub fn dir(prefix: &str) -> std::path::PathBuf {
        tempfile::Builder::new()
            .prefix(&format!("ar-{prefix}-"))
            .tempdir()
            .expect("creating a temp dir")
            .keep()
    }
}
