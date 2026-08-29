//! eshop:ordering — the eShopOnDapr Ordering.API over composed contracts.
//!
//! Every order is an fsm:workflow instance (the Dapr-actor stand-in): event
//! consumers and HTTP verbs FIRE transitions, the machine validates legality
//! and records history, and the order record mirrors the state (indexed) so
//! list/filter stays a lookup. The grace period is a pump-driven sweep over
//! `status == "submitted"` records instead of an actor reminder — same
//! observable behavior: a submitted order is cancellable until the window
//! elapses, then moves to stock validation on its own.

#[allow(warnings)]
mod bindings;

use serde::Deserialize;
use serde_json::{json, Value};

use bindings::auth::identity::authorizer;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::event::bus::bus;
use bindings::fsm::workflow::engine as fsm;
use bindings::idempotency::guard::store as idem;
use bindings::records::store::store as records;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::config::store as config;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const ORDERS: &str = "orders";
const MACHINE: &str = "order";
const GROUP: &str = "ordering";
/// eShopOnDapr's default GracePeriodTime is 1 minute; overridable so the
/// smoke test doesn't have to wait it out.
const DEFAULT_GRACE_SECS: u64 = 60;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let result = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage_json(),
            (Method::Get, ["api", "orders"]) => list_orders(&request),
            (Method::Get, ["api", "orders", id]) => get_order(&request, id),
            (Method::Post, ["api", "orders", id, "cancel"]) => cancel(&request, id),
            (Method::Post, ["api", "orders", id, "ship"]) => ship(&request, id),
            (Method::Post, ["internal", "pump"]) => pump(),
            _ => Outcome::NotFound,
        };
        emit(response_out, result);
    }
}

enum Outcome {
    Json(u16, String),
    Auth(AuthError),
    Bad(String),
    Err(u16, String),
    Forbidden(String),
    NotFound,
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "eshop-ordering",
            "orders": "GET /api/orders, GET /api/orders/{id}",
            "cancel": "POST /api/orders/{id}/cancel (pre-paid states)",
            "ship": "POST /api/orders/{id}/ship (admin, from paid)",
            "pump": "POST /internal/pump (checkout/stock/payment consumers + grace sweep)"
        })
        .to_string(),
    )
}

// ---- seeding -----------------------------------------------------------------

/// Idempotent: register the order lifecycle machine, helpdesk-domain style.
fn ensure_seeded() {
    if records::count("meta").map(|n| n > 0).unwrap_or(false) {
        return;
    }
    fn t(event: &str, source: &str, target: &str) -> fsm::Transition {
        fsm::Transition { event: event.into(), source: source.into(), target: target.into() }
    }
    let def = fsm::Definition {
        states: [
            "submitted",
            "awaitingStockValidation",
            "validated",
            "paid",
            "shipped",
            "cancelled",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        initial: "submitted".into(),
        transitions: vec![
            t("grace-expired", "submitted", "awaitingStockValidation"),
            t("stock-confirmed", "awaitingStockValidation", "validated"),
            t("stock-rejected", "awaitingStockValidation", "cancelled"),
            t("payment-succeeded", "validated", "paid"),
            t("payment-failed", "validated", "cancelled"),
            t("ship", "paid", "shipped"),
            t("cancel", "submitted", "cancelled"),
            t("cancel", "awaitingStockValidation", "cancelled"),
            t("cancel", "validated", "cancelled"),
        ],
        terminal: vec!["shipped".into(), "cancelled".into()],
    };
    let _ = fsm::define(MACHINE, &def);
    let _ = records::create("meta", "{\"seeded\":true}", &[]);
}

fn grace_secs() -> u64 {
    config::get("grace-period-secs")
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_GRACE_SECS)
}

fn now() -> u64 {
    wall_clock::now().seconds
}

// ---- HTTP: order queries + verbs ----------------------------------------------

fn is_admin(p: &Principal) -> bool {
    p.roles.iter().any(|r| r == "admin")
}

fn introspect(request: &IncomingRequest) -> Result<Principal, Outcome> {
    let Some(token) = bearer(request) else {
        return Err(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())));
    };
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

