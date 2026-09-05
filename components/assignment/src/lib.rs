//! assignment:router — stateless route assignment computation

#[allow(warnings)]
mod bindings;

use crate::bindings::exports::assignment::router::router::{AgentWorkload, Guest as RouterGuest};
use crate::bindings::exports::wasi::http::incoming_handler::Guest as HttpGuest;
use crate::bindings::wasi::http::types::{IncomingRequest, ResponseOutparam, OutgoingResponse, OutgoingBody, Fields};
use crate::bindings::event::bus::bus as eventbus;
use crate::bindings::records::store::store as records;
use serde::Deserialize;
use serde_json::{json, Value};

struct Component;

impl RouterGuest for Component {
    fn route(agents: Vec<AgentWorkload>, strategy: String) -> Option<String> {
        if agents.is_empty() {
            return None;
        }

        match strategy.as_str() {
            "load-balanced" => {
                agents.iter().min_by_key(|a| a.open_tickets).map(|a| a.agent_id.clone())
            }
            "round-robin" | _ => {
                Some(agents[0].agent_id.clone())
            }
        }
    }
}

impl HttpGuest for Component {
    fn handle(_request: IncomingRequest, response_out: ResponseOutparam) {
        // Poll for events from the bus
        let events = match eventbus::poll("helpdesk.events", "assignment_worker", 50) {
            Ok(evs) => evs,
            Err(_) => {
                emit(response_out, 503, "eventbus error".into());
                return;
            }
        };

        let mut ack_ids = Vec::new();

        for ev in events {
            let payload: Value = serde_json::from_slice(&ev.payload).unwrap_or(Value::Null);
            
            // Only handle TicketCreated events
            if payload["type"].as_str() == Some("ticket_created") {
                if let Some(ticket_id) = payload["ticket"].as_str() {
                    // 1. Fetch ticket to see if it needs assignment
                    if let Ok(entry) = records::get("tickets", ticket_id) {
                        let mut data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
                        
                        // 2. Perform mock routing
                        let mock_agents = vec![
                            AgentWorkload { agent_id: "agent-1".into(), open_tickets: 3 },
                            AgentWorkload { agent_id: "agent-2".into(), open_tickets: 1 },
                        ];
                        if let Some(assignee) = Component::route(mock_agents, "load-balanced".into()) {
                            // 3. Update the ticket record
                            data["assignee"] = json!(assignee);
                            let _ = records::update("tickets", ticket_id, &data.to_string(), entry.revision);
                            
                            // 4. Emit TicketAssigned event
                            let assign_payload = json!({
                                "type": "ticket_assigned",
                                "ticket": ticket_id,
                                "assignee": assignee,
                                "tenant": payload["tenant"].as_str().unwrap_or(""),
                            });
                            let _ = eventbus::publish("helpdesk.events", assign_payload.to_string().as_bytes());
                        }
                    }
                }
            }
            
            ack_ids.push(ev.id);
        }
        
        if !ack_ids.is_empty() {
            let _ = eventbus::ack("helpdesk.events", "assignment_worker", &ack_ids);
        }
        
        emit(response_out, 200, json!({ "processed": ack_ids.len() }).to_string());
    }
}

fn emit(out: ResponseOutparam, status: u16, body: String) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    let response = OutgoingResponse::new(headers);
    response.set_status_code(status).unwrap();
    let out_body = response.body().unwrap();
    ResponseOutparam::set(out, Ok(response));
    let stream = out_body.write().unwrap();
    stream.blocking_write_and_flush(body.as_bytes()).unwrap();
    drop(stream);
    OutgoingBody::finish(out_body, None).unwrap();
}

bindings::export!(Component with_types_in bindings);
