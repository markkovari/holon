//! A stateful actor representing a single helpdesk ticket's lifecycle.
#[allow(warnings)]
mod bindings;

use bindings::exports::actor::entity::handler::Guest;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TicketState {
    #[serde(rename = "ref")]
    pub id: String,
    pub subject: String,
    pub requester: String,
    pub assignee: String,
    pub priority: String,
    pub status: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type")]
pub enum TicketMessage {
    AddMessage { author: String, body: String, internal: bool },
    ChangeState { event: String },
    Assign { assignee: String },
}

use std::cell::RefCell;

thread_local! {
    static STATE: RefCell<Option<TicketState>> = RefCell::new(None);
}
struct Component;

impl Guest for Component {
    fn on_spawn(_id: String, state_bytes: Vec<u8>) -> Result<(), String> {
        let state: TicketState = serde_json::from_slice(&state_bytes)
            .map_err(|e| format!("failed to deserialize state: {}", e))?;
        STATE.with(|s| *s.borrow_mut() = Some(state));
        Ok(())
    }

    fn on_message(message_bytes: Vec<u8>) -> Result<(), String> {
        let msg: TicketMessage = serde_json::from_slice(&message_bytes)
            .map_err(|e| format!("failed to deserialize message: {}", e))?;
        
        STATE.with(|s| {
            let mut state_opt = s.borrow_mut();
            let state = state_opt.as_mut().ok_or_else(|| "actor not initialized".to_string())?;

            match msg {
                TicketMessage::AddMessage { author: _, body: _, internal: _ } => {
                    if state.status == "closed" {
                        return Err("ticket is closed".into());
                    }
                    // (In a real implementation, we'd emit an event here and maybe change state)
                }
                TicketMessage::ChangeState { event } => {
                    // Apply FSM transitions directly on the actor's state
                    let new_status = match (state.status.as_str(), event.as_str()) {
                        ("new", "triage") => "open",
                        ("open", "reply") => "pending",
                        ("pending", "requester-reply") => "open",
                        ("solved", "reopen") => "open",
                        ("new", "solve") | ("open", "solve") | ("pending", "solve") => "solved",
                        ("solved", "close") => "closed",
                        (s, e) => return Err(format!("invalid transition {} -> {}", s, e)),
                    };
                    state.status = new_status.to_string();
                }
                TicketMessage::Assign { assignee } => {
                    state.assignee = assignee;
                    if state.status == "new" {
                        state.status = "open".to_string();
                    }
                }
            }
            Ok(())
        })
    }

    fn on_snapshot() -> Vec<u8> {
        STATE.with(|s| {
            s.borrow()
                .as_ref()
                .map(|s| serde_json::to_vec(s).unwrap_or_default())
                .unwrap_or_default()
        })
    }
}

bindings::export!(Component with_types_in bindings);
