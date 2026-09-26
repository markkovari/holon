//! Part 1 — `catalog.rs`: listings and order placement (CONTRACT.md).
//!
//! Two things this file deliberately never does itself, because the composed
//! components own them:
//!
//!   * the order lifecycle is `fsm:workflow/engine` — the stored `orders`
//!     document carries no status field at all, and every read merges in
//!     `get-status("order", <order id>)`;
//!   * the category is *drafted* by `jev:decision/decision` and is advisory
//!     only: a `decision-error` still creates the listing, with the
//!     `other`/0 fallback. An advisory call must never block the write it
//!     advises on.

use crate::bindings::fsm::workflow::engine as fsm;
use crate::bindings::jev::decision::decision as jev;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{audit, introspect, is_admin, Reply, Route};
use serde_json::{json, Value};

// The two routes in this part whose authorization is a ROLE rather than an
// ownership check. CONTRACT.md's route table marks `POST /api/listings`
// `(vendor)` and `POST /api/orders` `(buyer)`, and spells out "or admin"
// explicitly wherever an admin is allowed too — so a role that is not
// listed is refused, an admin included: `PATCH .../category` and `delist`
// are the routes that say "or admin", and they are not these. Idempotent
// predicate setup, the same discipline `is_admin` is defined with.
guestauth::guest_role_check!(is_vendor, "vendor");
guestauth::guest_role_check!(is_buyer, "buyer");

/// CONTRACT.md's fixed taxonomy — the only category a listing ever carries,
/// and the exact option list handed to Jev on creation.
const CATEGORIES: [&str; 6] = ["electronics", "clothing", "home", "books", "toys", "other"];

