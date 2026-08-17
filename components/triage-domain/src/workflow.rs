//! The report lifecycle. **This file is the goal of the `workflow` part.**
//!
//! `CONTRACT.md` is the specification. `crate::Reply` is how you answer.
//!
//! ## The capability you must not reimplement
//!
//! `crate::bindings::fsm::workflow::engine` is a declarative state machine. The legal
//! moves are a DEFINITION you register once, not a ladder of
//! `if state == "open" && to == "triaged"`. An illegal move comes back as a typed
//! error carrying the current state, which is exactly what the contract's 409 needs to
//! report. The gate reads the compiled component's imports, so a hand-rolled
//! transition table fails it even if every response is correct.
//!
//! The surface, as wit-bindgen generates it in Rust:
//!
//!     use crate::bindings::fsm::workflow::engine as fsm;
//!
//!     fsm::define(name: &str, def: &fsm::Definition)            -> Result<(), fsm::FsmError>
//!     fsm::create_instance(machine: &str, instance: &str)       -> Result<fsm::Status, fsm::FsmError>
//!     fsm::get_status(machine: &str, instance: &str)            -> Result<fsm::Status, fsm::FsmError>
//!     fsm::can_fire(machine: &str, instance: &str, event: &str) -> Result<bool, fsm::FsmError>
//!     fsm::fire(machine: &str, instance: &str, event: &str)     -> Result<fsm::Status, fsm::FsmError>
//!     fsm::allowed_events(machine: &str, instance: &str)        -> Result<Vec<String>, fsm::FsmError>
//!
//!     pub struct Definition {
//!         pub states: Vec<String>,
//!         pub initial: String,
//!         pub transitions: Vec<Transition>,   // { event, source, target }
//!         pub terminal: Vec<String>,
//!     }
//!     pub struct Status { pub machine: String, pub instance: String,
//!                         pub state: String, pub done: bool, pub steps: u32 }
//!     pub enum FsmError { UnknownMachine, UnknownInstance,
//!                         IllegalTransition(String),   // carries the CURRENT state
//!                         InvalidDefinition(String), BackendUnavailable(String) }
//!
//! `define` is idempotent — registering the same machine again replaces it — so
//! calling it before you need it is safe and is the simplest way to be sure the
//! machine exists on a cold store. `create_instance` however RESETS an existing
//! instance to initial, so a report already in the machine must not be re-created.
//!
//! ## The part you own and the record you share
//!
//! The fsm holds the state. The report DOCUMENT in `records:store` also carries a
//! `state` field, because `digest` reads reports and must not need the fsm to know
//! what state one is in. Both have to move together: the contract says the document is
//! the readable copy and the fsm is the authority on whether a move is legal.
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    // Replace this. Every route in CONTRACT.md's `workflow` section, judged by real
    // HTTP requests against the running component.
    Reply::err(501, "not_implemented")
}
