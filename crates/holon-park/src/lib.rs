//! `holon-park` — a durable record of an outstanding call, instead of a thread
//! blocked on its answer (ADR-0100), and what a restarted turn may do about the
//! ones that never answered (ADR-0106).
//!
//! An agent turn dispatches an outbound call and then has nothing to do until
//! the answer shows up. `park` writes that down before the call is
//! dispatched; `wake` closes it when the answer lands — a webhook, a poller,
//! another daemon noticing something finished; `pending` and `take-ready` are
//! how whatever resumes the turn finds out. A call that is given up on without
//! an answer is `interrupt`ed, and the turn is told so rather than left to guess.
//!
//! The WIT contract is `wit/park/park.wit` (`holon:park/lot`); [`model`]
//! mirrors its types one to one, [`ticket`] is the content-addressed ticket id
//! that makes `park` idempotent, [`store`] is the one storage trait
//! [`engine::Engine`] needs, and [`mem`] is its in-memory implementation.
//! [`recover`] is the pass a sweep runs, [`resume`] is what it does with the result, [`kvstore`]
//! is the store over any compare-and-set keyvalue, [`wire`] is the JSON the park app speaks, and
//! [`serve`] answers it.
//!
//! The service around the engine is the park app (ADR-0132): `components/park-store` runs it on
//! its keyvalue and `components/park-gateway` serves it.

pub mod batch;
pub mod engine;
pub mod error;
pub mod group;
pub mod mem;
pub mod model;
pub mod recover;
pub mod resume;
pub mod serve;
pub mod store;
pub mod ticket;
pub mod wire;

pub mod kvstore;

pub use engine::Engine;
pub use error::ParkError;

pub type MemEngine = Engine<mem::MemParkStore>;

pub fn mem_engine() -> MemEngine {
    Engine::new(mem::MemParkStore::new())
}
