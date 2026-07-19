//! eshop:catalog — the eShopOnDapr Catalog.API over composed contracts.
//!
//! Read path: the seeded demo catalog (brands / types / items) with
//! pageIndex/pageSize paging and brand/type filters, exactly the storefront
//! query surface of the original. Write path: none over HTTP — stock only
//! moves through the checkout choreography, same as the original:
//!
//!   OrderStatusChangedToAwaitingStockValidation  -> OrderStockConfirmed
//!                                                 | OrderStockRejected
//!   OrderStatusChangedToPaid                     -> stock decremented
//!
//! event:bus is pull-based, so consumption happens on POST /internal/pump
//! (driven by the eshop-pump loop; Dapr's push subscription stand-in).

#[allow(warnings)]
mod bindings;

use serde::Deserialize;
use serde_json::{json, Value};

use bindings::event::bus::bus;
use bindings::idempotency::guard::store as idem;
use bindings::records::store::store as records;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const ITEMS: &str = "catalog-items";
const BRANDS: &str = "catalog-brands";
const TYPES: &str = "catalog-types";
const GROUP: &str = "catalog";

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let result = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage_json(),
            (Method::Get, ["api", "catalog", "items"]) => list_items(&query),
            (Method::Get, ["api", "catalog", "items", id]) => get_item(id),
            (Method::Get, ["api", "catalog", "brands"]) => list_lookup(BRANDS),
            (Method::Get, ["api", "catalog", "types"]) => list_lookup(TYPES),
            (Method::Post, ["internal", "pump"]) => pump(),
            _ => Outcome::NotFound,
        };
        emit(response_out, result);
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
    NotFound,
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "eshop-catalog",
            "items": "GET /api/catalog/items?pageIndex&pageSize&brand&type",
            "item": "GET /api/catalog/items/{id}",
            "lookups": "GET /api/catalog/brands, GET /api/catalog/types",
            "pump": "POST /internal/pump (stock choreography consumer)"
        })
        .to_string(),
    )
}

// ---- seeding ---------------------------------------------------------------

/// The eShopOnDapr demo catalog, verbatim (prices in integer cents).
/// ponytail: seeded in code, no admin CRUD — the original has none either.
const SEED_BRANDS: [&str; 5] = ["Azure", ".NET", "Visual Studio", "SQL Server", "Other"];
const SEED_TYPES: [&str; 4] = ["Mug", "T-Shirt", "Sheet", "USB Memory Stick"];
const SEED_ITEMS: [(&str, &str, &str, u64); 12] = [
    (".NET Bot Black Hoodie", "T-Shirt", ".NET", 1950),
    (".NET Black & White Mug", "Mug", ".NET", 850),
    ("Prism White T-Shirt", "T-Shirt", "Other", 1200),
    (".NET Foundation T-shirt", "T-Shirt", ".NET", 1200),
    ("Roslyn Red Sheet", "Sheet", "Other", 850),
    (".NET Blue Hoodie", "T-Shirt", ".NET", 1200),
    ("Roslyn Red T-Shirt", "T-Shirt", "Other", 1200),
    ("Kudu Purple Hoodie", "T-Shirt", "Other", 850),
    ("Cup<T> White Mug", "Mug", "Other", 1200),
    (".NET Foundation Sheet", "Sheet", ".NET", 1200),
    ("Cup<T> Sheet", "Sheet", "Other", 850),
    ("Prism White TShirt", "T-Shirt", "Other", 1200),
];
const SEED_STOCK: u64 = 100;

/// Idempotent: one count() read steady-state, same gate as helpdesk-domain.
fn ensure_seeded() -> Result<(), Outcome> {
    if records::count(ITEMS).map(|n| n > 0).unwrap_or(false) {
        return Ok(());
    }
    let mut brand_ids = Vec::new();
    for name in SEED_BRANDS {
        let e = records::create(BRANDS, &json!({ "brand": name }).to_string(), &[])
            .map_err(store_err)?;
        brand_ids.push((name, e.id));
    }
    let mut type_ids = Vec::new();
    for name in SEED_TYPES {
        let e = records::create(TYPES, &json!({ "type": name }).to_string(), &[])
            .map_err(store_err)?;
        type_ids.push((name, e.id));
    }
    let id_of = |pairs: &[(&str, String)], name: &str| {
        pairs.iter().find(|(n, _)| *n == name).map(|(_, id)| id.clone()).unwrap_or_default()
    };
    for (name, typ, brand, price) in SEED_ITEMS {
        let data = json!({
            "name": name,
            "description": name,
            "price": price,
            "brandId": id_of(&brand_ids, brand),
            "brand": brand,
            "typeId": id_of(&type_ids, typ),
            "type": typ,
            "availableStock": SEED_STOCK,
        });
        records::create(ITEMS, &data.to_string(), &["brandId".into(), "typeId".into()])
            .map_err(store_err)?;
    }
    Ok(())
}

