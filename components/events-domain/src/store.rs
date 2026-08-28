//! The three things all four parts do to the store, in one place.
//!
//! Not an abstraction over `record-store` — a place for the decisions the CONTRACT
//! makes, so four files cannot make them four ways. `find_by` taking the serialised
//! form of a value is the sharpest: it returns `Ok(vec![])` for a wrong query, which
//! is indistinguishable from an empty collection, so a part that gets it wrong looks
//! like a part with nothing to show.

use serde_json::{json, Value};

use crate::bindings::records::store::store as records;
use crate::Reply;

/// The pool a ticket claim reserves against. One subject per event.
pub fn quota_subject(event_id: &str) -> String {
    format!("event:{event_id}")
}

/// A fixed pool rather than a rate: the period only has to outlive the event.
pub const QUOTA_PERIOD: u64 = 31_536_000;

/// `find_by` indexes the SERIALISED value, so a string field `open` lives under
/// `"open"` — quotes included. Every call goes through here so no part has to
/// remember, and the one that forgets gets an empty list rather than an error.
pub fn find_by_str(collection: &str, field: &str, value: &str) -> Vec<records::Entry> {
    let encoded = serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""));
    records::find_by(collection, field, &encoded).unwrap_or_default()
}

/// A stored document plus its id, which is what every route answers with. The id
/// lives beside the document rather than inside it — `record-store` owns it, and a
/// copy in the JSON is a second source of truth that can disagree.
pub fn with_id(entry: &records::Entry) -> Value {
    let mut doc: Value = serde_json::from_str(&entry.data).unwrap_or_else(|_| json!({}));
    if let Some(map) = doc.as_object_mut() {
        map.insert("id".into(), json!(entry.id));
    }
    doc
}

/// Read one document, or the reply that says it is not there.
pub fn load(collection: &str, id: &str) -> Result<(records::Entry, Value), Reply> {
    match records::get(collection, id) {
        Ok(e) => {
            let doc = serde_json::from_str(&e.data).unwrap_or_else(|_| json!({}));
            Ok((e, doc))
        }
        Err(_) => Err(Reply::err(404, "not_found")),
    }
}

/// Write a document back at the revision it was read at.
pub fn save(collection: &str, entry: &records::Entry, doc: &Value) -> Result<(), Reply> {
    match records::update(collection, &entry.id, &doc.to_string(), entry.revision) {
        Ok(_) => Ok(()),
        Err(_) => Err(Reply::err(500, "store_failed")),
    }
}

/// The listing cap. A demo store holds tens of documents; this exists so a route
/// cannot be turned into an unbounded read by a caller.
pub const PAGE: u32 = 200;
