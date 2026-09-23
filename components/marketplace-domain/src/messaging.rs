//! Part 5 — inquiries and threaded messages. `CONTRACT.md`'s "Part 5 —
//! `messaging.rs`: inquiries and messages" section.
//!
//! A buyer opens an inquiry on a listing; this part reads the `listings`
//! collection `catalog.rs` writes to DIRECTLY (parts meet in `records:store`,
//! never by calling each other's Rust) — both to 404 a missing/delisted
//! listing and to copy the listing's `vendor` onto the inquiry at creation
//! time, the same "copy at creation" discipline `orders.items[].unit_price`
//! follows.

use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{audit, introspect, is_admin, Reply, Route};
use serde_json::{json, Value};

guestauth::guest_entries_json!();

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "listings", id, "inquiries"]) => open_inquiry(route, id, body),
        (Method::Get, ["api", "inquiries"]) => list_inquiries(route),
        (Method::Get, ["api", "inquiries", id, "messages"]) => list_messages(route, id),
        (Method::Post, ["api", "inquiries", id, "messages"]) => post_message(route, id, body),
        _ => Reply::err(404, "not_found"),
    }
}

/// The `"body"` field of a request body, `""` when it is absent or the JSON
/// doesn't parse — the only thing either write route takes from a request,
/// and nothing here is worth refusing with a 400 over.
fn body_field(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v.get("body").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default()
}

/// The stored inquiry document, or `None` when there is no such inquiry (an
/// unreadable one is the same 404 from a client's side).
fn inquiry_doc(id: &str) -> Option<Value> {
    let entry = records::get("inquiries", id).ok()?;
    Some(serde_json::from_str::<Value>(&entry.data).unwrap_or_else(|_| json!({})))
}

/// Either party to the inquiry — its `buyer` or its `vendor` — or an admin.
/// Anyone else is the `403` the thread routes answer.
fn is_party(admin: bool, subject: &str, inquiry: &Value) -> bool {
    admin
        || inquiry.get("buyer").and_then(Value::as_str) == Some(subject)
        || inquiry.get("vendor").and_then(Value::as_str) == Some(subject)
}

/// A stored document with its own store id merged in — the shape the other
/// parts' create routes answer with.
fn with_id(data: &str, id: &str) -> Value {
    let mut v: Value = serde_json::from_str(data).unwrap_or_else(|_| json!({}));
    if let Value::Object(ref mut map) = v {
        map.insert("id".to_string(), json!(id));
    }
    v
}

/// `POST /api/listings/{id}/inquiries` `{"body"}` — creates the `inquiries`
/// document (`vendor` copied off the listing) plus its first `messages`
/// document. `404` if the listing is missing OR delisted: a delisted listing
/// "cannot be ordered or messaged about".
fn open_inquiry(route: &Route, listing_id: &str, raw: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let listing = match records::get("listings", listing_id) {
        Ok(entry) => entry,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let listing_doc: Value = serde_json::from_str(&listing.data).unwrap_or_else(|_| json!({}));
    if listing_doc.get("status").and_then(Value::as_str) != Some("active") {
        return Reply::err(404, "not_found");
    }
    let vendor = listing_doc.get("vendor").and_then(Value::as_str).unwrap_or("").to_string();
    let inquiry_data = json!({
        "listing_id": listing_id,
        "buyer": &principal.subject,
        "vendor": vendor,
    })
    .to_string();
    let inquiry = match records::create(
        "inquiries",
        &inquiry_data,
        &["buyer".to_string(), "vendor".to_string()],
    ) {
        Ok(entry) => entry,
        Err(_) => return Reply::err(500, "store_error"),
    };
    let message_data = json!({
        "inquiry_id": &inquiry.id,
        "sender": &principal.subject,
        "body": body_field(raw),
    })
    .to_string();
    if records::create("messages", &message_data, &["inquiry_id".to_string()]).is_err() {
        return Reply::err(500, "store_error");
    }
    audit("inquiry.create", "allow", &principal.subject, &inquiry.id);
    Reply::json(201, with_id(&inquiry.data, &inquiry.id))
}

/// `GET /api/inquiries` — admin sees all; everyone else sees the inquiries
/// they are a party to. The store is asked for BOTH indexed fields rather
/// than branching on the caller's role: a buyer has no rows under `vendor`
/// and a vendor none under `buyer`, so the union is exactly each one's scope
/// (and a caller who is somehow both loses nothing).
fn list_inquiries(route: &Route) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let entries = if is_admin(&principal) {
        match crate::list_all("inquiries") {
            Ok(entries) => entries,
            Err(_) => return Reply::err(500, "store_error"),
        }
    } else {
        let subject = json!(&principal.subject).to_string();
        let mut found = records::find_by("inquiries", "buyer", &subject).unwrap_or_default();
        let mut as_vendor = records::find_by("inquiries", "vendor", &subject).unwrap_or_default();
        found.append(&mut as_vendor);
        found
    };
    Reply::json(200, json!({"inquiries": entries_json(&entries)}))
}

/// `GET /api/inquiries/{id}/messages` — either party or an admin, oldest
/// first, `403` for anyone else.
fn list_messages(route: &Route, inquiry_id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let inquiry = match inquiry_doc(inquiry_id) {
        Some(doc) => doc,
        None => return Reply::err(404, "not_found"),
    };
    if !is_party(is_admin(&principal), &principal.subject, &inquiry) {
        audit("message.list", "deny", &principal.subject, inquiry_id);
        return Reply::err(403, "forbidden");
    }
    let key = json!(inquiry_id).to_string();
    let mut entries = records::find_by("messages", "inquiry_id", &key).unwrap_or_default();
    // The store's own ids are ordered by creation, so sorting by id is
    // "oldest first" regardless of the order `find_by` hands them back.
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    Reply::json(200, json!({"messages": entries_json(&entries)}))
}

/// `POST /api/inquiries/{id}/messages` `{"body"}` — either party (or an
/// admin) may post, `403` for anyone else, `201` with the new message.
fn post_message(route: &Route, inquiry_id: &str, raw: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let inquiry = match inquiry_doc(inquiry_id) {
        Some(doc) => doc,
        None => return Reply::err(404, "not_found"),
    };
    if !is_party(is_admin(&principal), &principal.subject, &inquiry) {
        audit("message.create", "deny", &principal.subject, inquiry_id);
        return Reply::err(403, "forbidden");
    }
    let data = json!({
        "inquiry_id": inquiry_id,
        "sender": &principal.subject,
        "body": body_field(raw),
    })
    .to_string();
    match records::create("messages", &data, &["inquiry_id".to_string()]) {
        Ok(entry) => {
            audit("message.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, with_id(&entry.data, &entry.id))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}