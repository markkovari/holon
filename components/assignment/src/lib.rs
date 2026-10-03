//! assignment:router — stateless route assignment computation

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../event-bus/wit",
            "../../wit/deps/wasi-random-0.2.0",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "wit",
        ],
        world: "assignment:router/assignment-router",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use crate::bindings::event::bus::bus as eventbus;
use crate::bindings::exports::assignment::router::router::{AgentWorkload, Guest as RouterGuest};
use crate::bindings::p3::handler::Guest as HttpGuest;
use crate::bindings::p3::http::types::{ErrorCode, Request, Response};
use crate::bindings::records::store::store as records;
use serde_json::{json, Value};

guestio::guest_p3_respond!();

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
            // "round-robin", and anything unrecognised: the same fallback.
            _ => Some(agents[0].agent_id.clone()),
        }
    }
}

impl HttpGuest for Component {
    async fn handle(_request: Request) -> Result<Response, ErrorCode> {
        // Poll for events from the bus
        let events = match eventbus::poll("helpdesk.events", "assignment_worker", 50) {
            Ok(evs) => evs,
            Err(_) => {
                return emit(503, "eventbus error".into());
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
                        let mut data: Value =
                            serde_json::from_str(&entry.data).unwrap_or(Value::Null);

                        // 2. Perform mock routing
                        let mock_agents = vec![
                            AgentWorkload { agent_id: "agent-1".into(), open_tickets: 3 },
                            AgentWorkload { agent_id: "agent-2".into(), open_tickets: 1 },
                        ];
                        if let Some(assignee) =
                            Component::route(mock_agents, "load-balanced".into())
                        {
                            // 3. Update the ticket record
                            data["assignee"] = json!(assignee);
                            let _ = records::update(
                                "tickets",
                                ticket_id,
                                &data.to_string(),
                                entry.revision,
                            );

                            // 4. Emit TicketAssigned event
                            let assign_payload = json!({
                                "type": "ticket_assigned",
                                "ticket": ticket_id,
                                "assignee": assignee,
                                "tenant": payload["tenant"].as_str().unwrap_or(""),
                            });
                            let _ = eventbus::publish(
                                "helpdesk.events",
                                assign_payload.to_string().as_bytes(),
                            );
                        }
                    }
                }
            }

            ack_ids.push(ev.id);
        }

        if !ack_ids.is_empty() {
            let _ = eventbus::ack("helpdesk.events", "assignment_worker", &ack_ids);
        }

        emit(200, json!({ "processed": ack_ids.len() }).to_string())
    }
}

fn emit(status: u16, body: String) -> Result<Response, ErrorCode> {
    respond(status, "application/json", body)
}

bindings::export!(Component with_types_in bindings);
