//! eshop:payment — the eShopOnDapr Payment.API (simulated) over composed
//! contracts. Consumes OrderStatusChangedToValidated; answers
//! OrderPaymentSucceeded or OrderPaymentFailed per the `payment-succeeds`
//! config flag — the same success/failure toggle the original exposes.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../event-bus/wit",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "wit",
        ],
        world: "eshop:payment/payment-service",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use serde_json::{json, Value};

use bindings::event::bus::bus;
use bindings::wasi::config::store as config;

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};

guestio::guest_p3_respond!();

struct Component;

const GROUP: &str = "payment";

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
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
        respond(status, "application/json", body)
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

bindings::export!(Component with_types_in bindings);
