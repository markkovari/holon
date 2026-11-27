//! `vet-booking-domain` — Meadowbrook Vet Clinic booking portal.
//!
//! Domain-specific UI (1-page form vs. 3-step wizard) and `clinic:triage`
//! execution live here — never inside `delivery-domain`.

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/analytics-telemetry",
            "../clinic-triage/wit",
            "../id-generate/wit",
            "wit",
        ],
        world: "vet:booking/vet-booking-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0::handler;
        pub use super::wasi::clocks0_3_0 as clocks;
        pub use super::wasi::http0_3_0 as http;
    }
}

pub const PORTAL_HTML: &str = include_str!("pet_clinic_app.html");

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::bindings;
    use crate::PORTAL_HTML;
    use serde_json::{json, Value};

    use bindings::analytics::telemetry::ingest as analytics;
    use bindings::analytics::telemetry::types::{Event, Property};
    use bindings::clinic::triage::evaluator as triage;
    use bindings::id::generate::generator as ids;
    use bindings::p3::clocks::system_clock;
    use bindings::p3::handler::Guest;
    use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};

    guestio::guest_p3_respond!();

    const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
    guestio::guest_p3_read_body!(MAX_BODY_BYTES);

    struct Component;

    impl Guest for Component {
        async fn handle(request: Request) -> Result<Response, ErrorCode> {
            let method = request.get_method();
            let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
            let route = path.split('?').next().unwrap_or("/").to_string();
            let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

            match (&method, seg.as_slice()) {
                (Method::Get, ["health"]) => json_resp(200, json!({"ok": true}).to_string()),
                (Method::Get, [""] | ["apps", "pet-clinic"]) => html_resp(200, PORTAL_HTML),
                (Method::Post, ["api", "triage"]) => handle_triage(request).await,
                _ => json_resp(404, json!({"error": "not_found"}).to_string()),
            }
        }
    }

    async fn handle_triage(request: Request) -> Result<Response, ErrorCode> {
        let bytes = match read_body(request).await {
            Ok(b) => b,
            Err(_) => return json_resp(400, json!({"error": "bad body"}).to_string()),
        };
        let body: Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                return json_resp(400, json!({"error": format!("bad json: {e}")}).to_string())
            }
        };

        let tenant = body["context"]["tenant"].as_str().unwrap_or("main-clinic").to_string();
        let subject = body["context"]["subject"].as_str().unwrap_or("anonymous").to_string();
        let exp_key = body["deliverable_id"].as_str().unwrap_or("vet-booking").to_string();

        let visit = triage::VisitContext {
            species: body["species"].as_str().unwrap_or("dog").to_string(),
            age_years: body["age_years"].as_u64().unwrap_or(3) as u32,
            minutes: body["minutes"].as_u64().unwrap_or(30) as u32,
            symptoms: body["symptoms"].as_str().unwrap_or("annual checkup").to_string(),
            owner_tier: body["owner_tier"].as_str().unwrap_or("standard").to_string(),
            tenant: tenant.clone(),
        };
        let decision = triage::evaluate(&visit);
        let triage_id = format!("trg_{}", ids::short_code(10));

        let _ = analytics::capture(&Event {
            name: "exposed".to_string(),
            distinct_id: subject,
            project: "vet-clinic".to_string(),
            timestamp_unix: system_clock::now().seconds as u64,
            properties: vec![
                Property { key: "experiment_key".into(), value: exp_key },
                Property { key: "variant_id".into(), value: decision.variant_id.clone() },
                Property { key: "tenant".into(), value: tenant },
            ],
        });

        json_resp(
            200,
            json!({
                "triage_id": triage_id,
                "currency": "USD",
                "fee_cents": decision.fee_cents,
                "priority": decision.priority,
                "recommended_vet": decision.recommended_vet,
                "recommendation": decision.recommendation,
                "variant_id": decision.variant_id,
            })
            .to_string(),
        )
    }

    fn json_resp(status: u16, body: String) -> Result<Response, ErrorCode> {
        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
        respond_with(status, headers, body.into_bytes())
    }

    fn html_resp(status: u16, body: &str) -> Result<Response, ErrorCode> {
        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"text/html; charset=utf-8".to_vec()]);
        respond_with(status, headers, body.as_bytes().to_vec())
    }

    bindings::export!(Component with_types_in bindings);
}