/// Load an order; buyers only see their own (404, existence not leaked).
fn load_order(p: &Principal, id: &str) -> Result<(records::Entry, Value), Outcome> {
    let entry = match records::get(ORDERS, id) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Err(Outcome::NotFound),
        Err(e) => return Err(store_err(e)),
    };
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    if !is_admin(p) && data["buyer"].as_str() != Some(p.subject.as_str()) {
        return Err(Outcome::NotFound);
    }
    Ok((entry, data))
}

fn order_json(entry: &records::Entry) -> Value {
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    json!({
        "id": entry.id,
        "buyer": data["buyer"],
        "status": data["status"],
        "items": data["items"],
        "total": data["total"],
        "address": data["address"],
        "date": entry.created,
        "updated": entry.updated,
    })
}

fn list_orders(request: &IncomingRequest) -> Outcome {
    ensure_seeded();
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let entries = if is_admin(&p) {
        match records::list_records(ORDERS, 500, "") {
            Ok(page) => page.entries,
            Err(e) => return store_err(e),
        }
    } else {
        match records::find_by(ORDERS, "buyer", &json!(p.subject).to_string()) {
            Ok(entries) => entries,
            Err(records::StoreError::NotFound) => Vec::new(),
            Err(e) => return store_err(e),
        }
    };
    let orders: Vec<Value> = entries.iter().map(order_json).collect();
    Outcome::Json(200, json!({ "orders": orders }).to_string())
}

fn get_order(request: &IncomingRequest, id: &str) -> Outcome {
    ensure_seeded();
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let (entry, _) = match load_order(&p, id) {
        Ok(t) => t,
        Err(o) => return o,
    };
    let mut out = order_json(&entry);
    if let Ok(entries) = fsm::history(MACHINE, id) {
        out["history"] = json!(entries
            .iter()
            .map(|h| json!({"event": h.event, "from": h.source, "to": h.target, "at": h.at}))
            .collect::<Vec<_>>());
    }
    Outcome::Json(200, out.to_string())
}

fn cancel(request: &IncomingRequest, id: &str) -> Outcome {
    ensure_seeded();
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let (entry, data) = match load_order(&p, id) {
        Ok(t) => t,
        Err(o) => return o,
    };
    fire_and_mirror(&entry, &data, "cancel", "OrderStatusChangedToCancelled")
}

fn ship(request: &IncomingRequest, id: &str) -> Outcome {
    ensure_seeded();
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if !is_admin(&p) {
        return Outcome::Forbidden("shipping is admin-only".into());
    }
    let (entry, data) = match load_order(&p, id) {
        Ok(t) => t,
        Err(o) => return o,
    };
    fire_and_mirror(&entry, &data, "ship", "OrderStatusChangedToShipped")
}

/// Fire one FSM event; on success mirror the state onto the record and
/// publish the matching integration event. 409 on an illegal transition.
fn fire_and_mirror(entry: &records::Entry, data: &Value, event: &str, topic: &str) -> Outcome {
    match fsm::fire(MACHINE, &entry.id, event) {
        Ok(status) => {
            mirror_status(entry, data, &status.state);
            let payload = json!({
                "orderId": entry.id,
                "orderStatus": status.state,
                "buyerId": data["buyer"],
            });
            let _ = bus::publish(topic, payload.to_string().as_bytes());
            Outcome::Json(200, json!({"id": entry.id, "status": status.state}).to_string())
        }
        Err(fsm::FsmError::IllegalTransition(current)) => {
            Outcome::Err(409, format!("cannot {event} from {current}"))
        }
        Err(e) => Outcome::Err(503, format!("fsm: {e:?}")),
    }
}

fn mirror_status(entry: &records::Entry, data: &Value, state: &str) {
    let mut data = data.clone();
    data["status"] = json!(state);
    // revision 0 = last-write-wins; the FSM already serialized the transition.
    let _ = records::update(ORDERS, &entry.id, &data.to_string(), 0);
}

