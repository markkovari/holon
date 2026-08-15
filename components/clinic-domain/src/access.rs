//! Staff accounts and pet search. **This file is the goal of the `access-and-search` part.**
//!
//! `CONTRACT.md` is the specification. `crate::Reply` is how you answer.
//!
//! ## Do not write what is already here
//!
//! This part exists to prove a claim the other two halves cannot: that a goal is
//! finished by REACHING FOR CAPABILITIES rather than by writing more domain code.
//! Three of them are already in the world (see `wit/clinic.wit`) and are bound at
//! compose time by `bin/compose`:
//!
//! * `crate::bindings::auth::identity::accounts` — `register`, `login`,
//!   `verify_password`. Password hashing (argon2), salting and comparison happen
//!   in `auth-guard`. Do not hash anything in this file.
//! * `crate::bindings::auth::identity::session` — `issue` a token pair for a
//!   principal, `lookup` a session, `revoke` it. Do not invent a token format.
//! * `crate::bindings::search::index::index` — `index_doc`, `query` with a `mode`
//!   and tags, ranked hits back. Do not write a substring scan over every pet.
//!
//! A branch that reimplements any of the three can still pass the behavioural
//! checks, and `e2e-access.sh` therefore also asserts that the composed component
//! actually calls them.
//!
//! ## The one design decision left to you
//!
//! The index has to be populated from pets that the OTHER half creates, and this
//! part may not edit `src/owners.rs`. Indexing lazily on search (read `pets` from
//! the record store, feed the index, then query) is the obvious way and needs no
//! cooperation. If you would rather have `owners-and-pets` index on create, that
//! is a change to a file you do not own: write `CONTRACT-REQUEST.md` and ask.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    // The stub. `e2e-access.sh` fails against it, which is what makes the check
    // able to judge — a gate that passes on the base tree judges nothing.
    Reply::err(501, "not_implemented")
}