/// Jev is asked a question here, never given an order: whatever comes back
/// only annotates the record a vendor may afterwards overwrite.
const LABEL_INSTRUCTIONS: &str =
    "Which single category best fits this item? Answer with exactly one of the options.";

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "listings"]) => create_listing(route, body),
        (Method::Patch, ["api", "listings", id, "category"]) => override_category(route, id, body),
        // NOTE: CONTRACT.md's `GET /api/listings?category=` cannot be read
        // here — `Route` carries only `segments` and `bearer`, and the
        // router in `lib.rs` strips the query string before dispatching.
        // Everything else about the route is honoured.
        (Method::Get, ["api", "listings"]) => list_listings(route),
        (Method::Get, ["api", "listings", id]) => get_listing(route, id),
        (Method::Post, ["api", "listings", id, "delist"]) => delist_listing(route, id),
        (Method::Post, ["api", "orders"]) => place_order(route, body),
        (Method::Get, ["api", "orders"]) => list_orders(route),
        (Method::Get, ["api", "orders", id]) => get_order(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

// ---------------------------------------------------------------------------
// The five machines
// ---------------------------------------------------------------------------

/// CONTRACT.md's machine table, typed into Rust exactly as `lib.rs`'s seed
/// already types it. `define` on an already-defined name replaces it with
/// the same definition, which CONTRACT.md itself calls harmless — so every
/// request that needs a machine re-asserts it, rather than keeping a
/// "defined yet?" flag that would be wrong whenever a DIFFERENT part's code
/// path runs first.
fn ensure_machines() {
    let def =
        |states: &[&str], initial: &str, transitions: &[(&str, &str, &str)], terminal: &[&str]| {
            fsm::Definition {
                states: states.iter().map(|s| s.to_string()).collect(),
                initial: initial.to_string(),
                transitions: transitions
                    .iter()
                    .map(|(event, source, target)| fsm::Transition {
                        event: event.to_string(),
                        source: source.to_string(),
                        target: target.to_string(),
                    })
                    .collect(),
                terminal: terminal.iter().map(|s| s.to_string()).collect(),
            }
        };
    let _ = fsm::define(
        "order",
        &def(
            &["placed", "paid", "shipped", "delivered", "cancelled"],
            "placed",
            &[
                ("pay", "placed", "paid"),
                ("ship", "paid", "shipped"),
                ("deliver", "shipped", "delivered"),
                ("cancel", "placed", "cancelled"),
                ("cancel", "paid", "cancelled"),
            ],
            &["delivered", "cancelled"],
        ),
    );
    let _ = fsm::define(
        "payment",
        &def(
            &["authorized", "captured", "refunded"],
            "authorized",
            &[("capture", "authorized", "captured"), ("refund", "captured", "refunded")],
            &["refunded"],
        ),
    );
    let _ = fsm::define(
        "shipment",
        &def(
            &["label_created", "in_transit", "delivered"],
            "label_created",
            &[("dispatch", "label_created", "in_transit"), ("deliver", "in_transit", "delivered")],
            &["delivered"],
        ),
    );
    let _ = fsm::define(
        "return",
        &def(
            &["requested", "approved", "rejected"],
            "requested",
            &[("approve", "requested", "approved"), ("reject", "requested", "rejected")],
            &["approved", "rejected"],
        ),
    );
    let _ = fsm::define(
        "dispute",
        &def(&["open", "resolved"], "open", &[("resolve", "open", "resolved")], &["resolved"]),
    );
}

// ---------------------------------------------------------------------------
// Jev — the advisory auto-label
// ---------------------------------------------------------------------------

/// `choice-result.confidence` is a `u32` milli-probability (decision.wit),
/// and that is what every provider emits — jev-decision's baseline answers
/// 800 or 100. It is already in `category_confidence`'s units, so it is taken
/// as-is: a `1` is 1/1000, not certainty. Clamped to 1000, the ceiling both
/// the WIT and CONTRACT.md put on it, so a misbehaving provider cannot store
/// more than certain.
fn confidence_milli(raw: u32) -> u32 {
    raw.min(1000)
}

/// Asks Jev which of the six categories fits, and never fails: on any
/// `decision-error` the caller still gets a usable `(category, confidence)`.
fn label_category(title: &str, description: &str) -> (String, u32) {
    let request = jev::ChoiceRequest {
        state: format!("{title} {description}"),
        instructions: LABEL_INSTRUCTIONS.to_string(),
        options: CATEGORIES.iter().map(|c| c.to_string()).collect(),
    };
    match jev::choose(&request) {
        Ok(chosen) => {
            let category = if CATEGORIES.contains(&chosen.selected.as_str()) {
                chosen.selected
            } else {
                // A model may advise, never decide: an answer outside the
                // taxonomy is not a category.
                "other".to_string()
            };
            (category, confidence_milli(chosen.confidence))
        }
        Err(_) => ("other".to_string(), 0),
    }
}

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

/// A stored document plus its own id.
fn document_json(id: &str, data: &str) -> Value {
    let mut document: Value = serde_json::from_str(data).unwrap_or(json!({}));
    if let Value::Object(ref mut fields) = document {
        fields.insert("id".to_string(), json!(id));
    }
    document
}

/// An `orders` document as CONTRACT.md wants it on the wire: the stored
/// fields, the id, and `status` merged in from the `order` machine.
fn order_json(id: &str, data: &str) -> Value {
    let mut document = document_json(id, data);
    if let Value::Object(ref mut fields) = document {
        if let Ok(status) = fsm::get_status("order", id) {
            fields.insert("status".to_string(), json!(status.state));
        }
    }
    document
}

fn parse_json(body: &str) -> Option<Value> {
    serde_json::from_str(body).ok()
}

// ---------------------------------------------------------------------------
// Listings
// ---------------------------------------------------------------------------

/// `POST /api/listings` — vendor creates a listing. `400` if `title` is
/// empty, `price` is not a positive integer, or `stock` is negative.
fn create_listing(route: &Route, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    if !is_vendor(&principal) {
        audit("listing.create", "deny", &principal.subject, "");
        return Reply::err(403, "forbidden");
    }
    let request = match parse_json(body) {
        Some(value) => value,
        None => return Reply::err(400, "invalid_json"),
    };

    let title = request.get("title").and_then(Value::as_str).unwrap_or("").to_string();
    if title.is_empty() {
        return Reply::err(400, "title is required");
    }
    let description = request.get("description").and_then(Value::as_str).unwrap_or("").to_string();
    let price = match request.get("price").and_then(Value::as_i64) {
        Some(price) if price > 0 => price,
        _ => return Reply::err(400, "price must be a positive integer"),
    };
    let stock = match request.get("stock") {
        None => 0,
        Some(value) => match value.as_i64() {
            Some(stock) if stock >= 0 => stock,
            _ => return Reply::err(400, "stock must be a non-negative integer"),
        },
    };

    // The advisory call happens BEFORE the write, but never gates it.
    let (category, category_confidence) = label_category(&title, &description);

    let listing = json!({
        "vendor": &principal.subject,
        "title": title,
        "description": description,
        "price": price,
        "stock": stock,
        "category": category,
        "category_confidence": category_confidence,
        "status": "active",
    });

    let entry = match records::create(
        "listings",
        &listing.to_string(),
        &["vendor".to_string(), "status".to_string()],
    ) {
        Ok(entry) => entry,
        Err(_) => return Reply::err(500, "store_error"),
    };

    audit("listing.create", "allow", &principal.subject, &entry.id);

    let mut out = listing;
    out["id"] = json!(entry.id);
    Reply::json(201, out)
}

/// `PATCH /api/listings/{id}/category` — the listing's own vendor, or an
/// admin. A human overriding the label is certain, hence confidence 1000.
fn override_category(route: &Route, id: &str, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let entry = guestauth::guest_get_or_404!("listings", id);
    let mut listing: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));

    let vendor = listing.get("vendor").and_then(Value::as_str).unwrap_or("").to_string();
    if vendor != principal.subject && !is_admin(&principal) {
        return Reply::err(403, "forbidden");
    }

    let request = match parse_json(body) {
        Some(value) => value,
        None => return Reply::err(400, "invalid_json"),
    };
    let category = request.get("category").and_then(Value::as_str).unwrap_or("");
    if !CATEGORIES.contains(&category) {
        return Reply::err(400, "category must be one of the fixed taxonomy");
    }

    listing["category"] = json!(category);
    listing["category_confidence"] = json!(1000);

    match records::update("listings", id, &listing.to_string(), entry.revision) {
        Ok(_) => {
            audit("listing.category", "allow", &principal.subject, id);
            listing["id"] = json!(id);
            Reply::json(200, listing)
        }
        Err(_) => Reply::err(409, "conflict"),
    }
}

