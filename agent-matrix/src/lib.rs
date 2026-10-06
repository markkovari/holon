//! A Matrix bridge for `agent-runtime`: every agent is a Matrix user and every
//! project a Space. See `bridge.rs` for the model and the README for setup.

pub mod bridge;
pub mod commands;
pub mod config;
pub mod init;
pub mod matrix;
pub mod runtime;
pub mod serve;
pub mod state;

pub use bridge::Bridge;
pub use config::Config;
