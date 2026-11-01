//! Conversational voice companion for Holon on macOS.

pub mod audio;
pub mod client;
pub mod persona;
pub mod session;
pub mod stt;
pub mod tts;
pub mod vad;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