/// `GET /api/listings` — any authenticated caller, active listings only,
/// newest first.
fn list_listings(route: &Route) -> Reply {
    if introspect(route).is_err() {
        return Reply::err(401, "unauthorized");
    }
    let mut entries = match crate::list_all("listings") {
        Ok(entries) => entries,
        Err(_) => return Reply::err(500, "store_error"),
    };
    // "Newest first" is a property of the store's own creation stamp, not of
    // whatever order `list_records` happened to hand the pages back in.
    entries.sort_by_key(|a| std::cmp::Reverse(a.created));

    let mut listings = Vec::new();
    for entry in entries.iter() {
        let listing: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
        if listing.get("status").and_then(Value::as_str) != Some("active") {
            continue;
        }
        listings.push(document_json(&entry.id, &entry.data));
    }
    Reply::json(200, json!({"listings": listings}))
}

/// `GET /api/listings/{id}` — any authenticated caller. A delisted listing
/// 404s for everyone except its own vendor/admin, who still see it.
fn get_listing(route: &Route, id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let entry = match records::get("listings", id) {
        Ok(entry) => entry,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let listing: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));

    if listing.get("status").and_then(Value::as_str) == Some("delisted") {
        let vendor = listing.get("vendor").and_then(Value::as_str).unwrap_or("");
        if vendor != principal.subject && !is_admin(&principal) {
            return Reply::err(404, "not_found");
        }
    }
    Reply::json(200, document_json(&entry.id, &entry.data))
}

