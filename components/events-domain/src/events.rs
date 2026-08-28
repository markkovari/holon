//! `events` — the organizer's half: creating, listing, amending and cancelling.
//!
//! The one thing here that is not CRUD is the pair of counts on a single event.
//! They come from `quota:meter`, not from counting tickets, and that is a contract
//! decision rather than a convenience: `tickets` is a different part, the tickets
//! collection may not exist yet when this is judged, and the meter is the thing
//! that actually decides whether the next claim succeeds. Counting a collection
//! would produce a number that agrees with it most of the time.

use serde_json::json;

use crate::bindings::blob::store::blobstore as blobs;
use crate::bindings::quota::meter::meter as quota;
use crate::bindings::upload::policy::gate as policy;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::store::{find_by_str, load, quota_subject, save, with_id, PAGE, QUOTA_PERIOD};
use crate::{has_role, require, Reply, Route};

/// `claimed` and `remaining`, read off the meter that governs claiming.
fn counts(event_id: &str, capacity: u64) -> (u64, u64) {
    match quota::peek(&quota_subject(event_id), capacity, QUOTA_PERIOD) {
        Ok(b) => (b.used, b.remaining),
        // A pool nothing has reserved against yet has no entry; that is an empty
        // event, not an error.
        Err(_) => (0, capacity),
    }
}

