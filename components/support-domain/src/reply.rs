use crate::api::Reply;
use crate::bindings::audit::log::recorder;
use crate::bindings::audit::log::types;
use crate::bindings::id::generate::generator;
use crate::bindings::records::store::store;
use crate::bindings::wasi::clocks::wall_clock;
use crate::bindings::ai::inference::inference as ai;
use serde_json::{json, Value};

pub fn add_reply(id: &str, body: &str) -> Reply {

    let parsed: Value = match serde_json::from_str(body) {
        Ok(p) => p,
        Err(_) => return Reply::err(400, "invalid_json"),
    };

    let reply_text = parsed["text"].as_str().unwrap_or("");

    let record = match store::get("ticket", id) {
        Ok(r) => r,
        Err(_) => return Reply::err(404, "not_found"),
    };

    let mut data: Value = serde_json::from_str(&record.data).unwrap_or_else(|_| json!({}));
    
    if let Some(replies) = data["replies"].as_array_mut() {
        replies.push(json!({
            "text": reply_text,
            "author": "agent",
            "timestamp": wall_clock::now().seconds
        }));
    } else {
        data["replies"] = json!([{
            "text": reply_text,
            "author": "agent",
            "timestamp": wall_clock::now().seconds
        }]);
    }

    match store::update("ticket", id, &data.to_string(), record.revision) {
        Ok(_) => {
            log_action("tickets:reply", id, "added_reply");
            Reply::json(200, "{\"status\":\"ok\"}")
        }
        Err(e) => Reply::err(500, &format!("store error: {:?}", e)),
    }
}

pub fn suggest_reply(id: &str) -> Reply {

    let record = match store::get("ticket", id) {
        Ok(r) => r,
        Err(_) => return Reply::err(404, "not_found"),
    };

    let data: Value = serde_json::from_str(&record.data).unwrap_or_else(|_| json!({}));
    let description = data["description"].as_str().unwrap_or("");

    let sys_prompt = "You are a helpful customer support agent. Suggest a reply based on the description.";
    
    let res = match ai::generate(description, sys_prompt) {
        Ok(r) => r,
        Err(_) => return Reply::err(503, "model_unavailable"),
    };

    log_action("tickets:suggest", id, "generated_suggestion");
    Reply::json(200, &json!({"suggestion": res}).to_string())
}

pub fn close_ticket(id: &str) -> Reply {

    let record = match store::get("ticket", id) {
        Ok(r) => r,
        Err(_) => return Reply::err(404, "not_found"),
    };

    let mut data: Value = serde_json::from_str(&record.data).unwrap_or_else(|_| json!({}));
    data["status"] = json!("closed");

    match store::update("ticket", id, &data.to_string(), record.revision) {
        Ok(_) => {
            log_action("tickets:close", id, "closed_ticket");
            Reply::json(200, "{\"status\":\"ok\"}")
        }
        Err(e) => Reply::err(500, &format!("store error: {:?}", e)),
    }
}

fn log_action(action: &str, _resource: &str, detail: &str) {
    let now = wall_clock::now();
    let _ = recorder::record_event(&types::Event {
        id: crate::bindings::id::generate::generator::ulid(),
        trace_id: "".to_string(),
        span_id: "".to_string(),
        timestamp: now.seconds,
        event: action.to_string(),
        outcome: "allow".to_string(),
        tenant: "support".to_string(),
        subject: "agent".to_string(),
        detail: detail.to_string(),
    });
}