/// `POST /api/listings/{id}/delist` — the listing's own vendor, or an admin.
fn delist_listing(route: &Route, id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let entry = guestauth::guest_get_or_404!("listings", id);
    let mut listing: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));

    let vendor = listing.get("vendor").and_then(Value::as_str).unwrap_or("").to_string();
    if vendor != principal.subject && !is_admin(&principal) {
        return Reply::err(403, "forbidden");
    }

    listing["status"] = json!("delisted");
    match records::update("listings", id, &listing.to_string(), entry.revision) {
        Ok(_) => {
            audit("listing.delist", "allow", &principal.subject, id);
            listing["id"] = json!(id);
            Reply::json(200, listing)
        }
        Err(_) => Reply::err(409, "conflict"),
    }
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

/// `POST /api/orders` — a multi-item order with an atomic stock check.
///
/// Phase 1 resolves and checks EVERY line and writes nothing; phase 2 only
/// runs once every line has passed, so an oversell on item 2 of 3 leaves
/// item 1's stock untouched.
fn place_order(route: &Route, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    if !is_buyer(&principal) {
        audit("order.create", "deny", &principal.subject, "");
        return Reply::err(403, "forbidden");
    }
    ensure_machines();

    let request = match parse_json(body) {
        Some(value) => value,
        None => return Reply::err(400, "invalid_json"),
    };
    let requested = match request.get("items").and_then(Value::as_array) {
        Some(items) if !items.is_empty() => items.clone(),
        _ => return Reply::err(400, "items must not be empty"),
    };

    // ---- phase 1: check every line, write nothing --------------------------
    // One entry per REQUEST line (so the stored order keeps the caller's own
    // line structure)...
    let mut lines: Vec<(String, i64, i64)> = Vec::new(); // listing_id, quantity, unit_price
                                                         // ...plus one running total per LISTING, because a listing named twice in
                                                         // one order must be checked against its stock cumulatively.
    let mut wanted: Vec<(String, i64)> = Vec::new();

    for item in requested.iter() {
        let listing_id = match item.get("listing_id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return Reply::err(400, "every item needs a listing_id"),
        };
        let quantity = match item.get("quantity").and_then(Value::as_i64) {
            Some(quantity) if quantity > 0 => quantity,
            _ => return Reply::err(400, "every item needs a positive integer quantity"),
        };

        let entry = match records::get("listings", &listing_id) {
            Ok(entry) => entry,
            Err(_) => {
                return Reply::json(
                    409,
                    json!({"error": "listing_not_found", "listing_id": listing_id}),
                )
            }
        };
        let listing: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
        if listing.get("status").and_then(Value::as_str) != Some("active") {
            return Reply::json(
                409,
                json!({"error": "listing_delisted", "listing_id": listing_id}),
            );
        }

        let stock = listing.get("stock").and_then(Value::as_i64).unwrap_or(0);
        let already =
            wanted.iter().find(|(id, _)| *id == listing_id).map(|(_, total)| *total).unwrap_or(0);
        let asked = already + quantity;
        if stock < asked {
            return Reply::json(
                409,
                json!({
                    "error": "insufficient_stock",
                    "listing_id": listing_id,
                    "available": stock,
                    "requested": asked,
                }),
            );
        }
        match wanted.iter_mut().find(|(id, _)| *id == listing_id) {
            Some(slot) => slot.1 = asked,
            None => wanted.push((listing_id.clone(), asked)),
        }

        let unit_price = listing.get("price").and_then(Value::as_i64).unwrap_or(0);
        lines.push((listing_id, quantity, unit_price));
    }

    // ---- phase 2: every line checked out — now the writes ------------------
    for (listing_id, total_quantity) in wanted.iter() {
        let entry = match records::get("listings", listing_id) {
            Ok(entry) => entry,
            Err(_) => return Reply::err(500, "store_error"),
        };
        let mut listing: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
        let stock = listing.get("stock").and_then(Value::as_i64).unwrap_or(0);
        listing["stock"] = json!(stock - *total_quantity);
        if records::update("listings", listing_id, &listing.to_string(), entry.revision).is_err() {
            return Reply::err(500, "store_error");
        }
    }

    let items: Vec<Value> = lines
        .iter()
        .map(|(listing_id, quantity, unit_price)| {
            json!({
                "listing_id": listing_id,
                "quantity": quantity,
                "unit_price": unit_price,
            })
        })
        .collect();
    let total: i64 = lines.iter().map(|(_, quantity, unit_price)| *quantity * *unit_price).sum();

    let document = json!({
        "buyer": &principal.subject,
        "items": items,
        "total": total,
    });
    let entry = match records::create("orders", &document.to_string(), &["buyer".to_string()]) {
        Ok(entry) => entry,
        Err(_) => return Reply::err(500, "store_error"),
    };
    // The machine's own initial state IS `placed`; the new instance is the
    // order's lifecycle from here on, never a field on the document.
    if fsm::create_instance("order", &entry.id).is_err() {
        return Reply::err(500, "fsm_error");
    }

    audit("order.create", "allow", &principal.subject, &entry.id);

    let mut out = document;
    out["id"] = json!(entry.id);
    out["status"] = json!("placed");
    Reply::json(201, out)
}

