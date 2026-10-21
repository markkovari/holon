pub mod admin;
pub mod agent;
pub mod attach;
pub mod bus;
pub mod clock;
pub mod connector;
pub mod cron;
pub mod embed;
pub mod food;
pub mod google;
pub mod html;
pub mod jev;
pub mod keys;
pub mod kv;
pub mod lazy;
pub mod model;
pub mod otlp;
pub mod projects;
pub mod runtime;
pub mod server;
pub mod session;
pub mod spec;
pub mod speech;
pub mod store;
pub mod trace;

pub use runtime::{Config, Runtime};
pub use spec::{AgentSpec, Capability, ModelSpec, Trigger};

#[cfg(test)]
pub(crate) mod testutil {
    /// Not a predictable path under the shared temp dir: that invites an insecure-temp-file race.
    pub fn dir(prefix: &str) -> std::path::PathBuf {
        tempfile::Builder::new()
            .prefix(&format!("ar-{prefix}-"))
            .tempdir()
            .expect("creating a temp dir")
            .keep()
    }
}
