//! assignment:router — stateless route assignment computation

#[allow(warnings)]
mod bindings;

use crate::bindings::exports::assignment::router::router::{AgentWorkload, Guest};

struct Component;

impl Guest for Component {
    fn route(agents: Vec<AgentWorkload>, strategy: String) -> Option<String> {
        if agents.is_empty() {
            return None;
        }

        match strategy.as_str() {
            "load-balanced" => {
                // Find the agent with the minimum open tickets
                agents
                    .iter()
                    .min_by_key(|a| a.open_tickets)
                    .map(|a| a.agent_id.clone())
            }
            "round-robin" | _ => {
                // Since this component is stateless, round-robin can be simulated by 
                // returning a random agent, or just the first one if we assume the caller
                // rotates the list. For a truly pure function, we assume the list is rotated
                // by the caller, or we just pick the first agent. We'll pick the first.
                // A better approach for round-robin is passing a cursor, but the interface
                // just takes the list.
                Some(agents[0].agent_id.clone())
            }
        }
    }
}

bindings::export!(Component with_types_in bindings);
