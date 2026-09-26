//! `holon-park` — a durable record of an outstanding call, instead of a
//! thread blocked on its answer (ADR-0100).
//!
//! An agent turn dispatches an outbound call and then has nothing to do until
//! the answer shows up. `park` writes that down before the call is
//! dispatched; `wake` closes it when the answer lands — a webhook, a poller,
//! another daemon noticing something finished; `pending` and `take-ready` are
//! how whatever resumes the turn finds out.
//!
//! The WIT contract is `wit/park/park.wit` (`holon:park/lot`); [`model`]
//! mirrors its types one to one, [`ticket`] is the content-addressed ticket id
//! that makes `park` idempotent, [`store`] is the one storage trait
//! [`engine::Engine`] needs, and [`mem`] is its in-memory implementation.
//!
//! What is here now is the engine, not the service: there is no `comp-park`
//! daemon, no `PARK_WAKE` JetStream stream, and no NATS-backed `ParkStore`
//! yet. Those are next, the same order holon-vcs was built in — ADR, WIT,
//! workspace, engine (tested in memory), THEN wired to real storage and
//! served over HTTP (ADR-0100).

pub mod engine;
pub mod error;
pub mod mem;
pub mod model;
pub mod store;
pub mod ticket;
pub mod wire;

#[cfg(feature = "native")]
pub mod nats;

pub use engine::Engine;
pub use error::ParkError;

/// An engine entirely in memory.
pub type MemEngine = Engine<mem::MemParkStore>;

/// A fresh in-memory engine.
pub fn mem_engine() -> MemEngine {
    Engine::new(mem::MemParkStore::new())
}
