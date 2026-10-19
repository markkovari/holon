//! The agent board: a live tree of the agents that are working, what each is doing with which model
//! and tools, what it has cost, and the ones that are waiting for a person (docs/adr/0119).
//!
//! Three layers, so the one that is hard to see (the tree) can be tested without a window and the
//! one that is easy to see (the pixels) can be changed without touching it:
//!
//! * [`tree`] is the model: nodes keyed by id, built by feeding it the runtime's [`Event`]s. Pure.
//! * [`theme`] and [`icons`] are what it is drawn with.
//! * [`view`] is the GPUI view: layout, the cards, and the motion.
//!
//! [`Event`]: agent_runtime::agent::Event

pub mod demo;
pub mod icons;
pub mod theme;
pub mod tree;
pub mod view;

pub use tree::{AgentTree, Kind, Node, Pending, Status, Summary};
pub use view::{Actions, Board};