// ---- pump: the choreography ------------------------------------------------------

#[derive(Deserialize)]
struct CheckoutEvent {
    #[serde(rename = "userId")]
    user_id: String,
    city: String,
    street: String,
    state: String,
    country: String,
    #[serde(rename = "zipCode")]
    zip_code: String,
    basket: BasketSnapshot,
}

#[derive(Deserialize)]
struct BasketSnapshot {
    items: Vec<BasketItem>,
}

#[derive(Deserialize)]
struct BasketItem {
    #[serde(rename = "productId")]
    product_id: String,
    #[serde(rename = "productName")]
    product_name: String,
    #[serde(rename = "unitPrice")]
    unit_price: u64,
    quantity: u64,
}

fn pump() -> Outcome {
    ensure_seeded();
    let mut created = 0;
    let mut advanced = 0;

    // 1. checkout -> new submitted order (+ OrderStarted clears the basket).
    // Order creation is the one non-idempotent consumer here (the FSM naturally
    // dedupes everything else), so it's guarded per BUS EVENT ID: a duplicate
    // delivery of the same publish must not mint a second order, while two
    // distinct checkouts (distinct event ids) both must. The ack offset is a
    // watermark, so the pass stops at the first skippable event.
    match bus::poll("UserCheckoutAccepted", GROUP, 32) {
        Ok(events) => {
            let mut acked: Vec<String> = Vec::new();
            for ev in &events {
                let key = format!("checkout:{}", ev.id);
                match idem::begin(&key, 300) {
                    Ok(None) => {
                        if let Ok(co) = serde_json::from_slice::<CheckoutEvent>(&ev.payload) {
                            if start_order(&co) {
                                created += 1;
                            }
                        }
                        let _ = idem::complete(&key, 200, &[]);
                        acked.push(ev.id.clone());
                    }
                    Ok(Some(_)) => acked.push(ev.id.clone()),
                    Err(_) => break,
                }
            }
            if !acked.is_empty() {
                let _ = bus::ack("UserCheckoutAccepted", GROUP, &acked);
            }
        }
        Err(e) => return bus_err(e),
    }

    // 2. stock + payment answers -> FSM transitions + follow-on events.
    for (topic, event, next_topic) in [
        ("OrderStockConfirmed", "stock-confirmed", "OrderStatusChangedToValidated"),
        ("OrderStockRejected", "stock-rejected", "OrderStatusChangedToCancelled"),
        ("OrderPaymentSucceeded", "payment-succeeded", "OrderStatusChangedToPaid"),
        ("OrderPaymentFailed", "payment-failed", "OrderStatusChangedToCancelled"),
    ] {
        match bus::poll(topic, GROUP, 32) {
            Ok(events) => {
                for ev in &events {
                    let order_id = serde_json::from_slice::<Value>(&ev.payload)
                        .ok()
                        .and_then(|d| d["orderId"].as_str().map(str::to_string));
                    if let Some(order_id) = order_id {
                        if advance_order(&order_id, event, next_topic) {
                            advanced += 1;
                        }
                    }
                    let _ = bus::ack(&ev.topic, GROUP, std::slice::from_ref(&ev.id));
                }
            }
            Err(e) => return bus_err(e),
        }
    }

    // 3. grace sweep: submitted orders past the window go to stock validation.
    let grace = grace_secs();
    let submitted =
        records::find_by(ORDERS, "status", &json!("submitted").to_string()).unwrap_or_default();
    for entry in &submitted {
        let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
        let at = data["submittedAt"].as_u64().unwrap_or(entry.created);
        if now() >= at + grace
            && advance_order(
                &entry.id,
                "grace-expired",
                "OrderStatusChangedToAwaitingStockValidation",
            )
        {
            advanced += 1;
        }
    }

    Outcome::Json(200, json!({"created": created, "advanced": advanced}).to_string())
}

