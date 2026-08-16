//! eshop:basket — the eShopOnDapr Basket.API over composed contracts.
//!
//! One record per buyer (indexed by `buyer`), replaced wholesale on POST —
//! the same "the basket is a document" model the original keeps in Redis via
//! the Dapr state store. Checkout is fire-and-forget: validate, publish
//! UserCheckoutAccepted, 202. The basket survives until ordering publishes
//! OrderStarted (consumed on /internal/pump), so a crashed order flow never
//! eats the basket.

#[allow(warnings)]
mod bindings;

use serde::Deserialize;
use serde_json::{json, Value};

use bindings::auth::identity::authorizer;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::event::bus::bus;
use bindings::records::store::store as records;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const BASKETS: &str = "baskets";
const GROUP: &str = "basket";

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let result = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage_json(),
            (Method::Get, ["api", "basket"]) => get_basket(&request),
            (Method::Post, ["api", "basket"]) => put_basket(&request),
            (Method::Post, ["api", "basket", "checkout"]) => checkout(&request),
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
    NotFound,
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "eshop-basket",
            "basket": "GET|POST /api/basket (bearer token)",
            "checkout": "POST /api/basket/checkout {city,street,state,country,zipCode,cardNumber,cardHolderName,cardExpiration,cardSecurityNumber,cardTypeId}",
            "pump": "POST /internal/pump (OrderStarted -> clear basket)"
        })
        .to_string(),
    )
}

// ---- basket ------------------------------------------------------------------

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

/// The buyer's basket record, or None if they have none yet.
fn find_basket(buyer: &str) -> Result<Option<records::Entry>, Outcome> {
    match records::find_by(BASKETS, "buyer", &json!(buyer).to_string()) {
        Ok(entries) => Ok(entries.into_iter().next()),
        Err(records::StoreError::NotFound) => Ok(None),
        Err(e) => Err(store_err(e)),
    }
}

fn basket_json(buyer: &str, entry: Option<&records::Entry>) -> Value {
    let items = entry
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok())
        .map(|d| d["items"].clone())
        .unwrap_or_else(|| json!([]));
    json!({"buyerId": buyer, "items": items})
}

fn get_basket(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    match find_basket(&p.subject) {
        Ok(entry) => Outcome::Json(200, basket_json(&p.subject, entry.as_ref()).to_string()),
        Err(o) => o,
    }
}

fn put_basket(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    #[derive(Deserialize)]
    struct Req {
        items: Vec<BasketItem>,
    }
    let req: Req = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if req.items.len() > 100 {
        return Outcome::Bad("too many items".into());
    }
    if req.items.iter().any(|i| i.quantity == 0 || i.quantity > 1000) {
        return Outcome::Bad("quantity must be 1..1000".into());
    }
    let items: Vec<Value> = req
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
    let data = json!({"buyer": p.subject, "items": items}).to_string();
    let result = match find_basket(&p.subject) {
        Ok(Some(entry)) => records::update(BASKETS, &entry.id, &data, 0),
        Ok(None) => records::create(BASKETS, &data, &["buyer".to_string()]),
        Err(o) => return o,
    };
    match result {
        Ok(entry) => Outcome::Json(200, basket_json(&p.subject, Some(&entry)).to_string()),
        Err(e) => store_err(e),
    }
}

// ---- checkout ----------------------------------------------------------------

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct CheckoutReq {
    city: String,
    street: String,
    state: String,
    country: String,
    #[serde(rename = "zipCode")]
    zip_code: String,
    #[serde(rename = "cardNumber")]
    card_number: String,
    #[serde(rename = "cardHolderName")]
    card_holder_name: String,
    #[serde(rename = "cardExpiration")]
    card_expiration: String,
    #[serde(rename = "cardSecurityNumber")]
    _card_security_number: String,
    #[serde(rename = "cardTypeId", default)]
    card_type_id: u32,
}

