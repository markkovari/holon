pub mod appservice;
pub mod bridge;
pub mod commands;
pub mod config;
pub mod http;
pub mod jev;
pub mod matrix;
pub mod md;
pub mod retire;
pub mod runtime;
pub mod scheduled;
pub mod setup;
pub mod state;

pub use bridge::Bridge;
pub use config::Config;

// The test-only reqwest `Http` (tests/reqwest_http.rs) names the crate as an outside user does.
#[cfg(test)]
extern crate self as agent_matrix;
