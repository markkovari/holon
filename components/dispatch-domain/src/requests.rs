//! Taking a service request in. **This file is the goal of the `requests` part.**
//!
//! Nothing here is implemented. `CONTRACT.md` is the specification — the document
//! shape, every route, every status code. Read it first.
//!
//! What this part owns:
//!
//!   * `POST /api/requests` — validate, mask the notes, refuse a duplicate, store it
//!   * `GET  /api/requests` — the list, filtered and sorted as the contract says
//!   * `GET  /api/requests/{id}` — one document
//!
//! `notes` is stored MASKED, via `pii:redact` — the raw text never reaches the
//! store, because the manifest is readable by anyone. Empty `kinds` is every kind,
//! which is what a note typed by a dispatcher needs:
//!
//!     use crate::bindings::pii::redact::redactor as pii;
//!
//!     pii::redact(text: &str, opts: &pii::Options) -> String
//!     pub struct Options { pub kinds: Vec<Kind> }   // EMPTY kinds = every kind
//!     pub enum Kind { Email, CreditCard, Ssn, Phone, Ip }
//!
//! The store, as wit-bindgen generates it — this is the exact surface, and guessing
//! it wrong is how earlier runs of other goals died:
//!
//!     use crate::bindings::records::store::store as records;
//!
//!     records::create(collection: &str, data: &str, index_fields: &[String]) -> Result<Entry, StoreError>
//!     records::get(collection: &str, id: &str)                               -> Result<Entry, StoreError>
//!     records::update(collection: &str, id: &str, data: &str, expected_revision: u64) -> Result<Entry, StoreError>
//!     records::query(collection: &str, filters: &[Filter], limit: u32)       -> Result<Vec<Entry>, StoreError>
//!
//!     struct Entry  { id: String, data: String, revision: u64, created: u64, … }
//!     struct Filter { field: String, value: String }
//!
//! `query` takes a list of equals-filters, ANDs them, and re-checks each candidate —
//! so the `state`+`engineer` list filter and the duplicate lookup are the same three
//! lines with a different filter vector.
//!
//! AND `query` WANTS THE JSON ENCODING OF THE VALUE, NOT THE VALUE. `record-store`
//! indexes the serialised form, so a string field `new` is indexed under `"new"`,
//! quotes included. A wrong query returns `Ok(vec![])`, which is indistinguishable
//! from an empty collection — it fails silently and your gate blames the wrong thing.
//!
//! `created` can only be filled in once the record exists: its timestamp is the
//! store's `created`, and this world imports no wall clock. RFC3339 UTC.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}
