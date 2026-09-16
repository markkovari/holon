use crate::api::Reply;
use crate::bindings::records::store::store;
use crate::bindings::audit::log::recorder;
use crate::bindings::audit::log::types;
use crate::bindings::wasi::clocks::wall_clock;
use serde_json::{json, Value};

pub fn list() -> Reply {
    // 2. Fetch records
    let items = match store::list_records("ticket", 100, "") {
        Ok(page) => page.entries
            .iter()
            .map(|r| {
                let data: Value = serde_json::from_str(&r.data).unwrap_or(json!({}));
                json!({
                    "id": r.id,
                    "title": data["title"].as_str().unwrap_or(""),
                    "description": data["description"].as_str().unwrap_or(""),
                    "status": data["status"].as_str().unwrap_or("open"),
                    "replies": data["replies"].as_array().unwrap_or(&vec![]).clone(),
                    "created_at": r.created,
                    "updated_at": r.updated
                })
            })
            .collect::<Vec<_>>(),
        Err(_) => vec![],
    };
    
    // 3. Log read action
    let now = wall_clock::now();
    let _ = recorder::record_event(&types::Event {
        id: crate::bindings::id::generate::generator::ulid(),
        trace_id: "".to_string(),
        span_id: "".to_string(),
        timestamp: now.seconds,
        event: "tickets:read".to_string(),
        outcome: "allow".to_string(),
        tenant: "support".to_string(),
        subject: "agent".to_string(),
        detail: "list".to_string(),
    });
    
    Reply::json(200, &json!(items).to_string())
}

pub fn create(body: &str) -> Reply {

    let parsed: Value = match serde_json::from_str(body) {
        Ok(p) => p,
        Err(_) => return Reply::err(400, "invalid_json"),
    };

    let doc = json!({
        "title": parsed["title"].as_str().unwrap_or(""),
        "description": parsed["description"].as_str().unwrap_or(""),
        "status": "open",
        "replies": []
    });

    match store::create("ticket", &doc.to_string(), &[]) {
        Ok(entry) => {
            let now = wall_clock::now();
            let _ = recorder::record_event(&types::Event {
                id: crate::bindings::id::generate::generator::ulid(),
                trace_id: "".to_string(),
                span_id: "".to_string(),
                timestamp: now.seconds,
                event: "tickets:create".to_string(),
                outcome: "allow".to_string(),
                tenant: "support".to_string(),
                subject: "agent".to_string(),
                detail: "created".to_string(),
            });
            Reply::json(201, &json!({"id": entry.id}).to_string())
        }
        Err(e) => Reply::err(500, &format!("store error: {:?}", e)),
    }
}