fn checkout(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let req: CheckoutReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    for (name, v) in [
        ("city", &req.city),
        ("street", &req.street),
        ("state", &req.state),
        ("country", &req.country),
        ("zipCode", &req.zip_code),
        ("cardNumber", &req.card_number),
        ("cardHolderName", &req.card_holder_name),
        ("cardExpiration", &req.card_expiration),
    ] {
        if v.is_empty() || v.len() > 200 {
            return Outcome::Bad(format!("{name} must be 1..200 chars"));
        }
    }
    let entry = match find_basket(&p.subject) {
        Ok(Some(e)) => e,
        Ok(None) => return Outcome::Bad("basket is empty".into()),
        Err(o) => return o,
    };
    let basket = basket_json(&p.subject, Some(&entry));
    if basket["items"].as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return Outcome::Bad("basket is empty".into());
    }
    // Verbatim eShopOnDapr integration event: the buyer, the delivery/card
    // details ordering needs, and the basket snapshot. Card number is masked —
    // payment is simulated, nothing downstream needs it.
    let masked = format!("XXXX-{}", &req.card_number[req.card_number.len().saturating_sub(4)..]);
    let event = json!({
        "userId": p.subject,
        "userName": p.subject,
        "city": req.city,
        "street": req.street,
        "state": req.state,
        "country": req.country,
        "zipCode": req.zip_code,
        "cardNumber": masked,
        "cardHolderName": req.card_holder_name,
        "cardExpiration": req.card_expiration,
        "cardTypeId": req.card_type_id,
        "requestId": entry.id,
        "basket": basket,
    });
    match bus::publish("UserCheckoutAccepted", event.to_string().as_bytes()) {
        Ok(_) => Outcome::Json(202, json!({"accepted": true}).to_string()),
        Err(e) => bus_err(e),
    }
}

// ---- pump (OrderStarted -> clear basket) ---------------------------------------

fn pump() -> Outcome {
    let mut cleared = 0;
    match bus::poll("OrderStarted", GROUP, 32) {
        Ok(events) => {
            for ev in &events {
                let user = serde_json::from_slice::<Value>(&ev.payload)
                    .ok()
                    .and_then(|d| d["userId"].as_str().map(str::to_string));
                if let Some(user) = user {
                    if let Ok(Some(entry)) = find_basket(&user) {
                        let _ = records::delete(BASKETS, &entry.id);
                        cleared += 1;
                    }
                }
                let _ = bus::ack(&ev.topic, GROUP, &[ev.id.clone()]);
            }
        }
        Err(e) => return bus_err(e),
    }
    Outcome::Json(200, json!({ "cleared": cleared }).to_string())
}

// ---- helpers -------------------------------------------------------------------

fn introspect(request: &IncomingRequest) -> Result<Principal, Outcome> {
    let Some(token) = bearer(request) else {
        return Err(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())));
    };
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

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

fn parse<T: for<'a> Deserialize<'a>>(request: &IncomingRequest) -> Result<T, String> {
    let body = read_body(request).map_err(|_| "could not read body".to_string())?;
    serde_json::from_slice(&body).map_err(|e| format!("bad json: {e}"))
}

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let body = request.consume().map_err(|_| ())?;
    let stream = body.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => buf.extend_from_slice(&chunk),
            // `Closed` is how wasi:io says end-of-body; `LastOperationFailed` is a
            // read that went wrong. Collapsing both into `break` returns a TRUNCATED
            // body as if it were complete — the same silent truncation that, on the
            // write side, took four runs to find.
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(_) => return Err(()),
        }
    }
    Ok(buf)
}

fn bearer(request: &IncomingRequest) -> Option<String> {
    request
        .headers()
        .get(&"authorization".to_string())
        .into_iter()
        .find_map(|v| String::from_utf8(v).ok())
        .and_then(|s| s.strip_prefix("Bearer ").map(|tok| tok.trim().to_string()))
}

// ---- responses -------------------------------------------------------------------

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