// ---- browse ----------------------------------------------------------------

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key && !v.is_empty()).then(|| v.to_string())
    })
}

fn list_items(query: &str) -> Outcome {
    if let Err(o) = ensure_seeded() {
        return o;
    }
    let page_index: usize = query_param(query, "pageIndex").and_then(|v| v.parse().ok()).unwrap_or(0);
    let page_size: usize = query_param(query, "pageSize")
        .and_then(|v| v.parse().ok())
        .map(|n: usize| n.clamp(1, 100))
        .unwrap_or(10);
    let brand = query_param(query, "brand");
    let typ = query_param(query, "type");

    // ponytail: 12-item demo catalog — fetch the indexed subset (or all) and
    // page in memory; add real index-ordered paging only if the catalog grows.
    let entries = match (&brand, &typ) {
        (Some(b), _) => match records::find_by(ITEMS, "brandId", &json!(b).to_string()) {
            Ok(e) => e,
            Err(e) => return store_err(e),
        },
        (None, Some(t)) => match records::find_by(ITEMS, "typeId", &json!(t).to_string()) {
            Ok(e) => e,
            Err(e) => return store_err(e),
        },
        (None, None) => match records::list_records(ITEMS, 500, "") {
            Ok(page) => page.entries,
            Err(e) => return store_err(e),
        },
    };
    let mut items: Vec<Value> = entries
        .iter()
        .map(item_json)
        .filter(|it| typ.as_deref().is_none_or(|t| it["typeId"] == t))
        .collect();
    items.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let count = items.len();
    let data: Vec<Value> = items.into_iter().skip(page_index * page_size).take(page_size).collect();
    Outcome::Json(
        200,
        json!({"pageIndex": page_index, "pageSize": page_size, "count": count, "data": data})
            .to_string(),
    )
}

fn get_item(id: &str) -> Outcome {
    if let Err(o) = ensure_seeded() {
        return o;
    }
    match records::get(ITEMS, id) {
        Ok(e) => Outcome::Json(200, item_json(&e).to_string()),
        Err(records::StoreError::NotFound) => Outcome::NotFound,
        Err(e) => store_err(e),
    }
}

fn list_lookup(collection: &str) -> Outcome {
    if let Err(o) = ensure_seeded() {
        return o;
    }
    let entries = match records::list_records(collection, 100, "") {
        Ok(page) => page.entries,
        Err(e) => return store_err(e),
    };
    let rows: Vec<Value> = entries
        .iter()
        .map(|e| {
            let d: Value = serde_json::from_str(&e.data).unwrap_or(Value::Null);
            let name = d["brand"].as_str().or(d["type"].as_str()).unwrap_or("");
            json!({"id": e.id, "name": name})
        })
        .collect();
    Outcome::Json(200, json!({ "data": rows }).to_string())
}

fn item_json(entry: &records::Entry) -> Value {
    let mut data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    data["id"] = json!(entry.id);
    data
}

// ---- stock choreography (the Dapr pub/sub side) ------------------------------

#[derive(Deserialize)]
struct StockItem {
    #[serde(rename = "productId")]
    product_id: String,
    units: u64,
}

#[derive(Deserialize)]
struct OrderStockEvent {
    #[serde(rename = "orderId")]
    order_id: String,
    #[serde(rename = "orderStockItems")]
    items: Vec<StockItem>,
}

