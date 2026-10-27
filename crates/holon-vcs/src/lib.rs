//! An agent-native code store (ADR-0099).

pub mod engine;
pub mod error;
pub mod extract;
pub mod git;
pub mod graph;
pub mod mem;
pub mod model;
pub mod oplog;
pub mod order;
pub mod patch;
pub mod recovery;
pub mod store;
pub mod wire;

#[cfg(feature = "native")]
pub mod nats;
#[cfg(feature = "native")]
pub mod s3;
#[cfg(feature = "native")]
pub mod surreal;

pub use engine::Engine;
pub use error::VcsError;

pub type MemEngine = Engine<mem::MemBlobs, mem::MemPointers, mem::MemGraph, mem::MemOpLog>;

pub fn mem_engine() -> MemEngine {
    Engine::new(mem::MemBlobs::new(), mem::pointers(), mem::MemGraph::new(), mem::oplog())
}
