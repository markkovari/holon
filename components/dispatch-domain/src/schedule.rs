//! Assigning a request and moving it. **This file is the goal of the `schedule` part.**
//!
//! Nothing here is implemented. `CONTRACT.md` is the specification — the lifecycle,
//! the roster, every route, every status code. Read it first.
//!
//! What this part owns:
//!
//!   * `POST /api/requests/{id}/assign` — the nearest engineer, written onto the document
//!   * `POST /api/requests/{id}/transition` — `depart`, `complete`, `cancel`
//!   * `GET  /api/queue` — what is still open, ordered by distance
//!
//! THE LIFECYCLE IS A DEFINITION, not a ladder of `if state == "new"`. Register it
//! once under the machine name `dispatch`, then drive each request as an instance
//! whose id is the request id. An illegal move comes back as
//! `IllegalTransition(String)` carrying the CURRENT state — which is exactly what
//! the contract's 409 body has to report, so you do not need to look it up yourself.
//!
//!     use crate::bindings::fsm::workflow::engine as fsm;
//!
//!     fsm::define(name: &str, def: &fsm::Definition)            -> Result<(), fsm::FsmError>
//!     fsm::create_instance(machine: &str, instance: &str)       -> Result<fsm::Status, fsm::FsmError>
//!     fsm::get_status(machine: &str, instance: &str)            -> Result<fsm::Status, fsm::FsmError>
//!     fsm::fire(machine: &str, instance: &str, event: &str)     -> Result<fsm::Status, fsm::FsmError>
//!
//!     pub struct Definition { pub states: Vec<String>, pub initial: String,
//!                             pub transitions: Vec<Transition>, pub terminal: Vec<String> }
//!     pub struct Transition { pub event: String, pub source: String, pub target: String }
//!     pub struct Status { pub machine: String, pub instance: String,
//!                         pub state: String, pub done: bool, pub steps: u32 }
//!
//! Read `fsm.wit` for how `define` and `create_instance` behave on a store that
//! already has the machine or the instance — it says, and the difference matters.
//!
//! THE DISTANCE IS NOT YOURS TO COMPUTE. `geo:resolve` is in your world:
//!
//!     use crate::bindings::geo::resolve::coords as geo;
//!
//!     geo::distance_meters(a: geo::Point, b: geo::Point) -> Result<f64, geo::GeoError>
//!     pub struct Point { pub lat: f64, pub lon: f64 }
//!
//! `manifest` imports the same component to answer `within_m`. A hand-rolled
//! haversine here is internally consistent, passes this part's gate, and disagrees
//! with your sibling's radius filter — a failure neither gate can see alone. The
//! composition gate can, and does.
//!
//! `distance_m` on the document is an INTEGER number of metres, rounded half-up.
//! The fsm instance is not the readable copy: the contract says the document
//! carries `state`, so a transition has to move both.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