/// Drain this service's consumer group: answer stock-validation requests and
/// apply paid-order decrements. At-least-once, so both consumers are deduped
/// through idempotency:guard keyed on the order id — concurrent pump drivers
/// polling the same unacked batch otherwise double-decrement (caught by the
/// choreography bench, ESHOP-BENCH.md). Ack only what was handled or replayed;
/// an in-progress or backend-failed event stays unacked for the next pass.
fn pump() -> Outcome {
    if let Err(o) = ensure_seeded() {
        return o;
    }
    let validated = consume("OrderStatusChangedToAwaitingStockValidation", "stockval", |req| {
        validate_stock(req);
    });
    let decremented = consume("OrderStatusChangedToPaid", "paid", |req| {
        for item in &req.items {
            adjust_stock(&item.product_id, item.units);
        }
    });
    match (validated, decremented) {
        (Ok(v), Ok(d)) => {
            Outcome::Json(200, json!({"validated": v, "decremented": d}).to_string())
        }
        (Err(e), _) | (_, Err(e)) => e,
    }
}

/// Poll one topic and run `handle` exactly once per order id. The bus offset
/// is a watermark (ack advances past everything below the highest id), so
/// events are handled strictly in order and the pass STOPS at the first
/// skippable event — acking only the contiguous handled prefix.
fn consume(
    topic: &str,
    kind: &str,
    handle: impl Fn(&OrderStockEvent),
) -> Result<u32, Outcome> {
    let events = bus::poll(topic, GROUP, 32).map_err(|e| bus_err(e))?;
    let mut handled = 0;
    let mut acked: Vec<String> = Vec::new();
    for ev in &events {
        let Ok(req) = serde_json::from_slice::<OrderStockEvent>(&ev.payload) else {
            acked.push(ev.id.clone()); // permanently unparseable: drop it
            continue;
        };
        match idem::begin(&format!("{kind}:{}", req.order_id), 300) {
            Ok(None) => {
                handle(&req);
                let _ = idem::complete(&format!("{kind}:{}", req.order_id), 200, &[]);
                handled += 1;
                acked.push(ev.id.clone());
            }
            // already handled by an earlier delivery: just advance the offset.
            Ok(Some(_)) => acked.push(ev.id.clone()),
            // a concurrent twin holds the key, or the backend hiccuped —
            // stop here so the watermark can't pass this event; a later pass
            // retries it (the reservation expires).
            Err(_) => break,
        }
    }
    if !acked.is_empty() {
        bus::ack(topic, GROUP, &acked).map_err(|e| bus_err(e))?;
    }
    Ok(handled)
}

fn validate_stock(req: &OrderStockEvent) {
    let rejected: Vec<&str> = req
        .items
        .iter()
        .filter(|item| {
            let stock = records::get(ITEMS, &item.product_id)
                .ok()
                .and_then(|e| serde_json::from_str::<Value>(&e.data).ok())
                .and_then(|d| d["availableStock"].as_u64())
                .unwrap_or(0);
            stock < item.units
        })
        .map(|item| item.product_id.as_str())
        .collect();
    let (topic, payload) = if rejected.is_empty() {
        ("OrderStockConfirmed", json!({"orderId": req.order_id}))
    } else {
        ("OrderStockRejected", json!({"orderId": req.order_id, "rejectedItems": rejected}))
    };
    let _ = bus::publish(topic, payload.to_string().as_bytes());
}

/// Decrement, floored at 0. Revision-checked with one retry — pump is the only
/// writer, the retry covers a concurrent pump replica.
fn adjust_stock(product_id: &str, units: u64) {
    for _ in 0..2 {
        let Ok(entry) = records::get(ITEMS, product_id) else { return };
        let Ok(mut data) = serde_json::from_str::<Value>(&entry.data) else { return };
        let stock = data["availableStock"].as_u64().unwrap_or(0);
        data["availableStock"] = json!(stock.saturating_sub(units));
        match records::update(ITEMS, product_id, &data.to_string(), entry.revision) {
            Ok(_) => return,
            Err(records::StoreError::RevisionConflict(_)) => continue,
            Err(_) => return,
        }
    }
}

// ---- helpers -----------------------------------------------------------------

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::NotFound,
        records::StoreError::InvalidJson(m) => Outcome::Err(400, m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn bus_err(e: bus::BusError) -> Outcome {
    match e {
        bus::BusError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

// ---- responses -----------------------------------------------------------------

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond(response_out, code, body.as_bytes()),
        Outcome::Err(code, msg) => {
            respond(response_out, code, json!({ "error": msg }).to_string().as_bytes())
        }
        Outcome::NotFound => respond(response_out, 404, b"{\"error\":\"not_found\"}"),
    }
}

fn respond(response_out: ResponseOutparam, status: u16, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
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