/// UserCheckoutAccepted -> order record + FSM instance + OrderStarted.
fn start_order(co: &CheckoutEvent) -> bool {
    if co.basket.items.is_empty() {
        return false;
    }
    let total: u64 = co.basket.items.iter().map(|i| i.unit_price * i.quantity).sum();
    let items: Vec<Value> = co
        .basket
        .items
        .iter()
        .map(|i| {
            json!({
                "productId": i.product_id,
                "productName": i.product_name,
                "unitPrice": i.unit_price,
                "quantity": i.quantity,
            })
        })
        .collect();
    let data = json!({
        "buyer": co.user_id,
        "status": "submitted",
        "submittedAt": now(),
        "items": items,
        "total": total,
        "address": {
            "city": co.city, "street": co.street, "state": co.state,
            "country": co.country, "zipCode": co.zip_code,
        },
    });
    let Ok(entry) =
        records::create(ORDERS, &data.to_string(), &["buyer".to_string(), "status".to_string()])
    else {
        return false;
    };
    let _ = fsm::create_instance(MACHINE, &entry.id);
    let started = json!({"orderId": entry.id, "userId": co.user_id});
    let _ = bus::publish("OrderStarted", started.to_string().as_bytes());
    let submitted = json!({"orderId": entry.id, "orderStatus": "submitted", "buyerId": co.user_id});
    let _ = bus::publish("OrderStatusChangedToSubmitted", submitted.to_string().as_bytes());
    true
}

/// Fire `event` on an order; on success mirror + publish `next_topic` with the
/// stock items attached (catalog and payment both key off them).
fn advance_order(order_id: &str, event: &str, next_topic: &str) -> bool {
    let Ok(entry) = records::get(ORDERS, order_id) else { return false };
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    let Ok(status) = fsm::fire(MACHINE, order_id, event) else { return false };
    mirror_status(&entry, &data, &status.state);
    let stock_items: Vec<Value> = data["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|i| json!({"productId": i["productId"], "units": i["quantity"]}))
                .collect()
        })
        .unwrap_or_default();
    let payload = json!({
        "orderId": order_id,
        "orderStatus": status.state,
        "buyerId": data["buyer"],
        "total": data["total"],
        "orderStockItems": stock_items,
    });
    let _ = bus::publish(next_topic, payload.to_string().as_bytes());
    true
}

// ---- helpers ----------------------------------------------------------------------

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::NotFound,
        records::StoreError::InvalidJson(m) => Outcome::Bad(m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn bus_err(e: bus::BusError) -> Outcome {
    match e {
        bus::BusError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn auth_error(e: &AuthError) -> (u16, &'static str) {
    match e {
        AuthError::InvalidCredentials => (401, "invalid_credentials"),
        AuthError::AlreadyExists => (409, "already_exists"),
        AuthError::RateLimited(_) => (429, "rate_limited"),
        AuthError::InsufficientScope(_) => (403, "insufficient_scope"),
        AuthError::Expired => (401, "expired"),
        AuthError::InvalidToken(_) => (401, "invalid_token"),
        AuthError::UnknownTenant => (403, "unknown_tenant"),
        AuthError::Malformed(_) => (400, "malformed"),
        AuthError::BackendUnavailable(_) => (503, "backend_unavailable"),
        AuthError::Internal(_) => (500, "internal"),
    }
}

guestio::guest_bearer!();

// ---- responses ---------------------------------------------------------------------

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond(response_out, code, body.as_bytes()),
        Outcome::Auth(e) => {
            let (code, msg) = auth_error(&e);
            respond(response_out, code, format!("{{\"error\":\"{msg}\"}}").as_bytes());
        }
        Outcome::Bad(msg) => {
            respond(response_out, 400, json!({ "error": msg }).to_string().as_bytes())
        }
        Outcome::Err(code, msg) => {
            respond(response_out, code, json!({ "error": msg }).to_string().as_bytes())
        }
        Outcome::Forbidden(msg) => {
            respond(response_out, 403, json!({ "error": msg }).to_string().as_bytes())
        }
        Outcome::NotFound => respond(response_out, 404, b"{\"error\":\"not_found\"}"),
    }
}

fn respond(response_out: ResponseOutparam, status: u16, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in body.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
