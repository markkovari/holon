//! eshop:payment — the eShopOnDapr Payment.API (simulated) over composed
//! contracts. Consumes OrderStatusChangedToValidated; answers
//! OrderPaymentSucceeded or OrderPaymentFailed per the `payment-succeeds`
//! config flag — the same success/failure toggle the original exposes.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};

use bindings::event::bus::bus;
use bindings::wasi::config::store as config;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

guestio::guest_write_all!();

struct Component;

const GROUP: &str = "payment";

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/");

        let (status, body) = match (&method, route) {
            (Method::Get, "/") => (
                200,
                json!({
                    "service": "eshop-payment",
                    "pump": "POST /internal/pump (validated -> payment result)",
                    "toggle": "config payment-succeeds (default true)"
                })
                .to_string(),
            ),
            (Method::Post, "/internal/pump") => pump(),
            _ => (404, "{\"error\":\"not_found\"}".into()),
        };
        respond(response_out, status, body.as_bytes());
    }
}

fn pump() -> (u16, String) {
    let succeeds =
        config::get("payment-succeeds").ok().flatten().map(|v| v != "false").unwrap_or(true);
    let mut processed = 0;
    match bus::poll("OrderStatusChangedToValidated", GROUP, 32) {
        Ok(events) => {
            for ev in &events {
                if let Ok(data) = serde_json::from_slice::<Value>(&ev.payload) {
                    if let Some(order_id) = data["orderId"].as_str() {
                        let topic =
                            if succeeds { "OrderPaymentSucceeded" } else { "OrderPaymentFailed" };
                        let payload = json!({"orderId": order_id});
                        let _ = bus::publish(topic, payload.to_string().as_bytes());
                        processed += 1;
                    }
                }
                let _ = bus::ack(&ev.topic, GROUP, std::slice::from_ref(&ev.id));
            }
        }
        Err(bus::BusError::BackendUnavailable(m)) => {
            return (503, json!({ "error": m }).to_string())
        }
    }
    (200, json!({"processed": processed, "succeeds": succeeds}).to_string())
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
        let _ = write_all(&stream, body);
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