/// Amending and cancelling are the owner's, or an admin's.
fn may_change(doc: &serde_json::Value, subject: &str, is_admin: bool) -> bool {
    is_admin || doc["organizer"].as_str() == Some(subject)
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "events"]) => create(route, body),
        (Method::Get, ["api", "events"]) => list(route),
        (Method::Get, ["api", "events", id]) => one(route, id),
        (Method::Patch, ["api", "events", id]) => patch(route, id, body),
        (Method::Delete, ["api", "events", id]) => cancel(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

fn create(route: &Route, body: &str) -> Reply {
    let p = match require(route, "event", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let Ok(input) = serde_json::from_str::<serde_json::Value>(body) else {
        return Reply::err(400, "malformed_body");
    };

    let title = input["title"].as_str().unwrap_or_default().trim().to_string();
    let starts_at = input["starts_at"].as_str().unwrap_or_default().trim().to_string();
    // `as_u64` on a JSON number that is negative or fractional is None, which is the
    // same refusal as absent — all three are "not a capacity".
    let capacity = input["capacity"].as_u64().unwrap_or(0);
    if title.is_empty() || starts_at.is_empty() || capacity < 1 {
        return Reply::err(400, "invalid");
    }

    let mut doc = json!({
        "title": title,
        "starts_at": starts_at,
        "capacity": capacity,
        "organizer": p.subject,
        "state": "open",
    });
    // Optional, and absent rather than empty when it is not given: a caller reading
    // `"description": ""` cannot tell "nobody wrote one" from "somebody cleared it".
    if let Some(d) = input["description"].as_str().map(str::trim).filter(|d| !d.is_empty()) {
        doc["description"] = json!(d);
    }
    match records::create(
        "events",
        &doc.to_string(),
        &["state".to_string(), "organizer".to_string()],
    ) {
        Ok(e) => Reply::json(201, with_id(&e)),
        Err(_) => Reply::err(500, "store_failed"),
    }
}

fn list(route: &Route) -> Reply {
    if let Err(r) = require(route, "event", "read") {
        return r;
    }
    let filter = route.param("state");
    let entries = if filter.is_empty() {
        records::list_records("events", PAGE, "").map(|p| p.entries).unwrap_or_default()
    } else {
        find_by_str("events", "state", &filter)
    };
    // Every entry carries its id: a list nothing can be selected from is not a list.
    let events: Vec<_> = entries.iter().map(with_id).collect();
    Reply::json(200, json!({ "events": events }))
}

fn one(route: &Route, id: &str) -> Reply {
    if let Err(r) = require(route, "event", "read") {
        return r;
    }
    let (entry, _) = match load("events", id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mut doc = with_id(&entry);
    let capacity = doc["capacity"].as_u64().unwrap_or(0);
    let (claimed, remaining) = counts(id, capacity);
    doc["claimed"] = json!(claimed);
    doc["remaining"] = json!(remaining);
    Reply::json(200, doc)
}

fn patch(route: &Route, id: &str, body: &str) -> Reply {
    let p = match require(route, "event", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (entry, mut doc) = match load("events", id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if !may_change(&doc, &p.subject, has_role(&p, "admin")) {
        return Reply::err(403, "forbidden");
    }
    let Ok(input) = serde_json::from_str::<serde_json::Value>(body) else {
        return Reply::err(400, "malformed_body");
    };

    // Only these three. `organizer` and `state` are not amendable — one is identity
    // and the other is what DELETE is for, and a PATCH that could set either would
    // let a caller hand an event away or resurrect a cancelled one.
    for key in ["title", "starts_at", "capacity", "description"] {
        if let Some(v) = input.get(key) {
            if key == "capacity" && v.as_u64().unwrap_or(0) < 1 {
                return Reply::err(400, "invalid");
            }
            // An explicit null CLEARS an optional field. Without this the only way
            // to remove a description would be to delete the event.
            if key == "description" && (v.is_null() || v.as_str() == Some("")) {
                doc.as_object_mut().map(|m| m.remove("description"));
                continue;
            }
            doc[key] = v.clone();
        }
    }
    if let Err(r) = save("events", &entry, &doc) {
        return r;
    }
    doc["id"] = json!(entry.id);
    Reply::json(200, doc)
}

fn cancel(route: &Route, id: &str) -> Reply {
    let p = match require(route, "event", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (entry, mut doc) = match load("events", id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if !may_change(&doc, &p.subject, has_role(&p, "admin")) {
        return Reply::err(403, "forbidden");
    }
    // SOFT: tickets already issued stay readable, and their holders still have
    // something to show at a door that has moved rather than vanished.
    doc["state"] = json!("cancelled");
    if let Err(r) = save("events", &entry, &doc) {
        return r;
    }
    Reply::no_content()
}

/// The container every event poster lives in. One container, keyed by event id, so
/// deleting an event's image needs no index and cannot reach another event's.
const IMAGES: &str = "event-images";

/// `POST|PUT /api/events/{id}/image` and `GET /api/events/{id}/image`.
///
/// The bytes go to `blob-store` and the record keeps only the content type. A JSON
/// document is the wrong place for a JPEG: base64 is a third larger than what it
/// encodes, every read of the event pays for it, and `record-store` would be
/// indexing it.
pub fn image(
    method: &Method,
    route: &Route,
    id: &str,
    content_type: &str,
    bytes: Vec<u8>,
) -> Reply {
    match method {
        Method::Get => match blobs::get(IMAGES, id) {
            Ok(data) => {
                let ct = match load("events", id) {
                    Ok((_, ev)) => {
                        ev["image_type"].as_str().unwrap_or("application/octet-stream").to_string()
                    }
                    Err(_) => "application/octet-stream".to_string(),
                };
                Reply::raw(200, &ct, data)
            }
            Err(_) => Reply::err(404, "no_image"),
        },
        Method::Post | Method::Put => {
            let p = match require(route, "event", "write") {
                Ok(p) => p,
                Err(r) => return r,
            };
            let (entry, mut doc) = match load("events", id) {
                Ok(v) => v,
                Err(r) => return r,
            };
            if !may_change(&doc, &p.subject, has_role(&p, "admin")) {
                return Reply::err(403, "forbidden");
            }
            if bytes.is_empty() {
                return Reply::err(400, "empty_body");
            }
            // What counts as an image, and how big, is `upload-policy`'s answer from
            // `allowed-types` and `max-size` — not a match arm here. A content-type
            // allowlist written out in this file would be the fourth copy of one in
            // this repository, and the first that nothing tests.
            let ct = content_type.split(';').next().unwrap_or("").trim();
            if let Err(e) = policy::check(ct, bytes.len() as u64) {
                let code = match e {
                    policy::PolicyError::TypeNotAllowed(_) => "type_not_allowed",
                    policy::PolicyError::TooLarge(_) => "too_large",
                    _ => "rejected",
                };
                return Reply::err(415, code);
            }
            if blobs::put(IMAGES, id, &bytes, ct).is_err() {
                return Reply::err(500, "store_failed");
            }
            doc["image_type"] = json!(ct);
            if let Err(r) = save("events", &entry, &doc) {
                return r;
            }
            Reply::json(201, json!({ "id": id, "image_type": ct, "bytes": bytes.len() }))
        }
        Method::Delete => {
            let p = match require(route, "event", "write") {
                Ok(p) => p,
                Err(r) => return r,
            };
            let (entry, mut doc) = match load("events", id) {
                Ok(v) => v,
                Err(r) => return r,
            };
            if !may_change(&doc, &p.subject, has_role(&p, "admin")) {
                return Reply::err(403, "forbidden");
            }
            let _ = blobs::delete(IMAGES, id);
            doc.as_object_mut().map(|m| m.remove("image_type"));
            if let Err(r) = save("events", &entry, &doc) {
                return r;
            }
            Reply::no_content()
        }
        _ => Reply::err(404, "not_found"),
    }
}
