//! `ticket-triage-domain` — the goal. Nothing below is implemented yet: every route answers
//! 501 until `handle` is written.
//!
//! See the goal text in `.comp/goals/ticket-triage-domain.toml` for the routes, the storage
//! shape, the row-level policy:guard rule, and the one extra capability this
//! app composes.

use crate::{Reply, Route};
use crate::bindings::auth::identity::authorizer;
use crate::bindings::auth::identity::types::Permission;
use crate::bindings::audit::log::recorder as audit;
use crate::bindings::audit::log::types::Event;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::policy::guard::guard::Attr;
use crate::bindings::records::store::store as records;
use crate::bindings::search::index::index as search;
use crate::bindings::wasi::http::types::Method;
use serde_json::Value;

#[allow(unused)]
pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let _ = (method, route, body);
    unimplemented!("ticket-triage-domain: no route implemented yet")
}
