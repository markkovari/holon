//! Taking a defect report in. **This file is the goal of the `intake` part.**
//!
//! `CONTRACT.md` is the specification. `crate::Reply` is how you answer.
//!
//! ## The capability you must not reimplement
//!
//! `crate::bindings::pii::redact::redactor` masks personally-identifiable
//! information in free text. A reporter pastes their email and phone number into a
//! bug report constantly, and that text goes on to a digest anyone can read — so the
//! body is stored MASKED. The scanners are Luhn-checked and regex-free and they
//! already exist; hand-rolling an `@`-finder is how this part fails a gate while
//! answering every request correctly, because the gate reads which capabilities the
//! compiled component actually calls.
//!
//! The surface, as wit-bindgen generates it in Rust:
//!
//!     use crate::bindings::pii::redact::redactor as pii;
//!
//!     pii::redact(text: &str, opts: pii::Options) -> String
//!     pii::detect(text: &str, opts: pii::Options) -> Vec<pii::Finding>
//!
//!     pub struct Options { pub kinds: Vec<Kind> }   // EMPTY kinds = all of them
//!     pub enum Kind { Email, CreditCard, Ssn, Phone, Ip }
//!     pub struct Finding { pub kind: Kind, pub start: u32, pub length: u32 }
//!
//! ## `find_by` wants the JSON ENCODING of the value
//!
//! `records:store` indexes the serialised form, so a string field `billing` is
//! indexed under `"billing"` — quotes included. `find_by(.., "billing")` therefore
//! matches nothing and returns `Ok(vec![])`: an empty result and a wrong query are
//! indistinguishable from the caller. Pass `serde_json::to_string(&value)`.
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    // Replace this. Every route in CONTRACT.md's `intake` section, judged by real
    // HTTP requests against the running component.
    Reply::err(501, "not_implemented")
}
