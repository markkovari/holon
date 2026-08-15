//! What the clinic did, as a report. **This file is the goal of the `reports` part.**
//!
//! `CONTRACT.md` is the specification. `crate::Reply` is how you answer.
//!
//! ## The capability you must not reimplement
//!
//! `crate::bindings::csv::codec::codec` formats CSV — `format(rows, dialect)`,
//! where a row is a list of fields. It already knows that a field containing a
//! comma, a quote or a newline has to be quoted and its quotes doubled. A pet in
//! this clinic is called `Rex, Jr.` precisely so that `join(",")` produces a file
//! with the wrong number of columns, and the gate reads the column count.
//!
//! The other two halves are already written and are yours to read from: owners,
//! pets and visits are all in `records:store` under those collection names.
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    // The stub. `e2e-reports.sh` fails against it, which is what lets the check
    // judge: a gate that passes before the work is done judges nothing.
    Reply::err(501, "not_implemented")
}