/// `GET /api/orders` — a buyer sees their own, an admin sees all. Every
/// order carries its fsm status merged in.
fn list_orders(route: &Route) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    ensure_machines();

    let entries = if is_admin(&principal) {
        match crate::list_all("orders") {
            Ok(entries) => entries,
            Err(_) => return Reply::err(500, "store_error"),
        }
    } else {
        let buyer_json = serde_json::to_string(&principal.subject).unwrap_or_default();
        match records::find_by("orders", "buyer", &buyer_json) {
            Ok(entries) => entries,
            Err(_) => return Reply::err(500, "store_error"),
        }
    };

    let orders: Vec<Value> =
        entries.iter().map(|entry| order_json(&entry.id, &entry.data)).collect();
    Reply::json(200, json!({"orders": orders}))
}

/// `GET /api/orders/{id}` — the order's own buyer, an admin, or the vendor of
/// ANY item in it; 403 for anyone else, 404 if it doesn't exist.
fn get_order(route: &Route, id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    ensure_machines();

    let entry = match records::get("orders", id) {
        Ok(entry) => entry,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let order: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));

    let buyer = order.get("buyer").and_then(Value::as_str).unwrap_or("");
    let mut allowed = is_admin(&principal) || buyer == principal.subject;
    if !allowed {
        if let Some(items) = order.get("items").and_then(Value::as_array) {
            for item in items.iter() {
                let listing_id = item.get("listing_id").and_then(Value::as_str).unwrap_or("");
                if listing_id.is_empty() {
                    continue;
                }
                if let Ok(listing_entry) = records::get("listings", listing_id) {
                    let listing: Value =
                        serde_json::from_str(&listing_entry.data).unwrap_or(json!({}));
                    if listing.get("vendor").and_then(Value::as_str)
                        == Some(principal.subject.as_str())
                    {
                        allowed = true;
                        break;
                    }
                }
            }
        }
    }
    if !allowed {
        return Reply::err(403, "forbidden");
    }

    Reply::json(200, order_json(&entry.id, &entry.data))
}
#[cfg(test)]
mod tests {
    use super::confidence_milli;

    #[test]
    fn confidence_is_already_milli_units() {
        // jev-decision's baseline: 800 on a substring match, 100 on none.
        assert_eq!(confidence_milli(800), 800);
        assert_eq!(confidence_milli(100), 100);
        assert_eq!(confidence_milli(0), 0);
        // A 1 is one thousandth, not certainty.
        assert_eq!(confidence_milli(1), 1);
        assert_eq!(confidence_milli(1000), 1000);
    }

    #[test]
    fn confidence_never_exceeds_certain() {
        assert_eq!(confidence_milli(1001), 1000);
        assert_eq!(confidence_milli(u32::MAX), 1000);
    }
}
